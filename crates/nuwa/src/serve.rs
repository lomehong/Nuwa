//! `nuwa serve` — single-port HTTP surface: legacy console, `/api/v1`,
//! ingest and the static fallback. Extracted from `main.rs` in M2a so the
//! router can be assembled and route-tested without binding a socket.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::{now_ms, parse_signals, report_one, Shared};

/// State shared by the legacy console routes and the `/api/v1` surface.
/// `session`, `limiter`, `started_at_ms` and `cookie_secure` are consumed by
/// the Task 4 auth routes/middleware; `build_app` already wires the legacy
/// routes from `store`/`ui_token`/`ingest_token`.
//
// Temporary: those four fields are constructed but not read until Task 4, so
// the bin target would fail the zero-warning gate. Remove this allow when the
// login/logout/me routes and session middleware land.
#[allow(dead_code)]
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<shanhaijing::Store>,
    /// Effective console token: `NUWA_UI_TOKEN`, else the ingest token.
    pub ui_token: Arc<Option<String>>,
    pub ingest_token: Arc<Option<String>>,
    pub session: Arc<crate::session::SessionManager>,
    pub limiter: Arc<crate::session::LoginLimiter>,
    pub started_at_ms: i64,
    pub cookie_secure: bool,
}

/// Full app assembly: legacy console + `/api/v1` + static fallback.
/// Task 5 layers the security headers on top of this router.
pub fn build_app(state: AppState) -> Router {
    let legacy_state: Shared = (Arc::clone(&state.store), Arc::clone(&state.ui_token));
    let ingest_auth = axum::middleware::from_fn_with_state(
        (Arc::clone(&state.store), Arc::clone(&state.ingest_token)),
        check_auth,
    );
    let ui_auth = axum::middleware::from_fn_with_state(
        (Arc::clone(&state.store), Arc::clone(&state.ui_token)),
        crate::ui::ui_auth,
    );
    // UI/settings only. `/health` and `/ingest` keep their own gates — a
    // route_layer over all legacy routes would put Basic auth in front of
    // `/health` (and swallow the ingest token check behind it).
    let legacy_ui = Router::new()
        .route("/ui", get(crate::ui::dashboard))
        .route("/ui/reopen", post(crate::ui::reopen))
        .route("/ui/sync", post(crate::ui::sync_now))
        .route("/issue/{fp}", get(crate::ui::issue_detail))
        .route("/settings", get(crate::settings::get_settings))
        .route("/settings/save", post(crate::settings::save_settings))
        .route("/settings/restart", post(crate::settings::restart_services))
        .route_layer(ui_auth)
        .route_layer(axum::middleware::from_fn(crate::ui::origin_guard));
    let legacy = Router::new()
        .route("/ingest", post(ingest_endpoint).route_layer(ingest_auth))
        .route("/health", get(|| async { "nuwa serve ok" }))
        .merge(legacy_ui)
        .with_state(legacy_state);

    let api = crate::api::router(Arc::new(state.clone()));
    Router::new()
        .merge(legacy)
        .nest("/api/v1", api)
        .fallback(crate::web::serve)
}

/// HTTP ingest server — the online half of the 青鸟 path (HTTPSink → here).
/// POST /ingest with one envelope JSON or JSONL body; GET /health for checks.
/// v0 binds localhost by default: the collector runs on the same trust domain.
pub async fn run_serve(args: Vec<String>) -> anyhow::Result<()> {
    let mut db_path = std::path::PathBuf::from("shanhaijing.db");
    let mut listen = String::from("127.0.0.1:7321");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db_path = std::path::PathBuf::from(it.next().context("--db needs a value")?),
            "--listen" => listen = it.next().context("--listen needs a value")?,
            other => bail!("unknown serve flag {other}"),
        }
    }
    match crate::settings::migrate_env_file() {
        Ok(changed) if !changed.is_empty() => {
            println!(
                "env        : migrated lowercase keys -> {}（restart services to apply）",
                changed.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("env migrate: {e}"),
    }
    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );
    // Shared collector token (NUWA_INGEST_TOKEN). Set it when serving beyond
    // localhost: without it anyone can spam the issue store. This is a
    // report-only token — leak consequence is junk signals, rotate freely;
    // it is NOT an LLM key or repo credential (ADR: 现场无密钥红线不破).
    let ingest_token = Arc::new(std::env::var("NUWA_INGEST_TOKEN").ok().filter(|t| !t.is_empty()));
    // 控制台鉴权与上报令牌分离（设置页可改密钥）：未设 NUWA_UI_TOKEN 时
    // fallback 到上报令牌，兼容既有部署。
    let ui_token = std::env::var("NUWA_UI_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(|t| Arc::new(Some(t)) as Arc<Option<String>>)
        .unwrap_or_else(|| Arc::clone(&ingest_token));
    let ttl_secs: i64 = std::env::var("NUWA_SESSION_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(86_400);
    let cookie_secure = std::env::var("NUWA_COOKIE_SECURE").ok().as_deref() == Some("always");
    let state = AppState {
        store: Arc::clone(&store),
        ui_token: Arc::clone(&ui_token),
        ingest_token: Arc::clone(&ingest_token),
        session: Arc::new(crate::session::SessionManager::new(
            ui_token.as_deref().unwrap_or(""),
            ttl_secs * 1000,
        )),
        limiter: Arc::new(crate::session::LoginLimiter::new()),
        started_at_ms: now_ms(),
        cookie_secure,
    };
    let app = build_app(state);

    let token_mode = if ingest_token.is_some() { "required" } else { "DISABLED (local mode)" };
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    println!(
        "serve      : listening on http://{listen} (POST /ingest, GET /health) — token {token_mode}"
    );
    // ConnectInfo is required by the Task 4 login rate limiter (peer IP).
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .context("serve")
}

/// Shared-token gate. `401` unless NUWA_INGEST_TOKEN is unset (local mode)
/// or the `X-NUWA-Token` header matches.
async fn check_auth(
    State((_, expected)): State<Shared>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    if let Some(expected) = expected.as_deref() {
        let got = headers
            .get("x-nuwa-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if got != expected {
            return (
                StatusCode::UNAUTHORIZED,
                "missing or wrong X-NUWA-Token header".to_string(),
            )
                .into_response();
        }
    }
    next.run(req).await
}

async fn ingest_endpoint(State((store, _)): State<Shared>, body: axum::body::Bytes) -> (StatusCode, String) {
    let raw = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_REQUEST, format!("bad utf8: {e}")),
    };
    let signals = match parse_signals(raw) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return (axum::http::StatusCode::BAD_REQUEST, "empty payload".to_string()),
        Err(e) => return (axum::http::StatusCode::BAD_REQUEST, format!("bad payload: {e:#}")),
    };
    let now = now_ms();
    let mut lines = Vec::with_capacity(signals.len());
    for signal in &signals {
        match report_one(&store, signal, now) {
            Ok(line) => lines.push(line),
            Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("store: {e:#}")),
        }
    }
    (axum::http::StatusCode::OK, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use tower::ServiceExt;

    fn test_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(Some("secret".into())),
            ingest_token: Arc::new(Some("ingest-secret".into())),
            session: Arc::new(crate::session::SessionManager::new("secret", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            cookie_secure: false,
        };
        (dir, build_app(state))
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn health_is_public() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "nuwa serve ok");
    }

    #[tokio::test]
    async fn legacy_ui_requires_basic_auth() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(Request::builder().uri("/ui").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_some());
    }

    #[tokio::test]
    async fn legacy_ui_accepts_basic_auth() {
        use base64::Engine;
        let (_dir, app) = test_app();
        let credential =
            base64::engine::general_purpose::STANDARD.encode("owner:secret");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ui")
                    .header(header::AUTHORIZATION, format!("Basic {credential}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("山海经"));
    }

    #[tokio::test]
    async fn unknown_api_route_returns_json_404() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(Request::builder().uri("/api/v1/nope").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.starts_with("application/json"), "got {content_type}");
        let value: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json body");
        assert_eq!(value["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn root_serves_placeholder_console() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html")));
        assert!(body_text(response).await.contains("Nuwa console"));
    }

    // The legacy /ingest gate is the ingest token, not Basic auth: the refactor
    // must not move it behind the UI middleware.
    #[tokio::test]
    async fn ingest_keeps_token_gate() {
        let (_dir, app) = test_app();
        let response = app
            .clone()
            .oneshot(
                Request::builder().method("POST").uri("/ingest").body(Body::empty()).expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let signal = serde_json::json!({
            "repo": "example.com/spike",
            "language": "go",
            "title": "panic: boom",
            "severity": "high",
            "stack_trace": "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.F()\n\t/app/a.go:1 +0x1"
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ingest")
                    .header("x-nuwa-token", "ingest-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(signal.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("new issue"));
    }
}
