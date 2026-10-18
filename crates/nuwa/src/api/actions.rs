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
    issue_stuck_after_ms, parse_json_body, runner_intervals, schedule_restart, stuck_floor_ms,
    valid_systemd_unit,
};
use crate::api::ApiError;
use crate::ci;
use crate::serve::AppState;
use shanhaijing::{IssueRow, ShanError, Store};

/// The console's optional operator note. The Store methods journal the action
/// with a fixed detail (`reopened by owner` / `stuck requeued by owner`), so
/// persisting the reason separately would duplicate that audit row: the field
/// is accepted and validated as part of the body contract but not stored.
#[derive(Debug, Default, Deserialize)]
pub struct ReopenRequest {
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
    // Accepted for the documented body, intentionally not persisted.
    let _reason = request.reason;
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        store
            .reopen_issue(&fingerprint, now)
            .map_err(|error| store_error("escalated/rejected", error))
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
fn sync_lock() -> &'static tokio::sync::Mutex<()> {
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
        Ok(Ok(lines)) => Json(ci::summarize_lines(&lines)).into_response(),
        Ok(Err(error)) => {
            eprintln!("sync: reconcile failed: {error:#}");
            ApiError::upstream("GitHub PR reconciliation failed").into_response()
        }
        Err(_) => ApiError::upstream("sync timed out after 30s").into_response(),
    }
}

/// `NUWA_GITHUB_TOKEN` wins over `GITHUB_TOKEN` (the legacy console's order);
/// a blank value counts as unset.
fn github_token() -> Option<String> {
    let read = |key: &str| std::env::var(key).ok().filter(|token| !token.is_empty());
    read("NUWA_GITHUB_TOKEN").or_else(|| read("GITHUB_TOKEN"))
}

/// `POST /api/v1/runners/{id}/restart` — schedule a restart of the runner's
/// systemd unit (docs/09 §9.2).
pub async fn restart_runner(
    State(state): State<AppState>,
    Path(runner_id): Path<String>,
) -> Response {
    let store = Arc::clone(&state.store);
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
        return Err(ApiError::not_supported(
            "systemd-run is unavailable; restart the unit manually",
        ));
    }
    Ok(())
}

fn unit_error(message: &str) -> ApiError {
    let mut fields = BTreeMap::new();
    fields.insert("service_unit".to_string(), message.to_string());
    ApiError::validation(fields)
}

fn requeue_issue(
    store: &Store,
    fingerprint: &str,
    force: bool,
    now: i64,
) -> Result<(), ApiError> {
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
    use shanhaijing::{IssueRow, RunnerHeartbeat, RunnerUpsert, State, Store};
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    const APP: &str = "example.com/actions";

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

    async fn post_json(
        app: &Router,
        uri: &str,
        csrf: Option<&str>,
        body: &str,
    ) -> Response {
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
            .oneshot(
                request
                    .body(Body::from(body.to_string()))
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

    fn upsert_runner(
        store: &Store,
        id: &str,
        heartbeat_at: i64,
        status: &str,
        config_json: &str,
    ) {
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

    fn heartbeat(
        store: &Store,
        runner_id: &str,
        at: i64,
        status: &str,
        fingerprint: &str,
    ) {
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
        static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> =
            std::sync::OnceLock::new();
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
        start_run(store, "run-escalated", &escalated, "runner-gone", now - 48_000);
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
        start_run(store, "run-healthy", &healthy, "runner-healthy", now - 600_000);
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
        assert_eq!(event.detail, "reopened by owner");
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
            "message must name the missing variable: {value}"
        );
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
        ] {
            assert!(!crate::api::common::valid_systemd_unit(unit), "{unit:?}");
        }
    }

    #[tokio::test]
    async fn restart_unknown_runner_is_404() {
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
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{unit:?}");
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
        // None of these can become a valid unit after trimming, so the
        // systemd-run path is never reached.
        for unit in ["nuwa work", "nuwa-work; rm -rf /", "../etc/passwd"] {
            let (_dir, state) = test_state();
            upsert_runner_with_unit(&state.store, "runner-odd", Some(unit));
            let app = build_app(state);
            let csrf = csrf_token(&app).await;

            let response = post_json(
                &app,
                "/api/v1/runners/runner-odd/restart",
                Some(&csrf),
                "",
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{unit:?}");
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"]["service_unit"].is_string(),
                "invalid unit must name the field: {value}"
            );
        }
    }
}
