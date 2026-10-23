//! `POST /api/v1/issues/{fp}/reopen` and `/requeue` — owner actions
//! (docs/09 §8.5, §9.2).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::api::common::{
    issue_stuck_after_ms, parse_json_body, restart_lock, runner_intervals, schedule_restart,
    stuck_floor_ms, valid_systemd_unit,
};
use crate::api::ApiError;
use crate::ci;
use crate::serve::AppState;
use shanhaijing::{IssueRow, ShanError, Store};

/// The console's optional operator note. The reason is persisted in the
/// action's audit event and fed back into the next repair prompt.
#[derive(Debug, Default, Deserialize)]
pub struct ReopenRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct IgnoreRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RequeueRequest {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/v1/issues/{fp}/reopen` — escalated/rejected → new.
pub async fn reopen(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    payload: Result<Json<ReopenRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let reason = request.reason;
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        store
            .reopen_issue(&fingerprint, now, reason.as_deref())
            .map_err(|error| store_error("escalated/rejected/ignored", error))
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(error) => {
            eprintln!("reopen: blocking task failed: {error}");
            internal_error().into_response()
        }
    }
}

/// `POST /api/v1/issues/{fp}/ignore` — owner mute; new/escalated/rejected →
/// ignored. Reports are counted but never auto-reopen the issue.
pub async fn ignore(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    payload: Result<Json<IgnoreRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let reason = request.reason.unwrap_or_default();
    let now = crate::now_ms();
    let event_fingerprint = fingerprint.clone();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        store
            .ignore_issue(&fingerprint, &reason, "owner", now)
            .map_err(|error| store_error("new/escalated/rejected", error))
    })
    .await
    {
        Ok(Ok(())) => {
            crate::webhook::emit(crate::webhook::WebhookEvent::new(
                "ignored",
                Some(event_fingerprint),
                None,
                "ignored by owner",
            ));
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(error)) => error.into_response(),
        Err(error) => {
            eprintln!("ignore: blocking task failed: {error}");
            internal_error().into_response()
        }
    }
}

/// `POST /api/v1/issues/{fp}/requeue` — stuck triggered/fixing → new.
pub async fn requeue(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    payload: Result<Json<RequeueRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    // Accepted for the documented body, intentionally not persisted.
    let _reason = request.reason;
    let force = request.force;
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || requeue_issue(&store, &fingerprint, force, now)).await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(error) => {
            eprintln!("requeue: blocking task failed: {error}");
            internal_error().into_response()
        }
    }
}

/// GitHub reconciliation budget for the console action (docs/09 §9.2).
const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Process-wide guard: one GitHub reconcile at a time. `try_lock` turns a
/// concurrent POST into a 409 instead of queuing expensive GitHub calls.
pub(crate) fn sync_lock() -> &'static tokio::sync::Mutex<()> {
    static SYNC_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    SYNC_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// `POST /api/v1/sync` — reconcile every `pr_open` issue against GitHub
/// (docs/09 §9.2). Owner-triggered; the work runner keeps its own loop.
pub async fn sync(State(state): State<AppState>) -> Response {
    let Some(token) = github_token() else {
        return ApiError::upstream("NUWA_GITHUB_TOKEN (or GITHUB_TOKEN) is not set")
            .into_response();
    };
    let Ok(_guard) = sync_lock().try_lock() else {
        return ApiError::conflict("sync is already running").into_response();
    };
    let store = Arc::clone(&state.store);
    let now = crate::now_ms();
    match tokio::time::timeout(SYNC_TIMEOUT, ci::reconcile_all(&store, &token, now)).await {
        // Per-issue GitHub failures arrive as `error` lines inside this
        // summary (200 with `errors[]`), not as `Err` (docs/09 §9.2).
        Ok(Ok(lines)) => Json(ci::summarize_lines(&lines)).into_response(),
        // `reconcile_all` only errors on local store failures now: GitHub
        // unreachability is captured per issue. That is an internal failure,
        // not an upstream one.
        Ok(Err(error)) => {
            eprintln!("sync: local reconcile failure: {error:#}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "failed to reconcile PR state",
            )
            .into_response()
        }
        Err(_) => ApiError::upstream("sync timed out after 30s").into_response(),
    }
}

/// `NUWA_GITHUB_TOKEN` wins over `GITHUB_TOKEN` (the legacy console's order);
/// surrounding whitespace is trimmed, and a blank value counts as unset so a
/// `TOKEN=   ` line is a 502 instead of an opaque GitHub 401.
fn github_token() -> Option<String> {
    let read = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
    };
    read("NUWA_GITHUB_TOKEN").or_else(|| read("GITHUB_TOKEN"))
}

/// `POST /api/v1/runners/{id}/restart` — schedule a restart of the runner's
/// systemd unit (docs/09 §9.2). Shares the process-wide restart lock with
/// `POST /settings/restart`.
pub async fn restart_runner(
    State(state): State<AppState>,
    Path(runner_id): Path<String>,
) -> Response {
    let Ok(_restart) = restart_lock().try_lock() else {
        return ApiError::conflict("a restart is already being scheduled").into_response();
    };
    let store = Arc::clone(&state.store);
    // The guard stays in scope across the await: the lock is held until
    // scheduling (inside `restart_runner_now`) returns.
    match tokio::task::spawn_blocking(move || restart_runner_now(&store, &runner_id)).await {
        Ok(Ok(())) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(error) => {
            eprintln!("runner restart: blocking task failed: {error}");
            internal_error().into_response()
        }
    }
}

fn restart_runner_now(store: &Store, runner_id: &str) -> Result<(), ApiError> {
    let runner = store
        .list_runners()
        .map_err(|error| {
            eprintln!("runner restart: runner lookup failed: {error}");
            internal_error()
        })?
        .into_iter()
        .find(|runner| runner.id == runner_id)
        .ok_or_else(|| ApiError::not_found("runner not found"))?;
    // The stored unit is used verbatim: trimming must not turn an invalid
    // value into a unit systemd would accept.
    let unit = runner
        .service_unit
        .as_deref()
        .filter(|unit| !unit.is_empty())
        .ok_or_else(|| unit_error("runner has no service_unit; restart needs a systemd unit"))?;
    if !valid_systemd_unit(unit) {
        return Err(unit_error(
            "service_unit may only contain letters, digits and @ . _ : -",
        ));
    }
    if !schedule_restart(&[unit.to_string()]) {
        return Err(ApiError::not_supported(format!(
            "systemd-run is unavailable; restart the unit manually: systemctl restart {unit}"
        )));
    }
    Ok(())
}

fn unit_error(message: &str) -> ApiError {
    let mut fields = BTreeMap::new();
    fields.insert("service_unit".to_string(), message.to_string());
    ApiError::validation(fields)
}

fn requeue_issue(store: &Store, fingerprint: &str, force: bool, now: i64) -> Result<(), ApiError> {
    // The stuck window is sized like the overview/detail views: the owner
    // runner's configured interval plus the operator floor.
    let Some(issue) = find_issue(store, fingerprint)? else {
        return Err(ApiError::not_found("issue not found"));
    };
    let runners = store.list_runners().map_err(|error| {
        eprintln!("requeue: runner lookup failed: {error}");
        internal_error()
    })?;
    let stuck_after = issue_stuck_after_ms(&issue, &runner_intervals(&runners), stuck_floor_ms());
    store
        .requeue_stuck(fingerprint, force, now, stuck_after)
        .map_err(|error| store_error("triggered/fixing", error))
}

/// Store has no single-row issue projection yet; the single-owner issue table
/// is small and actions are rare (R25, matching the detail endpoint).
fn find_issue(store: &Store, fingerprint: &str) -> Result<Option<IssueRow>, ApiError> {
    store
        .list_issues()
        .map(|issues| {
            issues
                .into_iter()
                .find(|issue| issue.fingerprint == fingerprint)
        })
        .map_err(|error| {
            eprintln!("issue action: issue lookup failed: {error}");
            internal_error()
        })
}

/// Map Store action errors without leaking internals: unknown fingerprint is
/// 404, state guards are 409, everything else is a logged 500.
fn store_error(allowed: &'static str, error: ShanError) -> ApiError {
    match error {
        ShanError::UnknownIssue(_) => ApiError::not_found("issue not found"),
        ShanError::InvalidTransition { from, .. } => {
            let state = from.as_str();
            ApiError::invalid_state(format!("state is {state}, not {allowed}"))
        }
        ShanError::NotStuck(_) => {
            ApiError::invalid_state("issue is not stuck; retry with force=true")
        }
        other => {
            eprintln!("issue action: store failed: {other}");
            internal_error()
        }
    }
}

fn internal_error() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to apply issue action",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use shanhaijing::{IssueRow, RunUpdate, RunnerHeartbeat, RunnerUpsert, State, Store};
    use tower::ServiceExt;

    use crate::api::common::{restart_lock, restart_test_guard};
    use crate::serve::{build_app, AppState};

    const APP: &str = "example.com/actions";
    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

    // ---- app / request helpers --------------------------------------------

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("s.db");
        let store = Arc::new(Store::open(&db_path).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(None),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            env_path: dir.path().join("nuwa.env"),
            db_path,
            cookie_secure: false,
            cookie_secure_never: false,
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
        };
        (dir, state)
    }

    fn seeded_app() -> (tempfile::TempDir, Router, Arc<Store>, Seed) {
        let (dir, state) = test_state();
        let store = Arc::clone(&state.store);
        let seed = seed(&state.store);
        let app = build_app(state);
        (dir, app, store, seed)
    }

    fn set_cookie_value(response: &Response, name: &str) -> String {
        for value in response.headers().get_all(header::SET_COOKIE) {
            let raw = value.to_str().expect("ascii Set-Cookie");
            if let Some(rest) = raw.strip_prefix(&format!("{name}=")) {
                return rest.split(';').next().expect("cookie value").to_string();
            }
        }
        panic!("missing {name} Set-Cookie");
    }

    /// Local mode is implicitly authenticated, so `GET /auth/me` issues the
    /// readable CSRF cookie without a login round-trip.
    async fn csrf_token(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let token = set_cookie_value(&response, "nuwa_csrf");
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], true);
        token
    }

    async fn post_json(app: &Router, uri: &str, csrf: Option<&str>, body: &str) -> Response {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "localhost:7321")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = csrf {
            request = request
                .header(header::COOKIE, format!("nuwa_csrf={token}"))
                .header("x-nuwa-csrf", token);
        }
        app.clone()
            .oneshot(request.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response")
    }

    async fn put_json(app: &Router, uri: &str, csrf: Option<&str>, body: &str) -> Response {
        let mut request = Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::HOST, "localhost:7321")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = csrf {
            request = request
                .header(header::COOKIE, format!("nuwa_csrf={token}"))
                .header("x-nuwa-csrf", token);
        }
        app.clone()
            .oneshot(request.body(Body::from(body.to_string())).expect("request"))
            .await
            .expect("response")
    }

    async fn get(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn header_str(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
            .to_string()
    }

    fn assert_api_headers(response: &Response) {
        assert_eq!(header_str(response, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_str(response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert!(
            header_str(response, header::CONTENT_TYPE).starts_with("application/json"),
            "API responses must be JSON"
        );
    }

    fn set_session_dir(store: &Store, run_id: &str, dir: &std::path::Path) {
        store
            .update_run(
                run_id,
                &RunUpdate {
                    session_dir: Some(dir.display().to_string()),
                    ..Default::default()
                },
            )
            .expect("update run session dir");
    }

    fn write_session_jsonl(
        root: &std::path::Path,
        lines: &[serde_json::Value],
    ) -> std::path::PathBuf {
        let dir = root.join("--repo--");
        std::fs::create_dir_all(&dir).expect("session dir");
        let path = dir.join("2026-10-18T00-00-00-000Z_combined.jsonl");
        let mut body = String::new();
        for line in lines {
            body.push_str(&line.to_string());
            body.push('\n');
        }
        std::fs::write(&path, body).expect("write session jsonl");
        path
    }

    fn issue_row(store: &Store, fingerprint: &str) -> IssueRow {
        store
            .list_issues()
            .expect("list issues")
            .into_iter()
            .find(|issue| issue.fingerprint == fingerprint)
            .unwrap_or_else(|| panic!("issue {fingerprint} missing"))
    }

    // ---- seed helpers ------------------------------------------------------

    struct Seed {
        rejected: String,
        escalated: String,
        stale: String,
        slow: String,
        healthy: String,
    }

    fn signal(title: &str) -> shanhaijing::Signal {
        shanhaijing::Signal {
            repo: APP.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: "high".to_string(),
            message: Some(format!("boom {title}")),
            log_window: Vec::new(),
            stack_trace: format!(
                "panic: {title}\ngoroutine 1 [running]:\nexample.com/actions.F()\n\t/app/actions.go:1 +0x1"
            ),
        }
    }

    fn report(store: &Store, signal: &shanhaijing::Signal, at: i64) -> String {
        match store.report_signal(signal, at).expect("report signal") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{} must be a new issue, got {other:?}", signal.title),
        }
    }

    fn set_state(store: &Store, fingerprint: &str, state: State, at: i64) {
        store
            .set_state(fingerprint, state, at, None)
            .expect("set state");
    }

    fn upsert_runner(store: &Store, id: &str, heartbeat_at: i64, status: &str, config_json: &str) {
        store
            .upsert_runner(&RunnerUpsert {
                id: id.to_string(),
                repo: APP.to_string(),
                host: "test-host".to_string(),
                pid: None,
                started_at: heartbeat_at,
                heartbeat_at,
                status: status.to_string(),
                provider: None,
                model: None,
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: config_json.to_string(),
            })
            .expect("upsert runner");
    }

    fn heartbeat(store: &Store, runner_id: &str, at: i64, status: &str, fingerprint: &str) {
        store
            .heartbeat_runner(
                runner_id,
                &RunnerHeartbeat {
                    heartbeat_at: at,
                    status: status.to_string(),
                    current_fingerprint: Some(fingerprint.to_string()),
                    phase: Some("repair".to_string()),
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("heartbeat runner");
    }

    fn start_run(store: &Store, id: &str, fingerprint: &str, runner_id: &str, at: i64) {
        store
            .start_run(&shanhaijing::RunStart {
                id: id.to_string(),
                fingerprint: fingerprint.to_string(),
                runner_id: runner_id.to_string(),
                repo: APP.to_string(),
                started_at: at,
                provider: None,
                model: None,
            })
            .expect("start run");
    }

    fn upsert_runner_with_unit(store: &Store, id: &str, unit: Option<&str>) {
        let now = crate::now_ms();
        store
            .upsert_runner(&RunnerUpsert {
                id: id.to_string(),
                repo: APP.to_string(),
                host: "test-host".to_string(),
                pid: Some(1),
                started_at: now - 1_000,
                heartbeat_at: now,
                status: "idle".to_string(),
                provider: None,
                model: None,
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: unit.map(str::to_string),
                config_json: "{}".to_string(),
            })
            .expect("upsert runner");
    }

    /// Cargo runs tests in parallel and process env is global: the tests that
    /// fake `NUWA_GITHUB_TOKEN`/`GITHUB_TOKEN` serialize on this async lock,
    /// which may be held across the request await.
    async fn env_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    /// Restores saved env vars on drop, so a failed assertion cannot leak a
    /// fake token into another test.
    struct EnvRestore(Vec<(&'static str, Option<String>)>);

    impl EnvRestore {
        fn save(keys: &[&'static str]) -> Self {
            Self(
                keys.iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            )
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// Seeds the reopenable states, a stale owner-less fixing issue, a stale
    /// issue owned by a slow-polling idle runner, and a healthy owner.
    fn seed(store: &Store) -> Seed {
        let now = crate::now_ms();

        // rejected via a closed PR.
        let rejected = report(store, &signal("rejected action"), now - 60_000);
        set_state(store, &rejected, State::Triggered, now - 59_000);
        set_state(store, &rejected, State::Fixing, now - 58_000);
        set_state(store, &rejected, State::PrOpen, now - 57_000);
        set_state(store, &rejected, State::Rejected, now - 56_000);

        // escalated with an owner to prove reopen clears it.
        let escalated = report(store, &signal("escalated action"), now - 50_000);
        set_state(store, &escalated, State::Triggered, now - 49_000);
        set_state(store, &escalated, State::Fixing, now - 48_000);
        start_run(
            store,
            "run-escalated",
            &escalated,
            "runner-gone",
            now - 48_000,
        );
        set_state(store, &escalated, State::Escalated, now - 47_000);

        // Same 600s of silence as `slow`, but no owner and the default 30s
        // interval → a 300s stuck window → stale.
        let stale = report(store, &signal("stale fixing"), now - 700_000);
        set_state(store, &stale, State::Triggered, now - 600_000);
        set_state(store, &stale, State::Fixing, now - 600_000);

        // A 600s poll interval widens the window to 1800s, so 600s of
        // silence is not stuck even though the owner is idle.
        let slow = report(store, &signal("slow fixing"), now - 700_000);
        set_state(store, &slow, State::Triggered, now - 600_000);
        set_state(store, &slow, State::Fixing, now - 600_000);
        start_run(store, "run-slow", &slow, "runner-slow", now - 600_000);
        upsert_runner(
            store,
            "runner-slow",
            now - 600_000,
            "working",
            r#"{"interval_secs":600}"#,
        );
        heartbeat(store, "runner-slow", now - 600_000, "idle", &slow);

        // State change is old enough to look stuck, but the fresh working
        // heartbeat with this fingerprint marks the owner healthy.
        let healthy = report(store, &signal("healthy owner"), now - 700_000);
        set_state(store, &healthy, State::Triggered, now - 600_000);
        set_state(store, &healthy, State::Fixing, now - 600_000);
        start_run(
            store,
            "run-healthy",
            &healthy,
            "runner-healthy",
            now - 600_000,
        );
        upsert_runner(store, "runner-healthy", now - 1_000, "working", "{}");
        heartbeat(store, "runner-healthy", now - 1_000, "working", &healthy);

        Seed {
            rejected,
            escalated,
            stale,
            slow,
            healthy,
        }
    }

    // ---- CSRF --------------------------------------------------------------

    #[tokio::test]
    async fn actions_require_csrf_even_in_local_mode() {
        let (_dir, app, store, seed) = seeded_app();

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            None,
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.stale),
            None,
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        assert_eq!(issue_row(&store, &seed.rejected).state, "rejected");
        assert_eq!(issue_row(&store, &seed.stale).state, "fixing");
    }

    // ---- reopen ------------------------------------------------------------

    #[tokio::test]
    async fn reopen_returns_rejected_and_escalated_to_new() {
        let (_dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            Some(&csrf),
            r#"{"reason":"reviewed by owner"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(issue_row(&store, &seed.rejected).owner_runner_id.is_none());
        assert_eq!(issue_row(&store, &seed.rejected).state, "new");
        let event = store
            .latest_event(&seed.rejected)
            .expect("latest event")
            .expect("event");
        assert_eq!(event.kind, "state_changed");
        assert_eq!(event.detail, "reopened by owner: reviewed by owner");
        assert_eq!(event.actor.as_deref(), Some("owner"));

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.escalated),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let row = issue_row(&store, &seed.escalated);
        assert_eq!(row.state, "new");
        assert!(
            row.owner_runner_id.is_none(),
            "reopen must clear the owner: {row:?}"
        );

        // A second reopen targets `new`, which is not reopenable.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.escalated),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "invalid_state");
        let message = value["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("escalated/rejected"),
            "message must name the allowed states: {message}"
        );
    }

    #[tokio::test]
    async fn ignore_mutes_and_reopen_restores() {
        let (_dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/ignore", seed.rejected),
            Some(&csrf),
            r#"{"reason":"known upstream noise"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let row = issue_row(&store, &seed.rejected);
        assert_eq!(row.state, "ignored");
        assert_eq!(row.attempts, 0);
        let event = store
            .latest_event(&seed.rejected)
            .expect("event")
            .expect("event");
        assert!(
            event.detail.contains("known upstream noise"),
            "{:?}",
            event.detail
        );

        // Ignored reports are muted, but the owner can reopen explicitly.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            Some(&csrf),
            r#"{"reason":"deploy fixed the root cause"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(issue_row(&store, &seed.rejected).state, "new");
    }

    // ---- requeue -----------------------------------------------------------

    #[tokio::test]
    async fn requeue_moves_a_stale_issue_to_new() {
        let (_dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.stale),
            Some(&csrf),
            r#"{"reason":"runner died"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let row = issue_row(&store, &seed.stale);
        assert_eq!(row.state, "new");
        assert!(row.owner_runner_id.is_none());
        let event = store
            .latest_event(&seed.stale)
            .expect("latest event")
            .expect("event");
        assert_eq!(event.kind, "requeued");
        assert_eq!(event.detail, "stuck requeued by owner");
        assert_eq!(event.actor.as_deref(), Some("owner"));

        // Requeueing the now-`new` issue is rejected.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.stale),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "invalid_state");
    }

    #[tokio::test]
    async fn requeue_uses_the_owner_runner_interval() {
        let (_dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        // 600s of silence: stuck for a default 30s owner (300s floor), not
        // for a 600s owner (1800s window).
        assert_eq!(issue_row(&store, &seed.slow).state, "fixing");
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.slow),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "invalid_state");
        assert_eq!(issue_row(&store, &seed.slow).state, "fixing");

        // force skips the stuck check with the same computed threshold.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.slow),
            Some(&csrf),
            r#"{"force":true}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(issue_row(&store, &seed.slow).state, "new");
    }

    #[tokio::test]
    async fn requeue_requires_force_for_a_healthy_owner() {
        let (_dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.healthy),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "invalid_state");
        let message = value["error"]["message"].as_str().expect("message");
        assert!(
            message.contains("not stuck"),
            "healthy owner rejection must mention stuck: {message}"
        );
        assert_eq!(issue_row(&store, &seed.healthy).state, "fixing");

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.healthy),
            Some(&csrf),
            r#"{"force":true}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let row = issue_row(&store, &seed.healthy);
        assert_eq!(row.state, "new");
        assert!(row.owner_runner_id.is_none());
    }

    // ---- errors ------------------------------------------------------------

    #[tokio::test]
    async fn unknown_fingerprints_are_404() {
        let (_dir, app, _store, _seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        for (action, body) in [
            ("reopen", "{}"),
            ("requeue", "{}"),
            ("requeue", r#"{"force":true}"#),
        ] {
            let response = post_json(
                &app,
                &format!("/api/v1/issues/does-not-exist/{action}"),
                Some(&csrf),
                body,
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{action} {body}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "not_found");
            assert_eq!(value["error"]["message"], "issue not found");
        }
    }

    #[tokio::test]
    async fn malformed_bodies_map_to_400_and_422() {
        let (_dir, app, _store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        // Syntactically invalid JSON is a 400.
        for action in ["reopen", "requeue"] {
            let response = post_json(
                &app,
                &format!("/api/v1/issues/{}/{action}", seed.rejected),
                Some(&csrf),
                r#"{"reason":"#,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{action}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "invalid_request");
        }

        // A well-formed body with a mistyped field is a 422 with a field map.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.rejected),
            Some(&csrf),
            r#"{"force":"yes"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["body"].is_string());

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            Some(&csrf),
            r#"{"reason":7}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
    }

    // ---- sync --------------------------------------------------------------

    #[tokio::test]
    async fn sync_and_restart_require_csrf() {
        let (_dir, state) = test_state();
        let app = build_app(state);

        let response = post_json(&app, "/api/v1/sync", None, "").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        let response = post_json(&app, "/api/v1/runners/does-not-exist/restart", None, "").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn sync_without_a_github_token_is_502() {
        let (_dir, state) = test_state();
        let app = build_app(state);
        let csrf = csrf_token(&app).await;

        let _env = env_guard().await;
        let _restore = EnvRestore::save(&["NUWA_GITHUB_TOKEN", "GITHUB_TOKEN"]);
        for (nuwa, legacy) in [(None, None), (Some(" \t"), Some("  "))] {
            match nuwa {
                Some(token) => std::env::set_var("NUWA_GITHUB_TOKEN", token),
                None => std::env::remove_var("NUWA_GITHUB_TOKEN"),
            }
            match legacy {
                Some(token) => std::env::set_var("GITHUB_TOKEN", token),
                None => std::env::remove_var("GITHUB_TOKEN"),
            }

            let response = post_json(&app, "/api/v1/sync", Some(&csrf), "").await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "{nuwa:?} {legacy:?}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "upstream_error");
            assert!(
                value["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("NUWA_GITHUB_TOKEN")),
                "message must name the missing variable: {value}"
            );
        }
    }

    // Holding the process-wide lock proves the 409 path without any GitHub
    // call: the handler must fail on `try_lock` before reconciling.
    #[tokio::test]
    async fn sync_rejects_a_concurrent_run_with_409() {
        let (_dir, state) = test_state();
        let app = build_app(state);
        let csrf = csrf_token(&app).await;

        let _env = env_guard().await;
        let _restore = EnvRestore::save(&["NUWA_GITHUB_TOKEN"]);
        std::env::set_var("NUWA_GITHUB_TOKEN", "test-token-never-used");
        let held = super::sync_lock().try_lock().expect("sync lock free");
        let response = post_json(&app, "/api/v1/sync", Some(&csrf), "").await;
        drop(held);

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "conflict");
    }

    // ---- runner restart ----------------------------------------------------

    #[test]
    fn service_unit_whitelist_accepts_systemd_names_only() {
        for unit in ["nuwa-work", "nuwa-work@1.service", "nuwa-serve", "a.b_c:d"] {
            assert!(crate::api::common::valid_systemd_unit(unit), "{unit}");
        }
        for unit in [
            "",
            "nuwa work",
            "nuwa/work",
            "nuwa$(id)",
            "nuwa-work;rm",
            "nuwa-work\n",
            // `systemctl` would parse a leading `-` as an option.
            "-nuwa-work",
            "--now",
        ] {
            assert!(!crate::api::common::valid_systemd_unit(unit), "{unit:?}");
        }
    }

    #[tokio::test]
    async fn restart_unknown_runner_is_404() {
        let _serial = restart_test_guard().await;
        let (_dir, state) = test_state();
        let app = build_app(state);
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            "/api/v1/runners/does-not-exist/restart",
            Some(&csrf),
            "",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["message"], "runner not found");
    }

    #[tokio::test]
    async fn restart_without_a_service_unit_is_422() {
        let _serial = restart_test_guard().await;
        for unit in [None, Some("")] {
            let (_dir, state) = test_state();
            upsert_runner_with_unit(&state.store, "runner-plain", unit);
            let app = build_app(state);
            let csrf = csrf_token(&app).await;

            let response = post_json(
                &app,
                "/api/v1/runners/runner-plain/restart",
                Some(&csrf),
                "",
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{unit:?}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"]["service_unit"].is_string(),
                "missing unit must name the field: {value}"
            );
        }
    }

    #[tokio::test]
    async fn restart_with_an_invalid_service_unit_is_422() {
        let _serial = restart_test_guard().await;
        // None of these can become a valid unit after trimming, so the
        // systemd-run path is never reached.
        for unit in [
            "nuwa work",
            "nuwa-work; rm -rf /",
            "../etc/passwd",
            "-nuwa-work",
        ] {
            let (_dir, state) = test_state();
            upsert_runner_with_unit(&state.store, "runner-odd", Some(unit));
            let app = build_app(state);
            let csrf = csrf_token(&app).await;

            let response =
                post_json(&app, "/api/v1/runners/runner-odd/restart", Some(&csrf), "").await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{unit:?}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"]["service_unit"].is_string(),
                "invalid unit must name the field: {value}"
            );
        }
    }

    /// One process-wide mutex protects both restart endpoints: holding it
    /// turns either POST into a 409 before any audit row or subprocess.
    #[tokio::test]
    async fn restart_lock_is_shared_by_both_endpoints() {
        let _serial = restart_test_guard().await;
        let (_dir, state) = test_state();
        upsert_runner_with_unit(&state.store, "runner-locked", Some("nuwa-work"));
        let store = Arc::clone(&state.store);
        let app = build_app(state);
        let csrf = csrf_token(&app).await;

        let held = restart_lock().try_lock().expect("restart lock free");
        for uri in [
            "/api/v1/runners/runner-locked/restart",
            "/api/v1/settings/restart",
        ] {
            let response = post_json(&app, uri, Some(&csrf), "{}").await;
            assert_eq!(response.status(), StatusCode::CONFLICT, "{uri}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "conflict", "{uri}");
        }
        assert!(
            store
                .events_for("__system__", 10)
                .expect("events")
                .is_empty(),
            "a locked-out restart must not audit or schedule"
        );
        drop(held);
    }

    // ---- M2c Task 6: combined actions/settings/system/transcript walk ----

    /// One local-mode app over a temp DB/env/session tree: owner actions with
    /// CSRF, sync without a token, settings read/write/test/restart guard,
    /// system facts, and a parsed transcript with pagination and 404s.
    #[tokio::test]
    async fn m2c_combined_actions_settings_system_and_transcript() {
        let (dir, app, store, seed) = seeded_app();
        let csrf = csrf_token(&app).await;

        // ---- CSRF is mandatory even in local mode ----
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            None,
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");
        assert_eq!(issue_row(&store, &seed.rejected).state, "rejected");

        // ---- reopen / requeue with CSRF ----
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.rejected),
            Some(&csrf),
            r#"{"reason":"combined reopen"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(issue_row(&store, &seed.rejected).state, "new");
        let event = store
            .latest_event(&seed.rejected)
            .expect("event query")
            .expect("event");
        assert_eq!(event.kind, "state_changed");
        assert_eq!(event.detail, "reopened by owner: combined reopen");
        assert_eq!(event.actor.as_deref(), Some("owner"));

        // Escalated issues reopen too, and the owner is cleared.
        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/reopen", seed.escalated),
            Some(&csrf),
            "{}",
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let escalated = issue_row(&store, &seed.escalated);
        assert_eq!(escalated.state, "new");
        assert!(escalated.owner_runner_id.is_none());

        let response = post_json(
            &app,
            &format!("/api/v1/issues/{}/requeue", seed.stale),
            Some(&csrf),
            r#"{"reason":"combined requeue"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(issue_row(&store, &seed.stale).state, "new");
        let event = store
            .latest_event(&seed.stale)
            .expect("event query")
            .expect("event");
        assert_eq!(event.kind, "requeued");
        assert_eq!(event.detail, "stuck requeued by owner");

        // A read carries no-store + the strict API CSP.
        let response = get(&app, &format!("/api/v1/issues/{}", seed.rejected)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let detail = json_body(response).await;
        assert_eq!(detail["issue"]["state"], "new");

        // ---- sync without a GitHub token is an upstream 502, no network ----
        {
            let _env = env_guard().await;
            let _restore = EnvRestore::save(&["NUWA_GITHUB_TOKEN", "GITHUB_TOKEN"]);
            std::env::remove_var("NUWA_GITHUB_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
            let response = post_json(&app, "/api/v1/sync", Some(&csrf), "").await;
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "upstream_error");
            assert!(
                value["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("NUWA_GITHUB_TOKEN")),
                "{value}"
            );
        }

        // ---- settings: masked read, validated write, audit ----
        let env_path = dir.path().join("nuwa.env");
        let original_env = "# combined fixture\n\
                            NUWA_GITHUB_TOKEN=ghp_combinedsecret1234\n\
                            NUWA_INTERVAL=30\n\
                            LEGACY_KEY=keep-me\n";
        std::fs::write(&env_path, original_env).expect("write env");

        let response = get(&app, "/api/v1/settings").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let settings = json_body(response).await;
        assert_eq!(settings["env_path"], env_path.display().to_string());
        assert_eq!(settings["env_exists"], true);
        let token = &settings["values"]["NUWA_GITHUB_TOKEN"];
        assert_eq!(token["set"], true);
        assert_eq!(token["last4"], "1234");
        assert!(
            token.get("value").is_none(),
            "secrets must not echo: {token}"
        );
        assert_eq!(settings["values"]["NUWA_INTERVAL"]["value"], "30");
        assert_eq!(settings["values"]["NUWA_INTERVAL"]["source"], "env");
        assert_eq!(settings["unknown_keys"], serde_json::json!(["LEGACY_KEY"]));
        assert!(
            !settings.to_string().contains("ghp_combinedsecret1234"),
            "secret value leaked into the response"
        );

        let response = put_json(
            &app,
            "/api/v1/settings",
            Some(&csrf),
            r#"{"values":{"NUWA_INTERVAL":"60"}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["changed"], serde_json::json!(["NUWA_INTERVAL"]));
        assert_eq!(value["restart_required"], true);
        let text = std::fs::read_to_string(&env_path).expect("read env");
        assert!(text.contains("NUWA_INTERVAL=60"), "{text}");
        assert!(
            text.contains("NUWA_GITHUB_TOKEN=ghp_combinedsecret1234"),
            "an untouched secret must survive the write: {text}"
        );
        assert!(text.contains("# combined fixture"), "{text}");
        let events = store.events_for("__config__", 10).expect("config events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "config_changed");
        assert_eq!(events[0].detail, "NUWA_INTERVAL");
        assert_eq!(events[0].actor.as_deref(), Some("owner"));

        let response = get(&app, "/api/v1/settings").await;
        let after = json_body(response).await;
        assert_eq!(after["values"]["NUWA_INTERVAL"]["value"], "60");
        assert_eq!(after["values"]["NUWA_INTERVAL"]["source"], "env");
        assert_eq!(
            after["last_changes"][0]["key"], "NUWA_INTERVAL",
            "the write must be audited"
        );

        // ---- settings test: invalid target / empty key are 422, offline ----
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"bogus"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["target"].is_string(), "{value}");

        for body in [
            r#"{"target":"github","values":{"NUWA_GITHUB_TOKEN":""}}"#,
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"zai","ZAI_API_KEY":""}}"#,
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"anthropic","ANTHROPIC_API_KEY":""}}"#,
        ] {
            let response = post_json(&app, "/api/v1/settings/test", Some(&csrf), body).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{body}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed", "{body}");
        }

        // faux is the offline provider: a successful probe needs no network.
        let response = post_json(
            &app,
            "/api/v1/settings/test",
            Some(&csrf),
            r#"{"target":"llm","values":{"NUWA_PROVIDER":"faux"}}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["ok"], true);

        // ---- restart guard: open runs need force ----
        {
            // The process-wide restart lock is shared across tests, so the
            // guard must not race the test that deliberately holds it.
            let _serial = restart_test_guard().await;
            let response = post_json(
                &app,
                "/api/v1/settings/restart",
                Some(&csrf),
                r#"{"force":false}"#,
            )
            .await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "conflict");
            let active = value["active_runs"].as_array().expect("active runs");
            assert!(!active.is_empty(), "the seed leaves runs open");
            assert!(active.iter().all(|run| run["id"].is_string()), "{value}");
            assert!(
                store
                    .events_for("__system__", 10)
                    .expect("events")
                    .is_empty(),
                "a guarded restart must not schedule or audit"
            );
        }

        // ---- system facts ----
        let response = get(&app, "/api/v1/system").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let system = json_body(response).await;
        assert_eq!(system["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(system["local_mode"], true);
        assert!(system["uptime_secs"].is_u64());
        assert!(
            system["db_size_bytes"]
                .as_u64()
                .is_some_and(|size| size > 0),
            "the temp DB has a size: {system}"
        );
        let units: Vec<&str> = system["services"]
            .as_array()
            .expect("services")
            .iter()
            .map(|service| service["unit"].as_str().expect("unit"))
            .collect();
        assert_eq!(units, vec!["nuwa-serve", "nuwa-work"]);

        // ---- transcript: blocks, pagination, 404 ----
        let transcript_fp = report(&store, &signal("combined transcript"), crate::now_ms());
        start_run(
            &store,
            "run-combined-transcript",
            &transcript_fp,
            "runner-combined",
            crate::now_ms(),
        );
        let session_root = dir.path().join("sessions").join("run-combined-transcript");
        write_session_jsonl(
            &session_root,
            &[
                serde_json::json!({
                    "kind": "entry", "type": "message", "seq": 1, "timestamp": 1_000,
                    "message": { "kind": "user", "role": "user", "content": "fix the bug" }
                }),
                serde_json::json!({
                    "kind": "entry", "type": "message", "seq": 2, "timestamp": 2_000,
                    "message": {
                        "kind": "assistant", "role": "assistant",
                        "content": [
                            { "type": "thinking", "thinking": "read the code" },
                            { "type": "text", "text": "I will read calc.go" },
                            { "type": "toolCall", "name": "read",
                              "arguments": { "path": "calc.go" } }
                        ]
                    }
                }),
                serde_json::json!({
                    "kind": "entry", "type": "message", "seq": 3, "timestamp": 3_000,
                    "message": {
                        "kind": "tool_result", "role": "toolResult", "toolName": "read",
                        "content": [{ "type": "text", "text": "package main" }],
                        "isError": false
                    }
                }),
            ],
        );
        set_session_dir(&store, "run-combined-transcript", &session_root);

        let base = format!("/api/v1/issues/{transcript_fp}/transcript?page_size=1");
        let response = get(&app, &format!("{base}&page=1")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let page = json_body(response).await;
        assert_eq!(
            page["meta"],
            serde_json::json!({
                "page": 1, "page_size": 1, "total": 3, "truncated": false, "empty": false
            })
        );
        assert_eq!(page["data"][0]["seq"], 1);
        assert_eq!(page["data"][0]["role"], "user");
        assert_eq!(
            page["data"][0]["blocks"],
            serde_json::json!([{ "type": "text", "text": "fix the bug" }])
        );

        let response = get(&app, &format!("{base}&page=2")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = json_body(response).await;
        assert_eq!(page["data"][0]["seq"], 2);
        let blocks = page["data"][0]["blocks"].as_array().expect("blocks");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[2]["type"], "tool_call");
        assert_eq!(blocks[2]["arguments"]["path"], "calc.go");

        let response = get(&app, &format!("{base}&page=3")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = json_body(response).await;
        assert_eq!(page["data"][0]["seq"], 3);
        assert_eq!(page["data"][0]["role"], "tool");
        assert_eq!(page["data"][0]["blocks"][0]["is_error"], false);

        for (query, message) in [
            ("run_id=does-not-exist", "run not found"),
            // A run of another fingerprint is not addressable here.
            ("run_id=run-slow", "run not found"),
        ] {
            let response = get(
                &app,
                &format!("/api/v1/issues/{transcript_fp}/transcript?{query}"),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{query}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["message"], message, "{query}");
        }
        let response = get(&app, "/api/v1/issues/does-not-exist/transcript").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["message"], "issue not found");
    }
}
