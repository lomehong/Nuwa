//! `nuwa serve` — single-port HTTP surface: legacy console, `/api/v1`,
//! ingest and the static fallback. Extracted from `main.rs` in M2a so the
//! router can be assembled and route-tested without binding a socket.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::{now_ms, parse_signals, report_one, Shared};

/// Strict CSP for the JSON API and the SPA: no inline script or style.
const CSP_STRICT: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
/// The legacy console keeps inline styles until it is replaced in M6.
const CSP_LEGACY: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'self'";

/// Session TTL bounds and default from docs/09 (`NUWA_SESSION_TTL_SECS`).
const SESSION_TTL_DEFAULT_SECS: i64 = 86_400;
const SESSION_TTL_MIN_SECS: i64 = 300;
const SESSION_TTL_MAX_SECS: i64 = 2_592_000;

/// State shared by the legacy console routes and the `/api/v1` surface.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<shanhaijing::Store>,
    /// Effective console token: `NUWA_UI_TOKEN`, else the ingest token.
    pub ui_token: Arc<Option<String>>,
    pub ingest_token: Arc<Option<String>>,
    pub session: Arc<crate::session::SessionManager>,
    pub limiter: Arc<crate::session::LoginLimiter>,
    /// Read by the M2c system-status endpoint.
    #[allow(dead_code)]
    pub started_at_ms: i64,
    pub cookie_secure: bool,
}

impl AppState {
    /// Local mode = no console token configured. An empty token is treated as
    /// unset everywhere so a hand-built state cannot accept any login while
    /// the session middleware still demands a cookie.
    pub fn local_mode(&self) -> bool {
        self.ui_token.as_deref().map_or(true, str::is_empty)
    }
}

/// Full app assembly: legacy console + `/api/v1` + static fallback.
/// The security-header layer wraps every route, so auth rejections and
/// fallback responses carry the headers too.
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
        .route(
            "/ingest",
            // Non-POST methods are a plain 404, never the SPA shell (the
            // fallback below also guards this path).
            post(ingest_endpoint)
                .route_layer(ingest_auth)
                .fallback(|| async { StatusCode::NOT_FOUND }),
        )
        .route("/health", get(|| async { "nuwa serve ok" }))
        .merge(legacy_ui)
        .with_state(legacy_state);

    let api = crate::api::router(Arc::new(state.clone()));
    Router::new()
        .merge(legacy)
        .nest("/api/v1", api)
        .fallback(crate::web::serve)
        .layer(axum::middleware::from_fn(crate::serve::security_headers))
}

/// Response hardening for every route. The legacy console (`/ui`, `/issue/`,
/// `/settings`) keeps `style-src 'unsafe-inline'` until M6 replaces it; the
/// JSON API and the SPA get the strict policy.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let legacy = path.starts_with("/ui")
        || path.starts_with("/issue/")
        || path.starts_with("/settings");
    // API payloads (session state, M2b read data) must never be cached. Static
    // responses keep the cache policy their handler set.
    let api = path == "/api" || path.starts_with("/api/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    let csp = if legacy { CSP_LEGACY } else { CSP_STRICT };
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    if api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
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
    let ttl_ms = session_ttl_ms(std::env::var("NUWA_SESSION_TTL_SECS").ok().as_deref());
    let cookie_secure = std::env::var("NUWA_COOKIE_SECURE").ok().as_deref() == Some("always");
    let state = AppState {
        store: Arc::clone(&store),
        ui_token: Arc::clone(&ui_token),
        ingest_token: Arc::clone(&ingest_token),
        session: Arc::new(crate::session::SessionManager::new(
            ui_token.as_deref().unwrap_or(""),
            ttl_ms,
        )),
        limiter: Arc::new(crate::session::LoginLimiter::new()),
        started_at_ms: now_ms(),
        cookie_secure,
    };
    let local_mode = state.local_mode();
    let app = build_app(state);

    let token_mode = if ingest_token.is_some() { "required" } else { "DISABLED (local mode)" };
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    println!(
        "serve      : listening on http://{listen} (POST /ingest, GET /health) — token {token_mode}"
    );
    println!(
        "api auth   : {}",
        if local_mode { "LOCAL MODE" } else { "session" }
    );
    if local_mode && !listen_is_local(&listen) {
        eprintln!(
            "warning    : API auth is in LOCAL MODE while listening on {listen}; anyone who can reach the port has full console API access"
        );
    }
    // ConnectInfo is required by the Task 4 login rate limiter (peer IP).
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .context("serve")
}

/// Parse `NUWA_SESSION_TTL_SECS`, clamp it to the documented 5 min – 30 day
/// range, and convert to milliseconds without overflow.
fn session_ttl_ms(raw: Option<&str>) -> i64 {
    raw.and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(SESSION_TTL_DEFAULT_SECS)
        .clamp(SESSION_TTL_MIN_SECS, SESSION_TTL_MAX_SECS)
        .saturating_mul(1000)
}

/// Whether the configured listen address stays on this host. Used only for
/// the local-mode startup warning; bind behavior is unchanged.
fn listen_is_local(listen: &str) -> bool {
    if listen.starts_with("localhost:") {
        return true;
    }
    listen
        .parse::<SocketAddr>()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
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

    fn test_state(ui_token: Option<&str>) -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let ui_token = Arc::new(ui_token.map(str::to_string));
        let session_key = ui_token.as_deref().unwrap_or("");
        let state = AppState {
            store,
            ui_token: Arc::clone(&ui_token),
            ingest_token: Arc::new(Some("ingest-secret".into())),
            session: Arc::new(crate::session::SessionManager::new(
                session_key,
                86_400_000,
            )),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            cookie_secure: false,
        };
        (dir, state)
    }

    fn test_app() -> (tempfile::TempDir, Router) {
        let (dir, state) = test_state(Some("secret"));
        (dir, build_app(state))
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).to_string()
    }

    async fn json_body(response: Response) -> serde_json::Value {
        serde_json::from_str(&body_text(response).await).expect("json body")
    }

    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
    const LEGACY_CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'self'";

    fn header_str(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
            .to_string()
    }

    fn assert_common_security_headers(response: &Response) {
        assert_eq!(
            header_str(response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert_eq!(header_str(response, header::REFERRER_POLICY), "no-referrer");
        assert_eq!(header_str(response, header::X_FRAME_OPTIONS), "DENY");
        assert_eq!(
            header_str(
                response,
                header::HeaderName::from_static("permissions-policy")
            ),
            "camera=(), microphone=(), geolocation=()"
        );
    }

    fn peer() -> axum::extract::ConnectInfo<SocketAddr> {
        axum::extract::ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41234)))
    }

    async fn login(app: &Router, token: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .extension(peer())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token }).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    fn set_cookie(response: &Response, name: &str) -> String {
        let prefix = format!("{name}=");
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&prefix))
            .unwrap_or_else(|| panic!("missing Set-Cookie for {name}"))
            .to_string()
    }

    fn cookie_pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().expect("cookie pair").to_string()
    }

    fn cookie_token(pair: &str) -> &str {
        pair.split_once('=').expect("cookie token").1
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

    // The M0 origin guard must remain wired around the legacy write routes:
    // valid Basic auth alone must not let a cross-site POST through.
    #[tokio::test]
    async fn legacy_write_rejects_cross_origin_post_even_with_valid_basic_auth() {
        use base64::Engine;
        let (_dir, app) = test_app();
        let credential = base64::engine::general_purpose::STANDARD.encode("owner:secret");
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/settings/save")
                    .header(header::HOST, "localhost:7321")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(header::AUTHORIZATION, format!("Basic {credential}"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_common_security_headers(&response);
        assert_eq!(
            header_str(&response, header::CONTENT_SECURITY_POLICY),
            LEGACY_CSP
        );
        assert!(body_text(response).await.contains("cross-origin"));
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

    // Build-independent: once M3 ships `web/dist/index.html` this serves the
    // real shell, so only status/content-type/cache are asserted here. The
    // placeholder body is covered by the web.rs unit tests.
    #[tokio::test]
    async fn root_serves_the_spa_shell() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            header_str(&response, header::CONTENT_TYPE).starts_with("text/html"),
            "the shell must be HTML whether it is the placeholder or a built index.html"
        );
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-cache");
    }

    #[tokio::test]
    async fn api_response_carries_strict_security_headers() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nope")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_common_security_headers(&response);
        let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
        assert_eq!(csp, STRICT_CSP);
        assert!(
            !csp.contains("unsafe-inline"),
            "API CSP must stay strict: {csp}"
        );
        assert_eq!(
            header_str(&response, header::CACHE_CONTROL),
            "no-store",
            "API responses must never be cached"
        );
    }

    // The legacy console keeps inline styles until M6, and headers must be
    // applied even when auth rejects the request before a handler runs.
    #[tokio::test]
    async fn legacy_console_responses_carry_relaxed_security_headers() {
        let (_dir, app) = test_app();
        for path in ["/ui", "/settings", "/issue/abc123"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            assert_common_security_headers(&response);
            let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
            assert_eq!(csp, LEGACY_CSP, "{path}");
            assert!(
                csp.contains("style-src 'self' 'unsafe-inline'"),
                "legacy CSP must allow inline styles: {csp}"
            );
        }
    }

    #[tokio::test]
    async fn static_shell_carries_strict_security_headers() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_common_security_headers(&response);
        assert_eq!(
            header_str(&response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-cache");
    }

    // Extensionless GET/HEAD paths are SPA routes. Every other unmatched
    // method stays a 404 — the placeholder must not answer writes.
    #[tokio::test]
    async fn spa_routes_answer_get_and_head_only() {
        let (_dir, app) = test_app();
        for method in ["GET", "HEAD"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/issues/abc")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{method}");
            assert_common_security_headers(&response);
            assert_eq!(
                header_str(&response, header::CONTENT_SECURITY_POLICY),
                STRICT_CSP,
                "{method}"
            );
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/issues/abc")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
            assert_common_security_headers(&response);
        }
    }

    #[tokio::test]
    async fn missing_static_asset_is_404_with_strict_security_headers() {
        let (_dir, app) = test_app();
        for path in ["/assets/missing-abc123.js", "/missing-file-xyz.xyz"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_common_security_headers(&response);
            assert_eq!(
                header_str(&response, header::CONTENT_SECURITY_POLICY),
                STRICT_CSP,
                "{path}"
            );
        }
    }

    // Only `/api/v1` is a real API mount. Other `/api` paths must not fall
    // through to the SPA or the legacy console.
    #[tokio::test]
    async fn unknown_api_namespace_returns_json_404() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v2/x")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_common_security_headers(&response);
        assert_eq!(
            header_str(&response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");
    }

    // Legacy backend namespaces must never answer the SPA shell with a 200:
    // `/ingest` is POST-only and `/health/...` has no routes.
    #[tokio::test]
    async fn legacy_backend_paths_are_404_not_spa() {
        let (_dir, app) = test_app();
        for path in ["/ingest", "/health/unknown"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            assert!(
                !content_type.starts_with("text/html"),
                "{path} must not serve the SPA: {content_type}"
            );
        }
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

    #[tokio::test]
    async fn api_login_issues_cookies_and_me_reports_session() {
        let (_dir, app) = test_app();
        let response = login(&app, "secret").await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let session = set_cookie(&response, "nuwa_session");
        let csrf = set_cookie(&response, "nuwa_csrf");
        for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=86400"] {
            assert!(
                session.contains(attr),
                "session Set-Cookie missing {attr}: {session}"
            );
        }
        assert!(
            !session.contains("Secure"),
            "plain HTTP must not mark the session Secure: {session}"
        );
        assert!(
            !csrf.contains("HttpOnly"),
            "csrf cookie must stay readable by the SPA: {csrf}"
        );
        for attr in ["SameSite=Strict", "Path=/", "Max-Age=86400"] {
            assert!(csrf.contains(attr), "csrf Set-Cookie missing {attr}: {csrf}");
        }
        assert!(
            !csrf.contains("Secure"),
            "plain HTTP must not mark the csrf cookie Secure: {csrf}"
        );

        let cookie_header = format!("{}; {}", cookie_pair(&session), cookie_pair(&csrf));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .extension(peer())
                    .header(header::COOKIE, cookie_header)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], false);
        assert!(value["expires_at_ms"].is_i64(), "got {value}");
    }

    #[tokio::test]
    async fn api_me_without_session_is_unauthorized() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .extension(peer())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            header_str(&response, header::CACHE_CONTROL),
            "no-store",
            "API 401 responses must never be cached"
        );
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "unauthorized");
    }

    // A present-but-empty `nuwa_csrf` cookie cannot pass the CSRF guard, so
    // `/auth/me` must treat it as absent and reissue a usable token.
    #[tokio::test]
    async fn me_reissues_an_empty_csrf_cookie() {
        let (_dir, app) = test_app();
        let login_response = login(&app, "secret").await;
        assert_eq!(login_response.status(), StatusCode::NO_CONTENT);
        let session_pair = cookie_pair(&set_cookie(&login_response, "nuwa_session"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .extension(peer())
                    .header(header::COOKIE, format!("{session_pair}; nuwa_csrf="))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let csrf = set_cookie(&response, "nuwa_csrf");
        let csrf_pair = cookie_pair(&csrf);
        let reissued = cookie_token(&csrf_pair);
        assert!(
            !reissued.is_empty(),
            "empty csrf cookie must be replaced: {csrf}"
        );
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], false);
    }

    #[tokio::test]
    async fn api_logout_requires_csrf_and_clears_cookies() {
        let (_dir, app) = test_app();
        let login_response = login(&app, "secret").await;
        assert_eq!(login_response.status(), StatusCode::NO_CONTENT);
        let session_pair = cookie_pair(&set_cookie(&login_response, "nuwa_session"));
        let csrf_pair = cookie_pair(&set_cookie(&login_response, "nuwa_csrf"));
        let csrf_token = cookie_token(&csrf_pair).to_string();

        // Session present but the double-submit token missing: 403.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, &session_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        // Present but mismatched header token: still 403.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, format!("{session_pair}; {csrf_pair}"))
                    .header("x-nuwa-csrf", "mismatched-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // Valid session + cookie/header token: 204 and both cookies cleared.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, format!("{session_pair}; {csrf_pair}"))
                    .header("x-nuwa-csrf", &csrf_token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        for name in ["nuwa_session", "nuwa_csrf"] {
            let cleared = set_cookie(&response, name);
            assert!(
                cleared.contains("Max-Age=0"),
                "{name} removal missing Max-Age=0: {cleared}"
            );
            assert!(
                cleared.contains("Path=/"),
                "{name} removal missing Path=/: {cleared}"
            );
        }

        // Valid CSRF without a session is still an auth failure.
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, &csrf_pair)
                    .header("x-nuwa-csrf", &csrf_token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_login_rate_limits_after_five_failures_with_retry_after() {
        let (_dir, app) = test_app();
        for _ in 0..5 {
            let response = login(&app, "wrong").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = login(&app, "wrong").await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .expect("Retry-After header")
            .to_str()
            .expect("ascii")
            .parse::<u64>()
            .expect("Retry-After seconds");
        assert!(
            (1..=600).contains(&retry_after),
            "Retry-After out of window: {retry_after}"
        );
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "rate_limited");
    }

    // Extractor rejections must use the JSON error contract, not axum's
    // plain-text default: the SPA can only render `{"error": ...}`.
    #[tokio::test]
    async fn api_login_maps_json_rejections_to_the_error_contract() {
        let (_dir, app) = test_app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .extension(peer())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"token":"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "invalid_request");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .extension(peer())
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "unsupported_media_type");
    }

    #[tokio::test]
    async fn api_local_mode_is_authenticated_but_still_requires_csrf() {
        let (_dir, state) = test_state(None);
        let app = build_app(state);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .extension(peer())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let csrf = set_cookie(&response, "nuwa_csrf");
        assert!(
            !csrf.contains("HttpOnly"),
            "local mode still hands the CSRF token to the SPA: {csrf}"
        );
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], true);
        assert!(value["expires_at_ms"].is_null(), "got {value}");

        let csrf_pair = cookie_pair(&csrf);
        let csrf_token = cookie_token(&csrf_pair).to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, &csrf_pair)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "local mode must still demand the CSRF header"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::COOKIE, csrf_pair)
                    .header("x-nuwa-csrf", csrf_token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    // `Some("")` must mean local mode everywhere: previously login accepted
    // any token while the middleware still demanded a session.
    #[tokio::test]
    async fn empty_ui_token_is_local_mode() {
        let (_dir, state) = test_state(Some(""));
        let app = build_app(state);
        let response = login(&app, "anything").await;
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "local mode login accepts any token"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .extension(peer())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], true);
        assert!(value["expires_at_ms"].is_null(), "got {value}");
    }

    #[tokio::test]
    async fn api_login_marks_cookies_secure_behind_tls_proxy() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .extension(peer())
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-proto", "https")
                    .body(Body::from(r#"{"token":"secret"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        for name in ["nuwa_session", "nuwa_csrf"] {
            let cookie = set_cookie(&response, name);
            assert!(
                cookie.contains("; Secure"),
                "{name} missing Secure behind TLS proxy: {cookie}"
            );
        }
    }

    #[tokio::test]
    async fn api_csrf_checks_origin_and_rejects_empty_host() {
        let (_dir, app) = test_app();
        let login_response = login(&app, "secret").await;
        let session_pair = cookie_pair(&set_cookie(&login_response, "nuwa_session"));
        let csrf_pair = cookie_pair(&set_cookie(&login_response, "nuwa_csrf"));
        let csrf_token = cookie_token(&csrf_pair).to_string();
        let cookie_header = format!("{session_pair}; {csrf_pair}");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "localhost:7321")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(header::COOKIE, &cookie_header)
                    .header("x-nuwa-csrf", &csrf_token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .extension(peer())
                    .header(header::HOST, "")
                    .header(header::COOKIE, cookie_header)
                    .header("x-nuwa-csrf", csrf_token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "an empty Host header must not satisfy the origin check"
        );
    }

    #[test]
    fn session_ttl_is_clamped_and_converted_to_ms() {
        assert_eq!(session_ttl_ms(None), 86_400_000, "default 24h");
        assert_eq!(session_ttl_ms(Some("garbage")), 86_400_000);
        assert_eq!(session_ttl_ms(Some("60")), 300_000, "clamped to 5 min");
        assert_eq!(session_ttl_ms(Some("600")), 600_000);
        assert_eq!(
            session_ttl_ms(Some("999999999999")),
            2_592_000_000,
            "clamped to 30 days"
        );
    }

    #[test]
    fn listen_is_local_accepts_loopback_and_localhost_only() {
        for listen in ["127.0.0.1:7321", "localhost:7321", "[::1]:7321"] {
            assert!(listen_is_local(listen), "{listen}");
        }
        for listen in ["0.0.0.0:7321", "192.168.1.10:7321", "example.com:7321"] {
            assert!(!listen_is_local(listen), "{listen}");
        }
    }
}
