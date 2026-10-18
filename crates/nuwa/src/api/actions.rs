//! `POST /api/v1/issues/{fp}/reopen` and `/requeue` — owner actions
//! (docs/09 §8.5, §9.2).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::api::common::{issue_stuck_after_ms, runner_intervals, stuck_floor_ms};
use crate::api::ApiError;
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
    let request = match parse_body(payload) {
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
    let request = match parse_body(payload) {
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

fn parse_body<T: DeserializeOwned>(
    payload: Result<Json<T>, JsonRejection>,
) -> Result<T, ApiError> {
    match payload {
        Ok(Json(body)) => Ok(body),
        Err(rejection) => Err(rejection_error(rejection)),
    }
}

/// Keep extractor failures inside the JSON error contract: syntax errors stay
/// 400, well-formed bodies with wrong field types become 422 field errors.
fn rejection_error(rejection: JsonRejection) -> ApiError {
    let status = rejection.status();
    let message = rejection.body_text();
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            ApiError::new(status, "unsupported_media_type", message)
        }
        StatusCode::UNPROCESSABLE_ENTITY => {
            let mut fields = BTreeMap::new();
            fields.insert("body".to_string(), message);
            ApiError::validation(fields)
        }
        _ => ApiError::invalid_request(message),
    }
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
        let store = Arc::new(Store::open(dir.path().join("s.db")).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(None),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            cookie_secure: false,
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
}
