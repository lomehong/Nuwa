//! `nuwa serve` — single-port HTTP surface: SPA, `/api/v1`, ingest and the
//! static fallback. Extracted from `main.rs` in M2a so the router can be
//! assembled and route-tested without binding a socket.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::{now_ms, parse_signals, report_one, Shared};

/// Strict CSP for every route (API, SPA and redirects): no inline script or
/// style.
const CSP_STRICT: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// The embedded architecture diagram (`web/public/architecture.html`) is a
/// self-contained archify artifact — inline script/style by construction — and
/// the console frames it inside `/architecture`. This policy applies only to
/// that one build-owned file (same-origin framing + the inline content it
/// ships); every other route keeps the strict no-frame CSP above.
const CSP_ARCHITECTURE: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'self'; base-uri 'none'; form-action 'self'";

/// Only this path may be framed by the console and only it ships inline
/// content; see `CSP_ARCHITECTURE`.
const ARCHITECTURE_PAGE_PATH: &str = "/architecture.html";

/// Frame + content policy for a request path, as `(X-Frame-Options, CSP)`.
fn frame_policy_for(path: &str) -> (HeaderValue, HeaderValue) {
    if path == ARCHITECTURE_PAGE_PATH {
        (
            HeaderValue::from_static("SAMEORIGIN"),
            HeaderValue::from_static(CSP_ARCHITECTURE),
        )
    } else {
        (
            HeaderValue::from_static("DENY"),
            HeaderValue::from_static(CSP_STRICT),
        )
    }
}

/// Session TTL bounds and default from docs/09 (`NUWA_SESSION_TTL_SECS`).
const SESSION_TTL_DEFAULT_SECS: i64 = 86_400;
const SESSION_TTL_MIN_SECS: i64 = 300;
const SESSION_TTL_MAX_SECS: i64 = 2_592_000;

/// Background retention sweep cadence for a long-running `serve`.
const RETENTION_PERIOD: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// State shared by the `/api/v1` surface, the ingest gate and the SPA static
/// fallback (the legacy server-rendered console is gone as of 0.2.0).
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<shanhaijing::Store>,
    /// Effective console token: `NUWA_UI_TOKEN`, else the ingest token.
    pub ui_token: Arc<Option<String>>,
    pub ingest_token: Arc<Option<String>>,
    pub session: Arc<crate::session::SessionManager>,
    pub limiter: Arc<crate::session::LoginLimiter>,
    /// Process start time; a `config_changed` event newer than this means the
    /// running processes still use the previous env.
    pub started_at_ms: i64,
    /// `NUWA_ENV_FILE` (default `/opt/nuwa/nuwa.env`) — read and written by
    /// the settings API.
    pub env_path: PathBuf,
    /// `--db` path; shown by the system endpoint.
    pub db_path: PathBuf,
    /// `NUWA_COOKIE_SECURE=always`: force the `Secure` attribute.
    pub cookie_secure: bool,
    /// `NUWA_COOKIE_SECURE=never`: never set `Secure`, even when a proxy
    /// reports `X-Forwarded-Proto: https` (the explicit opt-out wins).
    pub cookie_secure_never: bool,
    /// `NUWA_SERVE_UNIT` (default `nuwa-serve`); restart target and the
    /// system endpoint's first service.
    pub serve_unit: String,
    /// `NUWA_RUNNER_UNITS` comma list (default `["nuwa-work"]`); restart
    /// targets and the system endpoint's remaining services.
    pub runner_units: Vec<String>,
}

impl AppState {
    /// Local mode = no console token configured. An empty token is treated as
    /// unset everywhere so a hand-built state cannot accept any login while
    /// the session middleware still demands a cookie.
    pub fn local_mode(&self) -> bool {
        self.ui_token.as_deref().is_none_or(str::is_empty)
    }
}

/// Full app assembly: `/api/v1`, ingest, SPA redirects and the static
/// fallback. The security-header layer wraps every route, so auth rejections
/// and fallback responses carry the headers too.
pub fn build_app(state: AppState) -> Router {
    let ingest_state: Shared = (Arc::clone(&state.store), Arc::clone(&state.ingest_token));
    let ingest_auth = axum::middleware::from_fn_with_state(
        (Arc::clone(&state.store), Arc::clone(&state.ingest_token)),
        check_auth,
    );
    // `/ingest` and `/health` keep their own gates; the SPA owns everything
    // else, except the two legacy-console namespaces redirected below.
    let backend = Router::new()
        .route(
            "/ingest",
            // Non-POST methods are a plain 404, never the SPA shell (the
            // fallback below also guards this path).
            post(ingest_endpoint)
                .route_layer(ingest_auth)
                .fallback(|| async { StatusCode::NOT_FOUND }),
        )
        .route("/health", get(|| async { "nuwa serve ok" }))
        .with_state(ingest_state);
    // Bookmark compatibility from the retired console (M6): `/ui*` returns
    // to the SPA root and `/issue/{fp}` to its new detail route; the bare
    // trailing-slash forms point at the canonical pages. `/settings` needs no
    // redirect — it is the same SPA path. Legacy POSTs stay 404 instead of
    // falling through to the SPA shell.
    let redirects = Router::new()
        .route("/ui", get(legacy_console_redirect).post(not_found))
        .route("/ui/", get(legacy_console_redirect).post(not_found))
        .route("/ui/{*rest}", get(legacy_console_redirect).post(not_found))
        .route(
            "/issue/",
            get(|| async { Redirect::permanent("/issues") }).post(not_found),
        )
        .route("/issue/{fp}", get(issue_redirect).post(not_found))
        .route("/issue/{fp}/", get(issue_redirect).post(not_found));

    let api = crate::api::router(Arc::new(state.clone()));
    Router::new()
        .merge(backend)
        .merge(redirects)
        .nest("/api/v1", api)
        .fallback(crate::web::serve)
        .layer(axum::middleware::from_fn(crate::serve::security_headers))
}

async fn legacy_console_redirect() -> Redirect {
    Redirect::permanent("/")
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// `/issue/{fp}` → `/issues/{fp}`. Fingerprints are opaque ids, so anything
/// outside the safe ASCII set is a 404 rather than a `Location` a browser
/// could reinterpret (a decoded CR/LF would otherwise panic the redirect).
async fn issue_redirect(Path(fp): Path<String>) -> Response {
    if is_safe_fingerprint(&fp) {
        Redirect::permanent(&format!("/issues/{fp}")).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// ASCII alphanumeric plus `-_:` only. Dots are rejected so a dotted segment
/// cannot be redirected into `web.rs`'s dotfile 404.
fn is_safe_fingerprint(fp: &str) -> bool {
    !fp.is_empty()
        && fp
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}

/// Response hardening for every route. Responses use the strict CSP; the
/// embedded architecture diagram alone is served frameable (see
/// `frame_policy_for`).
pub async fn security_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // API payloads (session state, M2b read data) must never be cached. Static
    // responses keep the cache policy their handler set.
    let api = path == "/api" || path.starts_with("/api/");
    let (frame_options, csp) = frame_policy_for(path);
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
    h.insert(header::X_FRAME_OPTIONS, frame_options);
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    h.insert(header::CONTENT_SECURITY_POLICY, csp);
    if api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// HTTP ingest server — the online half of the 青鸟 path (HTTPSink → here).
/// POST /ingest with one envelope JSON or JSONL body; GET /health for checks.
/// v0 binds localhost by default: the collector runs on the same trust domain.
pub async fn run_serve(args: Vec<String>) -> anyhow::Result<()> {
    crate::webhook::init_from_env();
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
    let event_days = event_retention_days();
    report_prune(prune_retention(&store, now_ms(), event_days));
    // A long-running serve must not grow forever between restarts: re-run the
    // same sweep daily from the tokio runtime. The returned handle is dropped
    // on purpose — the task runs until the runtime shuts down.
    spawn_daily_prune(Arc::clone(&store), event_days, RETENTION_PERIOD);
    // pr_open issues no longer depend on the owner remembering to click
    // "sync"; the same reconciler also feeds sync_error webhooks.
    let sync_token = std::env::var("NUWA_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    spawn_reconciler(Arc::clone(&store), sync_token, sync_interval_from_env());
    spawn_heartbeat_monitor(Arc::clone(&store));
    let env_path = crate::settings::env_file_path();
    let tokens = serve_tokens(&std::env::vars().collect());
    let ingest_token = tokens.ingest;
    let ui_token = tokens.console;
    let ttl_ms = session_ttl_ms(std::env::var("NUWA_SESSION_TTL_SECS").ok().as_deref());
    let cookie_mode = std::env::var("NUWA_COOKIE_SECURE").ok();
    let cookie_secure = cookie_mode.as_deref() == Some("always");
    // `never` is an explicit opt-out that also beats a proxy's
    // `X-Forwarded-Proto: https`.
    let cookie_secure_never = cookie_mode.as_deref() == Some("never");
    let serve_unit = serve_unit_from_env(std::env::var("NUWA_SERVE_UNIT").ok().as_deref());
    let runner_units = runner_units_from_env(std::env::var("NUWA_RUNNER_UNITS").ok().as_deref());
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
        env_path,
        db_path,
        cookie_secure,
        cookie_secure_never,
        serve_unit,
        runner_units,
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
        bail!(
            "API auth is in LOCAL MODE (no NUWA_UI_TOKEN) while listening on {listen} — \
             anyone who can reach the port would have full console API access. \
             Set NUWA_UI_TOKEN (a console token, not the collector token) or listen on loopback."
        );
    }
    // ConnectInfo is required by the Task 4 login rate limiter (peer IP).
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .context("serve")
}

/// The two server credentials.
///
/// `NUWA_INGEST_TOKEN` is the shared collector token: it is deployed on every
/// instrumented service, so its leak consequence is junk signals and it can
/// be rotated freely. It is NOT an LLM key or a repo credential.
///
/// `NUWA_UI_TOKEN` is the console token. It never falls back to the
/// collector token: the console can read the settings page, and the settings
/// page can talk to the model provider — falling back would turn "can post
/// signals" into "can read the LLM key". Without a console token the console
/// runs in local mode, which `run_serve` refuses to expose on a non-loopback
/// listener.
struct ServeTokens {
    ingest: Arc<Option<String>>,
    console: Arc<Option<String>>,
}

fn serve_tokens(env: &std::collections::BTreeMap<String, String>) -> ServeTokens {
    let token = |key: &str| {
        env.get(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    ServeTokens {
        ingest: Arc::new(token("NUWA_INGEST_TOKEN")),
        console: Arc::new(token("NUWA_UI_TOKEN")),
    }
}

/// Parse `NUWA_SESSION_TTL_SECS`, clamp it to the documented 5 min – 30 day
/// range, and convert to milliseconds without overflow.
fn session_ttl_ms(raw: Option<&str>) -> i64 {
    raw.and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(SESSION_TTL_DEFAULT_SECS)
        .clamp(SESSION_TTL_MIN_SECS, SESSION_TTL_MAX_SECS)
        .saturating_mul(1000)
}

/// `NUWA_SERVE_UNIT` — the serve unit name used by restart actions; blank
/// falls back to the documented default.
fn serve_unit_from_env(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("nuwa-serve")
        .to_string()
}

/// `NUWA_RUNNER_UNITS` — comma separated runner units; blanks are dropped and
/// an empty list falls back to `nuwa-work`.
fn runner_units_from_env(raw: Option<&str>) -> Vec<String> {
    let units: Vec<String> = raw
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|unit| !unit.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if units.is_empty() {
        vec!["nuwa-work".to_string()]
    } else {
        units
    }
}

/// `NUWA_EVENT_RETENTION_DAYS` — normal events roll off after this many days.
fn event_retention_days() -> u32 {
    retention_days_from_env(std::env::var("NUWA_EVENT_RETENTION_DAYS").ok().as_deref())
}

/// `NUWA_EVENT_RETENTION_DAYS` — normal events roll off after this many days.
/// Malformed values fall back to the documented 90-day default; out-of-range
/// values (including negatives) clamp to 1–3650.
pub(crate) fn retention_days_from_env(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<i64>().ok())
        .map(|days| days.clamp(1, 3_650) as u32)
        .unwrap_or(90)
}

/// Audit events keep at least their historical 365-day window, and outlive
/// normal events when `NUWA_EVENT_RETENTION_DAYS` is raised beyond a year.
fn audit_days_for(event_days: u32) -> u32 {
    event_days.max(365)
}

/// One best-effort retention sweep, shared by startup, the daily task and
/// `work` so every caller uses the same audit window.
pub(crate) fn prune_retention(
    store: &shanhaijing::Store,
    now: i64,
    event_days: u32,
) -> anyhow::Result<u64> {
    store
        .prune(now, event_days, audit_days_for(event_days), 365)
        .map_err(Into::into)
}

fn report_prune(result: anyhow::Result<u64>) {
    match result {
        Ok(deleted) if deleted > 0 => println!("retention : pruned {deleted} rows"),
        Ok(_) => {}
        Err(error) => eprintln!("warning    : retention prune failed: {error}"),
    }
}

/// Progress older than this while a runner still reports `working` is treated
/// as a hung run for the webhook (the console's requeue floor stays shorter).
const HEARTBEAT_STALE_MS: i64 = 15 * 60_000;
const HEARTBEAT_MONITOR_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);

fn sync_interval_from_env() -> std::time::Duration {
    let raw = std::env::var("NUWA_SYNC_INTERVAL").ok();
    sync_interval_from_secs(raw.as_deref())
}

fn sync_interval_from_secs(raw: Option<&str>) -> std::time::Duration {
    let secs = raw
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600)
        .clamp(60, 86_400);
    std::time::Duration::from_secs(secs)
}

fn spawn_reconciler(
    store: Arc<shanhaijing::Store>,
    token: Option<String>,
    period: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // the startup path does not reconcile immediately
        loop {
            ticker.tick().await;
            let Some(token) = token.clone() else {
                continue;
            };
            let Ok(_guard) = crate::api::actions::sync_lock().try_lock() else {
                continue;
            };
            match crate::ci::reconcile_all(&store, &token, now_ms()).await {
                Ok(lines) => {
                    for line in lines {
                        if line.starts_with("error") || line.starts_with("skip") {
                            crate::webhook::emit(crate::webhook::WebhookEvent::new(
                                "sync_error",
                                None,
                                None,
                                line,
                            ));
                        }
                    }
                }
                Err(error) => {
                    crate::webhook::emit(crate::webhook::WebhookEvent::new(
                        "sync_error",
                        None,
                        None,
                        format!("auto reconcile failed: {error:#}"),
                    ));
                }
            }
        }
    })
}

/// Runners whose liveness heartbeat is fresh but whose real progress is stale.
fn stale_progress(
    store: &shanhaijing::Store,
    now: i64,
    threshold_ms: i64,
) -> shanhaijing::ShanResult<Vec<shanhaijing::RunnerRow>> {
    Ok(store
        .list_runners()?
        .into_iter()
        .filter(|runner| {
            if runner.status != "working" || runner.current_fingerprint.is_none() {
                return false;
            }
            let last = if runner.progress_at > 0 {
                runner.progress_at
            } else {
                runner.heartbeat_at
            };
            now.saturating_sub(last) > threshold_ms
        })
        .collect())
}

fn spawn_heartbeat_monitor(store: Arc<shanhaijing::Store>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEARTBEAT_MONITOR_PERIOD);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut alerted: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        loop {
            ticker.tick().await;
            let now = now_ms();
            let Ok(stale) = stale_progress(&store, now, HEARTBEAT_STALE_MS) else {
                continue;
            };
            let current: std::collections::HashSet<(String, String)> = stale
                .iter()
                .filter_map(|runner| {
                    runner
                        .current_fingerprint
                        .as_ref()
                        .map(|fp| (runner.id.clone(), fp.clone()))
                })
                .collect();
            alerted.retain(|key| current.contains(key));
            for runner in stale {
                let Some(fingerprint) = runner.current_fingerprint.clone() else {
                    continue;
                };
                if !alerted.insert((runner.id.clone(), fingerprint.clone())) {
                    continue;
                }
                let last = if runner.progress_at > 0 {
                    runner.progress_at
                } else {
                    runner.heartbeat_at
                };
                crate::webhook::emit(crate::webhook::WebhookEvent::new(
                    "heartbeat_stale",
                    Some(fingerprint.clone()),
                    Some(runner.repo.clone()),
                    format!(
                        "runner {} has no progress for {}s (phase {:?})",
                        runner.id,
                        (now - last) / 1000,
                        runner.phase
                    ),
                ));
            }
        }
    })
}

/// Spawn the once-a-day retention sweep. `tokio::time::interval` fires an
/// immediate first tick, which is consumed here because `run_serve` already
/// pruned on startup.
fn spawn_daily_prune(
    store: Arc<shanhaijing::Store>,
    event_days: u32,
    period: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            report_prune(prune_retention(&store, now_ms(), event_days));
        }
    })
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
        // Constant-time: the collector token is a shared secret and this runs
        // on every reported signal.
        if !crate::session::SessionManager::token_eq(got, expected) {
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
        let db_path = dir.path().join("s.db");
        let store = Arc::new(shanhaijing::Store::open(&db_path).expect("open"));
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
            env_path: dir.path().join("nuwa.env"),
            db_path,
            cookie_secure: false,
            cookie_secure_never: false,
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
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
    async fn legacy_console_redirects_to_the_spa() {
        let (_dir, app) = test_app();
        for (uri, location) in [
            ("/ui", "/"),
            ("/ui/", "/"),
            ("/ui/reopen", "/"),
            ("/ui/sync", "/"),
            ("/issue/", "/issues"),
            ("/issue/abc123", "/issues/abc123"),
            ("/issue/abc/", "/issues/abc"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT, "{uri}");
            assert_eq!(
                header_str(&response, header::LOCATION),
                location,
                "{uri}"
            );
            let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
            assert!(csp.contains("style-src 'self'"), "{uri}: {csp}");
            assert!(!csp.contains("unsafe-inline"), "{uri}: {csp}");
            assert_eq!(header_str(&response, header::X_FRAME_OPTIONS), "DENY", "{uri}");
        }
    }

    /// End-to-end through the real middleware stack: the console must be able
    /// to frame `/architecture.html` (same-origin) with the artifact's inline
    /// content enabled, while the SPA shell keeps the strict no-frame policy.
    #[tokio::test]
    async fn architecture_page_is_served_frameable_and_the_shell_stays_strict() {
        let (_dir, app) = test_app();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/architecture.html")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        // Common hardening still applies; only the frame/CSP pair is relaxed
        // (assert_common_security_headers pins XFO=DENY, which is the point
        // being excepted here).
        assert_eq!(
            header_str(&response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert_eq!(header_str(&response, header::REFERRER_POLICY), "no-referrer");
        assert_eq!(
            header_str(
                &response,
                header::HeaderName::from_static("permissions-policy")
            ),
            "camera=(), microphone=(), geolocation=()"
        );
        assert_eq!(
            header_str(&response, header::X_FRAME_OPTIONS),
            "SAMEORIGIN"
        );
        let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
        assert!(csp.contains("script-src 'self' 'unsafe-inline'"), "{csp}");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(header_str(&response, header::X_FRAME_OPTIONS), "DENY");
        let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(!csp.contains("unsafe-inline"), "{csp}");
    }

    #[tokio::test]
    async fn legacy_write_routes_are_gone() {
        let (_dir, app) = test_app();
        for uri in [
            "/ui/",
            "/ui/reopen",
            "/ui/sync",
            "/issue/",
            "/issue/abc123",
            "/issue/abc123/",
            "/settings/save",
            "/settings/restart",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn issue_redirect_rejects_unsafe_fingerprints() {
        let (_dir, app) = test_app();
        for uri in [
            "/issue/%0d%0aX",  // decoded CR/LF must never reach `Location`
            "/issue/..",       // dot segments would be normalised by browsers
            "/issue/%2e%2e",
            "/issue/a.b",      // dots are rejected: `web.rs` would dotfile-404
            "/issue/abc.def",
            "/issue/a%20b",    // outside the alphanumeric/-_: set
            "/issue/a%2fb",    // encoded slash
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(
                response.headers().get(header::LOCATION).is_none(),
                "{uri} must not set Location"
            );
        }
    }

    #[test]
    fn is_safe_fingerprint_accepts_plain_ids_only() {
        for fp in ["abc123", "ab-cd_ef:12", "A0"] {
            assert!(is_safe_fingerprint(fp), "{fp}");
        }
        for fp in ["", ".", "..", "ab-cd_ef.1:2", "a.b", "a b", "a/b", "a\r\nb", "a\u{00e9}"] {
            assert!(!is_safe_fingerprint(fp), "{fp:?}");
        }
    }

    #[tokio::test]
    async fn settings_spa_uses_the_strict_csp() {
        let (_dir, app) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let csp = header_str(&response, header::CONTENT_SECURITY_POLICY);
        assert!(csp.contains("style-src 'self'"), "{csp}");
        assert!(!csp.contains("unsafe-inline"), "{csp}");
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
    fn serve_unit_env_defaults_and_trims() {
        assert_eq!(serve_unit_from_env(None), "nuwa-serve");
        assert_eq!(serve_unit_from_env(Some("")), "nuwa-serve");
        assert_eq!(serve_unit_from_env(Some("   ")), "nuwa-serve");
        assert_eq!(serve_unit_from_env(Some(" nuwa-web ")), "nuwa-web");
    }

    #[test]
    fn runner_units_env_splits_trims_and_defaults() {
        assert_eq!(runner_units_from_env(None), vec!["nuwa-work".to_string()]);
        assert_eq!(runner_units_from_env(Some("")), vec!["nuwa-work".to_string()]);
        assert_eq!(
            runner_units_from_env(Some(" , ,")),
            vec!["nuwa-work".to_string()],
            "a list of blanks falls back to the default"
        );
        assert_eq!(
            runner_units_from_env(Some("nuwa-work@a, nuwa-work@b ,,nuwa-runner")),
            vec![
                "nuwa-work@a".to_string(),
                "nuwa-work@b".to_string(),
                "nuwa-runner".to_string(),
            ]
        );
    }

    #[test]
    fn serve_prunes_events_past_retention_but_keeps_audit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("prune.db");
        let store = shanhaijing::Store::open(&path).expect("open");
        let now = crate::now_ms();
        store
            .record_event(
                "fp",
                now - 120 * 86_400_000,
                Some("example.com/spike"),
                "system",
                "reported",
                "old",
            )
            .expect("old event");
        store
            .record_event(
                "fp",
                now,
                Some("example.com/spike"),
                "owner",
                "config_changed",
                "keep",
            )
            .expect("audit event");
        // Older than the 90-day event window but inside the 365-day audit
        // window: audit history must survive.
        store
            .record_event(
                "fp",
                now - 200 * 86_400_000,
                Some("example.com/spike"),
                "owner",
                "config_changed",
                "audit-200d",
            )
            .expect("old audit event");

        let deleted = prune_retention(&store, now, 90).expect("prune");
        assert_eq!(deleted, 1);

        let events = store
            .list_events(&Default::default(), 100)
            .expect("events");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.kind == "config_changed"));
        assert!(events.iter().any(|e| e.detail == "keep"));
        assert!(events.iter().any(|e| e.detail == "audit-200d"));
    }

    #[test]
    fn retention_keeps_audit_rows_when_event_window_exceeds_a_year() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("long.db")).expect("open");
        let now = crate::now_ms();
        store
            .record_event(
                "fp",
                now - 410 * 86_400_000,
                Some("r"),
                "system",
                "reported",
                "old-normal",
            )
            .expect("normal event");
        store
            .record_event(
                "fp",
                now - 380 * 86_400_000,
                Some("r"),
                "owner",
                "config_changed",
                "audit",
            )
            .expect("audit event");

        let deleted = prune_retention(&store, now, 400).expect("prune");
        assert_eq!(deleted, 1);
        let events = store
            .list_events(&Default::default(), 100)
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].detail, "audit");
    }

    #[test]
    fn sync_interval_is_clamped_and_defaults_to_ten_minutes() {
        assert_eq!(sync_interval_from_secs(None).as_secs(), 600);
        assert_eq!(sync_interval_from_secs(Some("5")).as_secs(), 60);
        assert_eq!(sync_interval_from_secs(Some("999999")).as_secs(), 86_400);
        assert_eq!(sync_interval_from_secs(Some("nope")).as_secs(), 600);
    }

    #[test]
    fn stale_progress_uses_progress_not_the_heartbeat() {
        use shanhaijing::{RunnerHeartbeat, RunnerUpsert};

        let (_dir, state) = test_state(None);
        let store = &state.store;
        let now = crate::now_ms();
        store
            .upsert_runner(&RunnerUpsert {
                id: "runner-a".into(),
                repo: "example.com/app".into(),
                host: "host".into(),
                pid: None,
                started_at: now - 60_000,
                heartbeat_at: now,
                status: "working".into(),
                provider: None,
                model: None,
                daily_pr_limit: None,
                repo_path: None,
                service_unit: None,
                config_json: "{}".into(),
            })
            .expect("upsert");
        store
            .heartbeat_runner(
                "runner-a",
                &RunnerHeartbeat {
                    heartbeat_at: now,
                    status: "working".into(),
                    current_fingerprint: Some("fp".into()),
                    phase: Some("repair".into()),
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("heartbeat");
        store
            .set_runner_progress("runner-a", now - 20 * 60_000)
            .expect("progress");

        let stale = stale_progress(store, now, HEARTBEAT_STALE_MS).expect("stale");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].id, "runner-a");

        // Fresh progress removes the alert even with the same heartbeat.
        store
            .set_runner_progress("runner-a", now)
            .expect("progress");
        assert!(stale_progress(store, now, HEARTBEAT_STALE_MS)
            .expect("fresh")
            .is_empty());
    }

    #[tokio::test]
    async fn daily_prune_task_sweeps_after_the_first_period() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            shanhaijing::Store::open(dir.path().join("daily.db")).expect("open"),
        );
        store
            .record_event(
                "fp",
                crate::now_ms() - 120 * 86_400_000,
                Some("r"),
                "system",
                "reported",
                "old",
            )
            .expect("old event");

        // A short period keeps this deterministic without tokio's test-util
        // feature: the first tick is consumed, so the first sweep lands after
        // one period.
        let task = spawn_daily_prune(
            Arc::clone(&store),
            retention_days_from_env(Some("90")),
            std::time::Duration::from_millis(20),
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let events = store
                .list_events(&Default::default(), 100)
                .expect("events");
            if events.is_empty() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "daily prune did not run within 5s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        task.abort();
    }

    #[test]
    fn event_retention_days_defaults_and_clamps() {
        assert_eq!(retention_days_from_env(None), 90);
        assert_eq!(retention_days_from_env(Some("garbage")), 90);
        assert_eq!(retention_days_from_env(Some("abc")), 90);
        assert_eq!(retention_days_from_env(Some("0")), 1);
        assert_eq!(retention_days_from_env(Some("-5")), 1);
        assert_eq!(retention_days_from_env(Some("4000")), 3_650);
        assert_eq!(retention_days_from_env(Some(" 30 ")), 30);
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

    /// 上报令牌不得升格为控制台令牌：它部署在每个被采集的服务上，回退等于
    /// 把"能造垃圾信号"的凭据变成"能读设置页 LLM key"的凭据。
    #[test]
    fn console_token_never_falls_back_to_the_collector_token() {
        let mut env = std::collections::BTreeMap::new();
        env.insert(
            "NUWA_INGEST_TOKEN".to_string(),
            "collector-secret".to_string(),
        );
        let tokens = serve_tokens(&env);
        assert_eq!(
            tokens.ingest.as_deref(),
            Some("collector-secret"),
            "the collector token is used for /ingest"
        );
        assert!(
            tokens.console.is_none(),
            "without NUWA_UI_TOKEN the console must be in local mode, not \
             authenticated by the collector token"
        );

        env.insert("NUWA_UI_TOKEN".to_string(), " console-secret ".to_string());
        let tokens = serve_tokens(&env);
        assert_eq!(tokens.console.as_deref(), Some("console-secret"));
        assert_eq!(tokens.ingest.as_deref(), Some("collector-secret"));
    }

    // ---- M2b Task 3: GET /api/v1/overview ----

    const OVERVIEW_REPO: &str = "example.com/app";
    const OVERVIEW_OTHER_REPO: &str = "example.com/other";

    struct OverviewSeed {
        now: i64,
        escalated_old: String,
        escalated_new: String,
        fixing_stuck: String,
        fixing_healthy: String,
        rejected_closed: String,
        rejected_ci_red: String,
        pr_open_recent: String,
        pr_open_stale: String,
        other_pr_open: String,
        merged: String,
        run_repair_issue: String,
        new_critical: String,
        new_high: String,
        new_medium: String,
        pr_url: String,
    }

    fn overview_signal(title: &str, repo: &str, severity: &str) -> shanhaijing::Signal {
        shanhaijing::Signal {
            repo: repo.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: severity.to_string(),
            message: Some(format!("boom {title}")),
            stack_trace: format!(
                "panic: boom {title}\ngoroutine 1 [running]:\nexample.com/app.Fn_{}()\n\t/app/{}.go:1 +0x1",
                title.replace(' ', "_"),
                title.replace(' ', "_"),
            ),
        }
    }

    fn overview_issue(
        store: &shanhaijing::Store,
        title: &str,
        repo: &str,
        severity: &str,
        at: i64,
    ) -> String {
        match store
            .report_signal(&overview_signal(title, repo, severity), at)
            .expect("report signal")
        {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{title} must be a new issue, got {other:?}"),
        }
    }

    fn overview_state(
        store: &shanhaijing::Store,
        fp: &str,
        state: shanhaijing::State,
        at: i64,
        detail: Option<&str>,
    ) {
        store.set_state(fp, state, at, detail).expect("set state");
    }

    fn overview_runner(
        store: &shanhaijing::Store,
        id: &str,
        repo: &str,
        status: &str,
        heartbeat_at: i64,
        now: i64,
    ) {
        store
            .upsert_runner(&shanhaijing::RunnerUpsert {
                id: id.to_string(),
                repo: repo.to_string(),
                host: "test-host".to_string(),
                pid: None,
                started_at: now - 600_000,
                heartbeat_at,
                status: status.to_string(),
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: r#"{"interval_secs":30}"#.to_string(),
            })
            .expect("upsert runner");
    }

    /// Seed one of each overview bucket: escalation prefixes, a stuck issue, a
    /// rejected-without-prefix issue, a recent and an expired `ci_timeout`,
    /// runners in every status (plus a stale heartbeat), active runs in two
    /// repos, and CI events inside/outside the 24h window.
    fn seed_overview(store: &shanhaijing::Store) -> OverviewSeed {
        use shanhaijing::State;

        let now = crate::now_ms();
        // Reports must land on today's UTC day so `reports_today` is exact even
        // when the test runs just after midnight.
        let day_start = now - now.rem_euclid(86_400_000);
        let base = day_start.max(now - 15_000);
        let at = |step: i64| base + step * 500;

        let escalated_old = overview_issue(store, "escalated old", OVERVIEW_REPO, "high", at(1));
        overview_state(store, &escalated_old, State::Triggered, at(1) + 1_000, None);
        overview_state(store, &escalated_old, State::Fixing, at(1) + 2_000, None);
        overview_state(
            store,
            &escalated_old,
            State::Escalated,
            at(1) + 3_000,
            Some("repair_failed: boom"),
        );

        let escalated_new = overview_issue(store, "escalated new", OVERVIEW_REPO, "medium", at(2));
        overview_state(store, &escalated_new, State::Triggered, at(2) + 1_000, None);
        overview_state(
            store,
            &escalated_new,
            State::Escalated,
            at(2) + 2_000,
            Some("budget_exceeded: 900 > 800"),
        );

        let fixing_stuck = overview_issue(store, "fixing stuck", OVERVIEW_REPO, "high", at(3));
        overview_state(store, &fixing_stuck, State::Triggered, now - 700_000, None);
        overview_state(store, &fixing_stuck, State::Fixing, now - 650_000, None);

        let fixing_healthy = overview_issue(store, "fixing healthy", OVERVIEW_REPO, "medium", at(4));
        overview_state(store, &fixing_healthy, State::Triggered, now - 2_000, None);
        overview_state(store, &fixing_healthy, State::Fixing, now - 1_000, None);

        let rejected_closed = overview_issue(store, "rejected closed", OVERVIEW_REPO, "high", at(5));
        let pr_at = day_start.max(now - 5_000);
        overview_state(store, &rejected_closed, State::Triggered, pr_at - 3_000, None);
        overview_state(store, &rejected_closed, State::Fixing, pr_at - 2_000, None);
        overview_state(store, &rejected_closed, State::PrOpen, pr_at - 1_000, None);
        let pr_url = "https://github.com/example/app/pull/42".to_string();
        store.set_pr(&rejected_closed, &pr_url, pr_at).expect("set pr");
        overview_state(
            store,
            &rejected_closed,
            State::Rejected,
            pr_at + 1_000,
            Some("PR closed without merge"),
        );

        let rejected_ci_red = overview_issue(store, "rejected ci red", OVERVIEW_REPO, "high", at(6));
        overview_state(store, &rejected_ci_red, State::Triggered, at(6) + 1_000, None);
        overview_state(store, &rejected_ci_red, State::Fixing, at(6) + 2_000, None);
        overview_state(store, &rejected_ci_red, State::PrOpen, at(6) + 3_000, None);
        overview_state(
            store,
            &rejected_ci_red,
            State::Rejected,
            at(6) + 4_000,
            Some("ci_red: CI red — PR #7 closed, back to owner"),
        );

        let pr_open_recent = overview_issue(store, "pr open recent", OVERVIEW_REPO, "medium", at(7));
        overview_state(store, &pr_open_recent, State::Triggered, at(7) + 1_000, None);
        overview_state(store, &pr_open_recent, State::Fixing, at(7) + 2_000, None);
        overview_state(store, &pr_open_recent, State::PrOpen, at(7) + 3_000, None);
        store
            .record_event(
                &pr_open_recent,
                now - 3_000,
                Some(OVERVIEW_REPO),
                "runner",
                "ci_timeout",
                "head-abc",
            )
            .expect("recent ci timeout event");

        let pr_open_stale = overview_issue(store, "pr open stale", OVERVIEW_REPO, "medium", at(8));
        overview_state(store, &pr_open_stale, State::Triggered, at(8) + 1_000, None);
        overview_state(store, &pr_open_stale, State::Fixing, at(8) + 2_000, None);
        overview_state(store, &pr_open_stale, State::PrOpen, at(8) + 3_000, None);
        store
            .record_event(
                &pr_open_stale,
                now - 90_000_000,
                Some(OVERVIEW_REPO),
                "runner",
                "ci_timeout",
                "head-old",
            )
            .expect("expired ci timeout event");

        let other_pr_open =
            overview_issue(store, "other pr open", OVERVIEW_OTHER_REPO, "high", at(9));
        overview_state(store, &other_pr_open, State::Triggered, at(9) + 1_000, None);
        overview_state(store, &other_pr_open, State::Fixing, at(9) + 2_000, None);
        overview_state(store, &other_pr_open, State::PrOpen, at(9) + 3_000, None);

        let _other_new = overview_issue(store, "other new", OVERVIEW_OTHER_REPO, "low", at(10));

        let merged = overview_issue(store, "merged issue", OVERVIEW_REPO, "low", at(11));
        overview_state(store, &merged, State::Triggered, at(11) + 1_000, None);
        overview_state(store, &merged, State::Fixing, at(11) + 2_000, None);
        overview_state(store, &merged, State::PrOpen, at(11) + 3_000, None);
        overview_state(
            store,
            &merged,
            State::Merged,
            at(11) + 4_000,
            Some("https://github.com/example/app/pull/9"),
        );

        let run_repair_issue =
            overview_issue(store, "other repairing", OVERVIEW_OTHER_REPO, "high", at(12));
        overview_state(store, &run_repair_issue, State::Triggered, now - 2_000, None);
        overview_state(store, &run_repair_issue, State::Fixing, now - 1_000, None);

        let new_critical = overview_issue(store, "new critical", OVERVIEW_REPO, "critical", at(13));
        let new_high = overview_issue(store, "new high", OVERVIEW_REPO, "high", at(14));
        let new_medium = overview_issue(store, "new medium", OVERVIEW_REPO, "medium", at(15));
        let _new_low = overview_issue(store, "new low", OVERVIEW_REPO, "low", at(16));

        // One runner per status plus a stale heartbeat: the stale runner's
        // `idle` status must not leak into the online status counts.
        overview_runner(store, "runner-fresh", OVERVIEW_REPO, "working", now, now);
        overview_runner(store, "runner-idle", OVERVIEW_REPO, "idle", now, now);
        overview_runner(store, "runner-standing", OVERVIEW_REPO, "standing_down", now, now);
        overview_runner(store, "runner-error", OVERVIEW_REPO, "error", now, now);
        overview_runner(store, "runner-stale", OVERVIEW_REPO, "idle", now - 200_000, now);
        overview_runner(store, "runner-other", OVERVIEW_OTHER_REPO, "working", now, now);

        store
            .start_run(&shanhaijing::RunStart {
                id: "run-ci".to_string(),
                fingerprint: fixing_healthy.clone(),
                runner_id: "runner-fresh".to_string(),
                repo: OVERVIEW_REPO.to_string(),
                started_at: now - 60_000,
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
            })
            .expect("start ci run");
        store
            .update_run(
                "run-ci",
                &shanhaijing::RunUpdate {
                    phase: Some("ci".to_string()),
                    turns: Some(6),
                    tokens_uncached: Some(17_000),
                    tokens_cached: Some(500),
                    ..Default::default()
                },
            )
            .expect("update ci run");
        store
            .start_run(&shanhaijing::RunStart {
                id: "run-repair".to_string(),
                fingerprint: run_repair_issue.clone(),
                runner_id: "runner-other".to_string(),
                repo: OVERVIEW_OTHER_REPO.to_string(),
                started_at: now - 120_000,
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
            })
            .expect("start repair run");
        store
            .update_run(
                "run-repair",
                &shanhaijing::RunUpdate {
                    phase: Some("repair".to_string()),
                    turns: Some(2),
                    tokens_uncached: Some(100),
                    tokens_cached: Some(50),
                    ..Default::default()
                },
            )
            .expect("update repair run");

        store
            .record_event(&merged, now - 1_000, Some(OVERVIEW_REPO), "runner", "ci_green", "head-green")
            .expect("ci green event");
        // Older than the rejection state change, so the state_changed event
        // (detail prefix `ci_red:`) stays the newest for the attention reason.
        store
            .record_event(
                &rejected_ci_red,
                at(6) + 500,
                Some(OVERVIEW_REPO),
                "runner",
                "ci_red",
                "head-red",
            )
            .expect("ci red event");
        // An other-repo CI event proves `ci_24h` follows the `?repo=` filter.
        store
            .record_event(
                &other_pr_open,
                now - 2_000,
                Some(OVERVIEW_OTHER_REPO),
                "runner",
                "ci_green",
                "other-green",
            )
            .expect("other ci green event");

        OverviewSeed {
            now,
            escalated_old,
            escalated_new,
            fixing_stuck,
            fixing_healthy,
            rejected_closed,
            rejected_ci_red,
            pr_open_recent,
            pr_open_stale,
            other_pr_open,
            merged,
            run_repair_issue,
            new_critical,
            new_high,
            new_medium,
            pr_url,
        }
    }

    async fn overview_request(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    fn overview_counts(value: &serde_json::Value) -> [u64; 8] {
        let counts = &value["counts"];
        [
            "new", "triggered", "fixing", "pr_open", "merged", "rejected", "escalated",
            "ignored",
        ]
        .map(|state| counts[state].as_u64().unwrap_or_else(|| panic!("counts.{state} missing")))
    }

    #[tokio::test]
    async fn overview_returns_counts_attention_runs_queue_health_and_ci() {
        let (_dir, state) = test_state(None);
        let seed = seed_overview(&state.store);
        let app = build_app(state);

        let response = overview_request(&app, "/api/v1/overview").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_common_security_headers(&response);
        assert_eq!(
            header_str(&response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert_eq!(header_str(&response, header::CACHE_CONTROL), "no-store");
        let value = json_body(response).await;

        // The state funnel keeps all eight keys, zeros included.
        assert_eq!(overview_counts(&value), [5, 0, 3, 3, 1, 2, 2, 0]);
        assert_eq!(value["counts"].as_object().expect("counts object").len(), 8);

        // Escalated > stuck > rejected > ci_timeout; same level by last_seen asc.
        let attention = value["attention"].as_array().expect("attention array");
        let expected = [
            (seed.escalated_old.as_str(), "escalated", "repair_failed", "修复 runner 失败，需要复核", false),
            (seed.escalated_new.as_str(), "escalated", "budget_exceeded", "token 预算超限，需要复核", false),
            (seed.fixing_stuck.as_str(), "fixing", "stuck", "疑似卡单，runner 心跳已过期", true),
            (seed.rejected_closed.as_str(), "rejected", "pr_closed", "PR 未合并被关闭", false),
            (seed.rejected_ci_red.as_str(), "rejected", "ci_red", "CI 红灯，PR 已关闭", false),
            (seed.pr_open_recent.as_str(), "pr_open", "ci_timeout", "CI 超时，需要人工查看", false),
        ];
        assert_eq!(attention.len(), expected.len(), "attention: {attention:?}");
        for (index, (fp, state, code, text, stuck)) in expected.iter().enumerate() {
            let item = &attention[index];
            assert_eq!(item["fingerprint"].as_str(), Some(*fp), "attention[{index}]");
            assert_eq!(item["state"].as_str(), Some(*state), "attention[{index}]");
            assert_eq!(item["reason_code"].as_str(), Some(*code), "attention[{index}]");
            assert_eq!(item["reason_text"].as_str(), Some(*text), "attention[{index}]");
            assert_eq!(item["stuck"].as_bool(), Some(*stuck), "attention[{index}]");
            assert!(item["last_seen_ms"].is_i64(), "attention[{index}]: {item}");
            assert!(item["title"].as_str().is_some_and(|title| !title.is_empty()));
            assert_eq!(item["repo"].as_str(), Some(OVERVIEW_REPO));
        }
        assert_eq!(attention[0]["owner_runner_id"], serde_json::Value::Null);
        assert_eq!(attention[3]["pr_url"].as_str(), Some(seed.pr_url.as_str()));
        assert!(
            attention
                .iter()
                .all(|item| item["fingerprint"].as_str() != Some(seed.pr_open_stale.as_str())),
            "a ci_timeout older than 24h must not be attention"
        );
        assert!(
            attention
                .iter()
                .all(|item| item["fingerprint"].as_str() != Some(seed.fixing_healthy.as_str())),
            "a healthy fixing issue must not be attention"
        );
        assert!(
            attention
                .iter()
                .all(|item| item["fingerprint"].as_str() != Some(seed.merged.as_str())),
            "a merged issue must not be attention"
        );

        // Active runs: newest first, titles resolved from the issue rows.
        let runs = value["active_runs"].as_array().expect("active runs");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["id"].as_str(), Some("run-ci"));
        assert_eq!(runs[0]["fingerprint"].as_str(), Some(seed.fixing_healthy.as_str()));
        assert_eq!(runs[0]["title"].as_str(), Some("fixing healthy"));
        assert_eq!(runs[0]["repo"].as_str(), Some(OVERVIEW_REPO));
        assert_eq!(runs[0]["runner_id"].as_str(), Some("runner-fresh"));
        assert_eq!(runs[0]["phase"].as_str(), Some("ci"));
        assert_eq!(runs[0]["started_at_ms"].as_i64(), Some(seed.now - 60_000));
        assert_eq!(runs[0]["turns"].as_i64(), Some(6));
        assert_eq!(runs[0]["tokens"].as_i64(), Some(17_500));
        assert_eq!(runs[1]["id"].as_str(), Some("run-repair"));
        assert_eq!(runs[1]["title"].as_str(), Some("other repairing"));
        assert_eq!(runs[1]["phase"].as_str(), Some("repair"));
        assert_eq!(runs[1]["tokens"].as_i64(), Some(150));

        // Queue: globally pickable issues, top three in claim order.
        assert_eq!(value["queue"]["ready_count"].as_u64(), Some(5));
        let next = value["queue"]["next"].as_array().expect("queue next");
        assert_eq!(next.len(), 3);
        assert_eq!(next[0]["fingerprint"].as_str(), Some(seed.new_critical.as_str()));
        assert_eq!(next[1]["fingerprint"].as_str(), Some(seed.new_high.as_str()));
        assert_eq!(next[2]["fingerprint"].as_str(), Some(seed.new_medium.as_str()));
        assert_eq!(next[0]["severity"].as_str(), Some("critical"));
        assert_eq!(next[0]["occurrences"].as_u64(), Some(1));
        assert!(next[0]["title"].as_str().is_some_and(|title| !title.is_empty()));

        // Health: the stale runner counts offline, not as idle.
        let health = &value["health"];
        assert_eq!(health["runners_total"].as_u64(), Some(6));
        assert_eq!(health["runners_online"].as_u64(), Some(5));
        assert_eq!(health["working"].as_u64(), Some(2));
        assert_eq!(health["idle"].as_u64(), Some(1));
        assert_eq!(health["standing_down"].as_u64(), Some(1));
        assert_eq!(health["error"].as_u64(), Some(1));
        assert_eq!(health["reports_today"].as_u64(), Some(16));
        assert_eq!(health["prs_today"].as_u64(), Some(1));

        // CI 24h counters: the expired ci_timeout is outside the window.
        let ci = &value["ci_24h"];
        assert_eq!(ci["green"].as_u64(), Some(2));
        assert_eq!(ci["red"].as_u64(), Some(1));
        assert_eq!(ci["timeout"].as_u64(), Some(1));
        assert_eq!(ci["pending"].as_u64(), Some(1));
    }

    #[tokio::test]
    async fn overview_repo_filter_scopes_issues_runners_and_runs_but_not_the_queue() {
        let (_dir, state) = test_state(None);
        let seed = seed_overview(&state.store);
        let app = build_app(state);

        let response = overview_request(&app, "/api/v1/overview?repo=example.com%2Fapp").await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(overview_counts(&value), [4, 0, 2, 2, 1, 2, 2, 0]);
        assert_eq!(value["attention"].as_array().expect("attention").len(), 6);
        let runs = value["active_runs"].as_array().expect("active runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"].as_str(), Some("run-ci"));
        // The queue is a global claim-order view; a repo filter must not
        // change what the runner pool will pick next.
        assert_eq!(value["queue"]["ready_count"].as_u64(), Some(5));
        assert_eq!(
            value["queue"]["next"][0]["fingerprint"].as_str(),
            Some(seed.new_critical.as_str())
        );
        let health = &value["health"];
        assert_eq!(health["runners_total"].as_u64(), Some(5));
        assert_eq!(health["runners_online"].as_u64(), Some(4));
        assert_eq!(health["working"].as_u64(), Some(1));
        assert_eq!(health["idle"].as_u64(), Some(1));
        assert_eq!(health["standing_down"].as_u64(), Some(1));
        assert_eq!(health["error"].as_u64(), Some(1));
        assert_eq!(
            health["reports_today"].as_u64(),
            Some(13),
            "reports_today follows the repo filter"
        );
        assert_eq!(health["prs_today"].as_u64(), Some(1));
        assert_eq!(
            value["ci_24h"]["green"].as_u64(),
            Some(1),
            "the other repo's ci_green must not leak into the scoped counts"
        );
        assert_eq!(value["ci_24h"]["timeout"].as_u64(), Some(1));
        assert_eq!(value["ci_24h"]["pending"].as_u64(), Some(1));

        let response = overview_request(&app, "/api/v1/overview?repo=example.com%2Fother").await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(overview_counts(&value), [1, 0, 1, 1, 0, 0, 0, 0]);
        assert!(value["attention"].as_array().expect("attention").is_empty());
        assert_eq!(value["attention"].as_array().expect("attention").len(), 0);
        let runs = value["active_runs"].as_array().expect("active runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"].as_str(), Some("run-repair"));
        assert_eq!(runs[0]["fingerprint"].as_str(), Some(seed.run_repair_issue.as_str()));
        assert_eq!(value["queue"]["ready_count"].as_u64(), Some(5));
        let health = &value["health"];
        assert_eq!(health["runners_total"].as_u64(), Some(1));
        assert_eq!(health["runners_online"].as_u64(), Some(1));
        assert_eq!(health["working"].as_u64(), Some(1));
        assert_eq!(health["idle"].as_u64(), Some(0));
        assert_eq!(
            health["reports_today"].as_u64(),
            Some(3),
            "only the other repo's reports count"
        );
        assert_eq!(health["prs_today"].as_u64(), Some(0));
        assert_eq!(value["ci_24h"]["green"].as_u64(), Some(1));
        assert_eq!(value["ci_24h"]["red"].as_u64(), Some(0));
        assert_eq!(value["ci_24h"]["timeout"].as_u64(), Some(0));

        // `?repo=` (blank) means no filter.
        let response = overview_request(&app, "/api/v1/overview?repo=").await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(overview_counts(&value), [5, 0, 3, 3, 1, 2, 2, 0]);
        assert_eq!(value["active_runs"].as_array().expect("active runs").len(), 2);
        assert_eq!(value["queue"]["ready_count"].as_u64(), Some(5));
        assert_eq!(value["health"]["runners_total"].as_u64(), Some(6));
        assert!(
            !seed.other_pr_open.is_empty(),
            "other repo PR issue is seeded"
        );
    }

    // ---- M2b Task 6: combined six-endpoint read-API smoke test ----

    const READ_API_REPO: &str = "example.com/read";

    struct ReadApiSeed {
        now: i64,
        /// First seeded timestamp; the rest are `base + step * 1s`.
        base: i64,
        /// UTC hour start containing `base`, for the aggregation fixture.
        hour: i64,
        escalated: String,
        merged: String,
        merged_pr_url: String,
        active: String,
        queued: String,
    }

    fn read_api_signal(title: &str, severity: &str) -> shanhaijing::Signal {
        shanhaijing::Signal {
            repo: READ_API_REPO.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: severity.to_string(),
            message: Some(format!("boom {title}")),
            stack_trace: format!(
                "panic: boom {title}\ngoroutine 1 [running]:\nexample.com/read.Fn_{}()\n\t/read/{}.go:1 +0x1",
                title.replace(' ', "_"),
                title.replace(' ', "_"),
            ),
        }
    }

    fn read_api_report(store: &shanhaijing::Store, title: &str, severity: &str, at: i64) -> String {
        match store
            .report_signal(&read_api_signal(title, severity), at)
            .expect("report signal")
        {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{title} must be a new issue, got {other:?}"),
        }
    }

    /// Seeds one issue per attention bucket the console cares about: an
    /// escalated issue, a merged issue with a run and a PR, an active fixing
    /// run owned by a live runner, and a queued `new` issue. Plus a runner
    /// heartbeat and an events-only fingerprint for aggregation.
    ///
    /// Every timestamp is pinned to the UTC day containing `now` (the same
    /// trick as `seed_overview`) so today-scoped counters are exact even when
    /// the test runs just after midnight.
    fn seed_read_api(store: &shanhaijing::Store) -> ReadApiSeed {
        use shanhaijing::{
            RunFinish, RunStart, RunUpdate, RunnerHeartbeat, RunnerUpsert, State,
        };

        let now = crate::now_ms();
        let day_start = now - now.rem_euclid(86_400_000);
        let base = day_start.max(now - 20_000);
        let at = |step: i64| base + step * 1_000;

        let escalated = read_api_report(store, "read escalated", "critical", at(0));
        overview_state(store, &escalated, State::Triggered, at(1), None);
        overview_state(store, &escalated, State::Fixing, at(2), None);
        overview_state(
            store,
            &escalated,
            State::Escalated,
            at(3),
            Some("repair_failed: boom"),
        );

        let merged = read_api_report(store, "read merged", "high", at(4));
        overview_state(store, &merged, State::Triggered, at(5), None);
        overview_state(store, &merged, State::Fixing, at(6), None);
        overview_state(store, &merged, State::PrOpen, at(7), None);
        let merged_pr_url = "https://github.com/example/read/pull/7".to_string();
        let pr_at = at(8);
        store
            .set_pr(&merged, &merged_pr_url, pr_at)
            .expect("set merged pr");
        overview_state(store, &merged, State::Merged, at(9), Some(&merged_pr_url));
        store
            .record_event(
                &merged,
                at(10),
                Some(READ_API_REPO),
                "system",
                "merged",
                &merged_pr_url,
            )
            .expect("merged event");
        store
            .start_run(&RunStart {
                id: "run-detail".to_string(),
                fingerprint: merged.clone(),
                runner_id: "runner-live".to_string(),
                repo: READ_API_REPO.to_string(),
                started_at: at(5),
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
            })
            .expect("start detail run");
        store
            .update_run(
                "run-detail",
                &RunUpdate {
                    turns: Some(6),
                    tokens_uncached: Some(17_000),
                    tokens_cached: Some(500),
                    verification_ok: Some(true),
                    verification_detail: Some("verification passed".to_string()),
                    change_stat: Some("1 file changed".to_string()),
                    branch: Some("agent/fix/read".to_string()),
                    commit_sha: Some("deadbeef".to_string()),
                    pr_url: Some(merged_pr_url.clone()),
                    pr_opened_at: Some(pr_at),
                    ci_verdict: Some("success".to_string()),
                    ..Default::default()
                },
            )
            .expect("update detail run");
        store
            .finish_run(
                "run-detail",
                &RunFinish {
                    finished_at: at(10),
                    outcome: "success".to_string(),
                    ci_verdict: Some("success".to_string()),
                    error: None,
                },
            )
            .expect("finish detail run");

        let active = read_api_report(store, "read active", "medium", at(11));
        overview_state(store, &active, State::Triggered, at(12), None);
        overview_state(store, &active, State::Fixing, at(13), None);
        store
            .start_run(&RunStart {
                id: "run-active".to_string(),
                fingerprint: active.clone(),
                runner_id: "runner-live".to_string(),
                repo: READ_API_REPO.to_string(),
                started_at: at(14),
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
            })
            .expect("start active run");
        store
            .update_run(
                "run-active",
                &RunUpdate {
                    phase: Some("repair".to_string()),
                    turns: Some(2),
                    tokens_uncached: Some(1_000),
                    tokens_cached: Some(100),
                    ..Default::default()
                },
            )
            .expect("update active run");

        let queued = read_api_report(store, "read queued", "low", at(15));

        // The merged issue's CI outcome events feed the overview 24h counters.
        store
            .record_event(
                &merged,
                at(16),
                Some(READ_API_REPO),
                "runner",
                "ci_green",
                "head-green",
            )
            .expect("ci green event");
        store
            .record_event(
                &merged,
                at(16),
                Some(READ_API_REPO),
                "runner",
                "ci_timeout",
                "head-timeout",
            )
            .expect("ci timeout event");

        // One live runner owning the active run; the effective config pins
        // every key the console renders.
        let config_json = serde_json::json!({
            "interval_secs": 600,
            "ci_timeout_secs": 900,
            "max_turns": 30,
            "max_tokens": 1_500_000,
            "failing_check": "go test ./...",
            "forbidden_paths_count": 4,
        })
        .to_string();
        store
            .upsert_runner(&RunnerUpsert {
                id: "runner-live".to_string(),
                repo: READ_API_REPO.to_string(),
                host: "read-host".to_string(),
                pid: Some(4242),
                started_at: now - 60_000,
                heartbeat_at: now,
                status: "working".to_string(),
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
                daily_pr_limit: Some(5),
                repo_path: Some("/tmp/read".to_string()),
                service_unit: Some("nuwa-read".to_string()),
                config_json,
            })
            .expect("upsert runner");
        store
            .heartbeat_runner(
                "runner-live",
                &RunnerHeartbeat {
                    heartbeat_at: now,
                    status: "working".to_string(),
                    current_fingerprint: Some(active.clone()),
                    phase: Some("repair".to_string()),
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("runner heartbeat");

        // Events-only fingerprint: three same-hour `reported` rows with an
        // interleaved other kind, plus one row in the next hour. `aggregate=1`
        // must group by fingerprint + UTC hour, not by adjacency.
        let hour = base - base.rem_euclid(3_600_000);
        for (offset, kind, detail) in [
            (60_000i64, "reported", "r0"),
            (120_000, "state_changed", "interleaved"),
            (180_000, "reported", "r1"),
            (240_000, "reported", "r2"),
            (3_600_000, "reported", "next-hour"),
        ] {
            store
                .record_event(
                    "read-api-events",
                    hour + offset,
                    Some(READ_API_REPO),
                    "system",
                    kind,
                    detail,
                )
                .expect("event fixture");
        }

        ReadApiSeed {
            now,
            base,
            hour,
            escalated,
            merged,
            merged_pr_url,
            active,
            queued,
        }
    }

    async fn read_api_get(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
            .await
            .expect("response")
    }

    fn assert_read_api_headers(response: &Response) {
        assert_common_security_headers(response);
        assert_eq!(
            header_str(response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert_eq!(header_str(response, header::CACHE_CONTROL), "no-store");
        assert!(
            header_str(response, header::CONTENT_TYPE).starts_with("application/json"),
            "read API responses must be JSON"
        );
    }

    fn read_api_fingerprints(value: &serde_json::Value) -> Vec<&str> {
        value["data"]
            .as_array()
            .expect("data array")
            .iter()
            .map(|item| item["fingerprint"].as_str().expect("fingerprint"))
            .collect()
    }

    /// One end-to-end walk over the six M2b read endpoints against a single
    /// seeded local-mode app: shapes, filters, pagination, 404, response
    /// headers, and cross-endpoint agreement (overview attention == filtered
    /// issue list == detail; overview runner health == runner list).
    #[tokio::test]
    async fn read_api_covers_all_six_endpoints_end_to_end() {
        let (_dir, state) = test_state(None);
        let seed = seed_read_api(&state.store);
        let app = build_app(state);

        // ---- 1. GET /overview ----
        let response = read_api_get(&app, "/api/v1/overview").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let overview = json_body(response).await;
        assert_eq!(overview_counts(&overview), [1, 0, 1, 0, 1, 0, 1, 0]);
        let attention = overview["attention"].as_array().expect("attention");
        assert_eq!(attention.len(), 1);
        assert_eq!(
            attention[0]["fingerprint"].as_str(),
            Some(seed.escalated.as_str())
        );
        assert_eq!(attention[0]["state"].as_str(), Some("escalated"));
        assert_eq!(attention[0]["severity"].as_str(), Some("critical"));
        assert_eq!(attention[0]["repo"].as_str(), Some(READ_API_REPO));
        assert_eq!(attention[0]["stuck"].as_bool(), Some(false));
        assert_eq!(attention[0]["owner_runner_id"], serde_json::Value::Null);
        assert_eq!(
            attention[0]["reason_code"].as_str(),
            Some("repair_failed")
        );
        assert_eq!(
            attention[0]["reason_text"].as_str(),
            Some("修复 runner 失败，需要复核")
        );
        assert_eq!(attention[0]["last_seen_ms"].as_i64(), Some(seed.base));
        let active_runs = overview["active_runs"].as_array().expect("active runs");
        assert_eq!(active_runs.len(), 1);
        assert_eq!(active_runs[0]["id"].as_str(), Some("run-active"));
        assert_eq!(
            active_runs[0]["fingerprint"].as_str(),
            Some(seed.active.as_str())
        );
        assert_eq!(active_runs[0]["title"].as_str(), Some("read active"));
        assert_eq!(active_runs[0]["runner_id"].as_str(), Some("runner-live"));
        assert_eq!(active_runs[0]["phase"].as_str(), Some("repair"));
        assert_eq!(active_runs[0]["turns"].as_i64(), Some(2));
        assert_eq!(active_runs[0]["tokens"].as_i64(), Some(1_100));
        assert_eq!(overview["queue"]["ready_count"].as_u64(), Some(1));
        assert_eq!(
            overview["queue"]["next"][0]["fingerprint"].as_str(),
            Some(seed.queued.as_str())
        );
        assert_eq!(overview["health"]["runners_total"].as_u64(), Some(1));
        assert_eq!(overview["health"]["runners_online"].as_u64(), Some(1));
        assert_eq!(overview["health"]["working"].as_u64(), Some(1));
        assert_eq!(overview["health"]["idle"].as_u64(), Some(0));
        assert_eq!(overview["health"]["reports_today"].as_u64(), Some(4));
        assert_eq!(overview["health"]["prs_today"].as_u64(), Some(1));
        assert_eq!(overview["ci_24h"]["green"].as_u64(), Some(1));
        assert_eq!(overview["ci_24h"]["red"].as_u64(), Some(0));
        assert_eq!(overview["ci_24h"]["timeout"].as_u64(), Some(1));
        assert_eq!(overview["ci_24h"]["pending"].as_u64(), Some(0));

        // ---- 2. GET /issues: filters + pagination ----
        let response = read_api_get(
            &app,
            "/api/v1/issues?repo=example.com%2Fread&page=1&page_size=2",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let page_one = json_body(response).await;
        assert_eq!(
            page_one["meta"],
            serde_json::json!({ "page": 1, "page_size": 2, "total": 4 })
        );
        assert_eq!(
            read_api_fingerprints(&page_one),
            vec![seed.queued.as_str(), seed.active.as_str()]
        );
        assert_eq!(page_one["data"][0]["title"].as_str(), Some("read queued"));
        assert_eq!(page_one["data"][0]["state"].as_str(), Some("new"));
        assert_eq!(page_one["data"][0]["severity"].as_str(), Some("low"));
        assert_eq!(page_one["data"][0]["occurrences"].as_u64(), Some(1));
        assert_eq!(page_one["data"][0]["stuck"].as_bool(), Some(false));
        assert_eq!(page_one["data"][0]["owner_runner_id"], serde_json::Value::Null);
        assert_eq!(page_one["data"][1]["state"].as_str(), Some("fixing"));
        assert_eq!(
            page_one["data"][1]["owner_runner_id"].as_str(),
            Some("runner-live")
        );
        assert_eq!(page_one["data"][1]["stuck"].as_bool(), Some(false));

        let response = read_api_get(
            &app,
            "/api/v1/issues?repo=example.com%2Fread&page=2&page_size=2",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page_two = json_body(response).await;
        assert_eq!(
            read_api_fingerprints(&page_two),
            vec![seed.merged.as_str(), seed.escalated.as_str()]
        );
        assert_eq!(page_two["data"][0]["state"].as_str(), Some("merged"));
        assert_eq!(
            page_two["data"][0]["pr_url"].as_str(),
            Some(seed.merged_pr_url.as_str())
        );

        let response = read_api_get(&app, "/api/v1/issues?page=3&page_size=2").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let page_three = json_body(response).await;
        assert!(page_three["data"].as_array().expect("data").is_empty());
        assert_eq!(page_three["meta"]["total"].as_u64(), Some(4));

        for (query, expected) in [
            (
                "attention=1",
                vec![seed.escalated.as_str()],
            ),
            ("state=escalated", vec![seed.escalated.as_str()]),
            ("severity=low", vec![seed.queued.as_str()]),
            ("q=merged", vec![seed.merged.as_str()]),
            ("severity=critical&language=go", vec![seed.escalated.as_str()]),
        ] {
            let uri = format!("/api/v1/issues?repo=example.com%2Fread&{query}");
            let response = read_api_get(&app, &uri).await;
            assert_eq!(response.status(), StatusCode::OK, "{query}");
            assert_eq!(
                read_api_fingerprints(&json_body(response).await),
                expected,
                "{query}"
            );
        }
        let response = read_api_get(&app, "/api/v1/issues?repo=example.com%2Fother").await;
        assert_eq!(response.status(), StatusCode::OK);
        let other_repo = json_body(response).await;
        assert!(other_repo["data"].as_array().expect("data").is_empty());
        assert_eq!(other_repo["meta"]["total"].as_u64(), Some(0));

        // ---- 3. GET /issues/{fp}: detail + 404 ----
        let response = read_api_get(&app, &format!("/api/v1/issues/{}", seed.merged)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let detail = json_body(response).await;
        assert_eq!(
            detail["issue"]["fingerprint"].as_str(),
            Some(seed.merged.as_str())
        );
        assert_eq!(detail["issue"]["state"].as_str(), Some("merged"));
        assert_eq!(
            detail["issue"]["pr_url"].as_str(),
            Some(seed.merged_pr_url.as_str())
        );
        assert_eq!(detail["issue"]["ci_verdict"].as_str(), Some("success"));
        assert!(detail["stack_sample"]
            .as_str()
            .is_some_and(|stack| stack.contains("read merged")));
        let timeline = detail["timeline"].as_array().expect("timeline");
        assert!(
            timeline
                .iter()
                .any(|item| item["kind"].as_str() == Some("merged")),
            "timeline must carry the terminal event: {timeline:?}"
        );
        assert!(
            timeline
                .iter()
                .any(|item| item["kind"].as_str() == Some("pr_opened")),
            "timeline must carry the PR event: {timeline:?}"
        );
        let runs = detail["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run["id"].as_str(), Some("run-detail"));
        assert_eq!(run["started_at_ms"].as_i64(), Some(seed.base + 5_000));
        assert_eq!(run["finished_at_ms"].as_i64(), Some(seed.base + 10_000));
        assert_eq!(run["phase"].as_str(), Some("done"));
        assert_eq!(run["outcome"].as_str(), Some("success"));
        assert_eq!(run["ci_verdict"].as_str(), Some("success"));
        assert_eq!(run["turns"].as_i64(), Some(6));
        assert_eq!(run["tokens_uncached"].as_i64(), Some(17_000));
        assert_eq!(run["tokens_cached"].as_i64(), Some(500));
        assert_eq!(run["branch"].as_str(), Some("agent/fix/read"));
        assert_eq!(run["pr_url"].as_str(), Some(seed.merged_pr_url.as_str()));
        assert_eq!(run["pr_opened_at_ms"].as_i64(), Some(seed.base + 8_000));
        assert_eq!(run["verification_ok"].as_bool(), Some(true));
        assert_eq!(detail["similar"]["count"].as_u64(), Some(0));
        assert!(detail["similar"]["items"]
            .as_array()
            .expect("similar items")
            .is_empty());

        // Cross-endpoint: the overview attention issue is the only
        // `attention=1` list row and its detail is the same escalated issue.
        let response = read_api_get(&app, &format!("/api/v1/issues/{}", seed.escalated)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let escalated_detail = json_body(response).await;
        assert_eq!(
            escalated_detail["issue"]["fingerprint"].as_str(),
            Some(seed.escalated.as_str())
        );
        assert_eq!(
            escalated_detail["issue"]["state"].as_str(),
            Some("escalated")
        );
        assert_eq!(
            escalated_detail["timeline"][0]["kind"].as_str(),
            Some("state_changed")
        );
        assert_eq!(
            escalated_detail["timeline"][0]["detail"].as_str(),
            Some("repair_failed: boom")
        );

        let response = read_api_get(&app, "/api/v1/issues/does-not-exist").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_read_api_headers(&response);
        let missing = json_body(response).await;
        assert_eq!(missing["error"]["code"].as_str(), Some("not_found"));
        assert_eq!(missing["error"]["message"].as_str(), Some("issue not found"));

        // ---- 4. GET /runners ----
        let response = read_api_get(&app, "/api/v1/runners").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let runners = json_body(response).await;
        let runner_list = runners["data"].as_array().expect("data");
        assert_eq!(runner_list.len(), 1);
        // Cross-endpoint: overview health and the fleet list agree.
        assert_eq!(
            overview["health"]["runners_total"].as_u64(),
            Some(runner_list.len() as u64)
        );
        let runner = &runner_list[0];
        assert_eq!(runner["id"].as_str(), Some("runner-live"));
        assert_eq!(runner["repo"].as_str(), Some(READ_API_REPO));
        assert_eq!(runner["host"].as_str(), Some("read-host"));
        assert_eq!(runner["pid"].as_i64(), Some(4242));
        assert_eq!(runner["started_at_ms"].as_i64(), Some(seed.now - 60_000));
        assert_eq!(runner["heartbeat_at_ms"].as_i64(), Some(seed.now));
        assert_eq!(runner["status"].as_str(), Some("working"));
        assert_eq!(runner["offline"].as_bool(), Some(false));
        assert_eq!(
            runner["current_fingerprint"].as_str(),
            Some(seed.active.as_str())
        );
        assert_eq!(runner["phase"].as_str(), Some("repair"));
        assert_eq!(runner["provider"].as_str(), Some("faux"));
        assert_eq!(runner["model"].as_str(), Some("faux"));
        assert_eq!(runner["daily_pr_limit"].as_i64(), Some(5));
        assert_eq!(runner["prs_today"].as_u64(), Some(1));
        assert_eq!(runner["repo_path"].as_str(), Some("/tmp/read"));
        assert_eq!(runner["service_unit"].as_str(), Some("nuwa-read"));
        assert_eq!(runner["last_error"], serde_json::Value::Null);
        assert_eq!(
            runner["effective"],
            serde_json::json!({
                "interval_secs": 600,
                "ci_timeout_secs": 900,
                "max_turns": 30,
                "max_tokens": 1_500_000,
                "failing_check": "go test ./...",
                "forbidden_paths_count": 4,
            })
        );

        // ---- 5. GET /events: filters, pagination, aggregation ----
        let response = read_api_get(
            &app,
            &format!(
                "/api/v1/events?fingerprint={}&kind=state_changed&page=1&page_size=2",
                seed.escalated
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let events = json_body(response).await;
        assert_eq!(
            events["meta"],
            serde_json::json!({ "page": 1, "page_size": 2, "total": 3 })
        );
        let rows = events["data"].as_array().expect("data");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ts_ms"].as_i64(), Some(seed.base + 3_000));
        assert_eq!(rows[0]["detail"].as_str(), Some("repair_failed: boom"));
        assert_eq!(rows[0]["repo"].as_str(), Some(READ_API_REPO));
        assert_eq!(rows[0]["actor"].as_str(), Some("system"));
        assert_eq!(rows[0]["count"].as_u64(), Some(1));
        assert_eq!(rows[0]["first_ts_ms"], rows[0]["ts_ms"]);
        assert_eq!(rows[0]["last_ts_ms"], rows[0]["ts_ms"]);
        assert_eq!(rows[1]["ts_ms"].as_i64(), Some(seed.base + 2_000));

        let response = read_api_get(
            &app,
            &format!(
                "/api/v1/events?fingerprint={}&kind=state_changed&page=2&page_size=2",
                seed.escalated
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let page_two_events = json_body(response).await;
        let rows = page_two_events["data"].as_array().expect("data");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["ts_ms"].as_i64(), Some(seed.base + 1_000));

        // Without aggregation the same fingerprint yields all five fixture
        // rows; `aggregate=1` groups the three same-hour `reported` rows even
        // though a `state_changed` row interleaves between them.
        let response =
            read_api_get(&app, "/api/v1/events?fingerprint=read-api-events").await;
        assert_eq!(response.status(), StatusCode::OK);
        let raw_events = json_body(response).await;
        assert_eq!(raw_events["meta"]["total"].as_u64(), Some(5));

        let response =
            read_api_get(&app, "/api/v1/events?fingerprint=read-api-events&aggregate=1").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let aggregated = json_body(response).await;
        assert_eq!(
            aggregated["meta"],
            serde_json::json!({ "page": 1, "page_size": 50, "total": 3 })
        );
        let rows = aggregated["data"].as_array().expect("data");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["kind"].as_str(), Some("reported"));
        assert_eq!(rows[0]["detail"].as_str(), Some("next-hour"));
        assert_eq!(rows[0]["count"].as_u64(), Some(1));
        assert_eq!(rows[1]["kind"].as_str(), Some("reported"));
        assert_eq!(rows[1]["detail"].as_str(), Some("r2"));
        assert_eq!(rows[1]["count"].as_u64(), Some(3));
        assert_eq!(rows[1]["ts_ms"].as_i64(), Some(seed.hour + 240_000));
        assert_eq!(
            rows[1]["first_ts_ms"].as_i64(),
            Some(seed.hour + 60_000)
        );
        assert_eq!(
            rows[1]["last_ts_ms"].as_i64(),
            Some(seed.hour + 240_000)
        );
        assert_eq!(rows[2]["kind"].as_str(), Some("state_changed"));
        assert_eq!(rows[2]["count"].as_u64(), Some(1));
        assert_eq!(rows[2]["ts_ms"].as_i64(), Some(seed.hour + 120_000));

        // ---- 6. GET /metrics/trends ----
        let response = read_api_get(
            &app,
            "/api/v1/metrics/trends?range=7d&repo=example.com%2Fread",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_read_api_headers(&response);
        let trends = json_body(response).await;
        assert_eq!(trends["range"].as_str(), Some("7d"));
        let buckets = trends["buckets"].as_array().expect("buckets");
        assert_eq!(buckets.len(), 7);
        assert_eq!(
            buckets[6],
            serde_json::json!({
                "day": shanhaijing::utc_day(seed.base),
                "reports": 4,
                "new_issues": 4,
                "prs_opened": 1,
                "merged": 1,
                "rejected": 0,
                "tokens": 18_600,
                "ci_green": 1,
                "ci_red": 0,
            })
        );
        assert_eq!(
            trends["totals"],
            serde_json::json!({
                "reports": 4,
                "new_issues": 4,
                "prs_opened": 1,
                "merged": 1,
                "rejected": 0,
                "tokens": 18_600,
                "ci_green": 1,
                "ci_red": 0,
                "median_time_to_pr_secs": 3,
                "false_positive_rate": 0.0,
                "review_latency_p50_secs": 2,
                "ci_red_rate": 0.0,
            })
        );

        let response = read_api_get(&app, "/api/v1/metrics/trends?range=1d").await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_read_api_headers(&response);
        let invalid = json_body(response).await;
        assert_eq!(invalid["error"]["code"].as_str(), Some("validation_failed"));
        assert!(
            invalid["error"]["fields"]["range"].is_string(),
            "invalid range must name the field: {invalid}"
        );
    }

    /// The architecture diagram is the one document the console frames: it
    /// alone gets same-origin framing plus the inline script/style its
    /// self-contained artifact ships with.
    #[test]
    fn only_the_architecture_page_gets_the_frameable_inline_policy() {
        let (xfo, csp) = frame_policy_for("/architecture.html");
        assert_eq!(xfo, "SAMEORIGIN");
        let csp = csp.to_str().expect("ascii header");
        assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
        assert!(csp.contains("script-src 'self' 'unsafe-inline'"), "{csp}");
        assert!(csp.contains("style-src 'self' 'unsafe-inline'"), "{csp}");
        // Framing stays same-origin only: foreign sites still cannot embed it.
        assert!(!csp.contains("frame-ancestors *"), "{csp}");
    }

    #[test]
    fn every_other_route_keeps_the_strict_no_frame_policy() {
        for path in [
            "/",
            "/architecture",
            "/architecture.htmlx",
            "/Architecture.html",
            "/api/v1/overview",
            "/settings",
        ] {
            let (xfo, csp) = frame_policy_for(path);
            assert_eq!(xfo, "DENY", "{path}");
            assert_eq!(csp, CSP_STRICT, "{path}");
        }
    }
}
