# Web 控制台 M2a 会话鉴权与 API 路由 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立 `/api/v1` 的路由与鉴权地基：HMAC 会话 Cookie、CSRF 双提交、登录限速、安全响应头、静态资源占位与旧控制台共存；为 M2b/M2c 的只读/动作/设置端点提供可测试的 AppState 与错误契约。

**Architecture:** 新增 `session.rs`（签名/CSRF/限速）、`api/mod.rs`（错误契约/路由）、`serve.rs`（完整 app 组装）、`web.rs`（rust-embed 占位与 SPA fallback）。旧 `/ui`/`/settings` Basic 路由继续存在到 M6；新 API 使用会话 Cookie。

**Tech Stack:** axum 0.8、axum-extra（cookie）、hmac/sha2/subtle、rust-embed、mime_guess、tower（测试）、serde。

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md) §6、§9.1–9.2（认证与约定）、§13（配置源）；ADR-009。

## Global Constraints

- 自建 crate 零 clippy warning。
- 会话密钥由 UI token 派生；UI token 为空时进入本地模式（仍要求 CSRF）。
- Cookie：`nuwa_session` HttpOnly/SameSite=Strict/Path=/；`nuwa_csrf` 非 HttpOnly；`Secure` 按 env/`X-Forwarded-Proto`。
- 除 `/auth/login` 外，所有非 GET API 请求要求 `nuwa_csrf` Cookie + `X-NUWA-CSRF` 头且同源。
- 登录失败每 IP 5 次/10 分钟；常量时间比较。
- 安全头：API/SPA 严格 CSP；旧 `/ui` 允许 `style-src 'self' 'unsafe-inline'`（旧页内联 CSS），M6 删除旧页后收紧。
- 不修改 Store API、迁移、vendored crates、guard/deliver。
- 每个任务一个 commit；不 push/tag/deploy。

---

### Task 1: 会话签名、CSRF 与登录限速

**Files:**
- Create: `crates/nuwa/src/session.rs`
- Modify: `crates/nuwa/src/main.rs`（`mod session;`）
- Modify: `Cargo.toml` / `crates/nuwa/Cargo.toml`（hmac/subtle/sha2/axum-extra）
- Test: `session.rs` 内嵌 tests

**Interfaces:**
- `SessionManager::new(ui_token: &str, ttl_ms: i64) -> Self`
- `issue(now_ms) -> IssuedSession { token, csrf, expires_at_ms }`
- `verify(token, now_ms) -> Option<i64>`（返回过期时间）
- `token_eq(got, expected) -> bool`（常量时间）
- `LoginLimiter::{new,check,failure,success}`
- `origin_ok(origin, referer, host) -> bool`

- [ ] **Step 1: 依赖**

在 workspace `Cargo.toml` 的 `[workspace.dependencies]` 增加：

```toml
hmac = "0.12"
subtle = "2"
axum-extra = { version = "0.10", features = ["cookie"] }
rust-embed = "8"
mime_guess = "2"
```

`crates/nuwa/Cargo.toml` 增加：`hmac`, `subtle`, `sha2`, `axum-extra`, `rust-embed`, `mime_guess`（workspace 引用）。测试 dev-deps 增加 `tower = { version = "0.5", features = ["util"] }`、`http-body-util = "0.1"`。

- [ ] **Step 2: 写失败测试**

在 `session.rs` 内嵌：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn session_round_trip_and_tamper() {
        let m = SessionManager::new("ui-secret", 60_000);
        let issued = m.issue(1_000);
        assert_eq!(m.verify(&issued.token, 1_001), Some(61_000));
        assert!(m.verify(&issued.token, 61_000).is_none(), "expired");
        let mut tampered = issued.token.clone();
        tampered.push('x');
        assert!(m.verify(&tampered, 1_001).is_none());
    }

    #[test]
    fn token_compare_is_exact() {
        assert!(SessionManager::token_eq("abc", "abc"));
        assert!(!SessionManager::token_eq("abc", "abd"));
        assert!(!SessionManager::token_eq("abc", "abcd"));
    }

    #[test]
    fn limiter_blocks_after_five_failures_and_clears_on_success() {
        let l = LoginLimiter::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for i in 0..5 {
            assert!(l.check(ip, 1_000 + i).is_ok());
            l.failure(ip, 1_000 + i);
        }
        assert!(l.check(ip, 1_006).is_err());
        assert!(l.check(ip, 700_000).is_ok(), "window expired");
        l.failure(ip, 700_001);
        l.success(ip);
        assert!(l.check(ip, 700_002).is_ok());
    }

    #[test]
    fn origin_ok_rules() {
        assert!(origin_ok(None, None, Some("h:1")));
        assert!(origin_ok(Some("https://h:1"), None, Some("h:1")));
        assert!(!origin_ok(Some("https://evil"), None, Some("h:1")));
        assert!(!origin_ok(Some("https://h:1"), None, None));
    }
}
```

- [ ] **Step 3: 实现**

`session.rs`:

```rust
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MAX_LOGIN_FAILURES: u32 = 5;
pub const LOGIN_WINDOW_MS: i64 = 10 * 60 * 1000;

pub struct SessionManager {
    key: [u8; 32],
    ttl_ms: i64,
}

pub struct IssuedSession {
    pub token: String,
    pub csrf: String,
    pub expires_at_ms: i64,
}

impl SessionManager {
    pub fn new(ui_token: &str, ttl_ms: i64) -> Self {
        let key: [u8; 32] = Sha256::digest(ui_token.as_bytes()).into();
        Self { key, ttl_ms }
    }

    pub fn issue(&self, now_ms: i64) -> IssuedSession {
        let expires_at_ms = now_ms + self.ttl_ms;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let payload = format!("v1.{expires_at_ms}.{nonce}");
        let token = format!("{payload}.{}", self.sign(&payload));
        let csrf = uuid::Uuid::new_v4().simple().to_string();
        IssuedSession { token, csrf, expires_at_ms }
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
        mac.update(payload.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    pub fn verify(&self, token: &str, now_ms: i64) -> Option<i64> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 4 || parts[0] != "v1" {
            return None;
        }
        let expires_at_ms: i64 = parts[1].parse().ok()?;
        if expires_at_ms <= now_ms {
            return None;
        }
        let payload = format!("v1.{}.{}", parts[1], parts[2]);
        let expected = self.sign(&payload);
        if !Self::token_eq(&expected, parts[3]) {
            return None;
        }
        Some(expires_at_ms)
    }

    pub fn token_eq(got: &str, expected: &str) -> bool {
        got.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1
    }

    pub fn ttl_ms(&self) -> i64 {
        self.ttl_ms
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct Bucket {
    failures: u32,
    reset_at_ms: i64,
}

pub struct LoginLimiter {
    inner: Mutex<HashMap<IpAddr, Bucket>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub fn check(&self, ip: IpAddr, now_ms: i64) -> Result<(), i64> {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(b) = map.get(&ip) {
            if now_ms < b.reset_at_ms && b.failures >= MAX_LOGIN_FAILURES {
                return Err(b.reset_at_ms - now_ms);
            }
            if now_ms >= b.reset_at_ms {
                map.remove(&ip);
            }
        }
        Ok(())
    }

    pub fn failure(&self, ip: IpAddr, now_ms: i64) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let bucket = map.entry(ip).or_insert(Bucket {
            failures: 0,
            reset_at_ms: now_ms + LOGIN_WINDOW_MS,
        });
        if now_ms >= bucket.reset_at_ms {
            bucket.failures = 0;
            bucket.reset_at_ms = now_ms + LOGIN_WINDOW_MS;
        }
        bucket.failures += 1;
    }

    pub fn success(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(&ip);
    }
}

fn authority(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let end = rest.find(|c| matches!(c, '/' | '?' | '#')).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() { None } else { Some(host) }
}

pub fn origin_ok(origin: Option<&str>, referer: Option<&str>, host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let candidate = match origin {
        Some(o) => authority(o),
        None => referer.and_then(authority),
    };
    match candidate {
        Some(h) => h.eq_ignore_ascii_case(host),
        None => origin.is_none() && referer.is_none(),
    }
}
```

在 `main.rs` 顶部加 `mod session;`。

- [ ] **Step 4: 运行测试**

Run: `cargo test -p nuwa session::tests -- --nocapture`
Expected: 4 tests PASS.

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock crates/nuwa/Cargo.toml crates/nuwa/src/session.rs crates/nuwa/src/main.rs
git commit -m "feat(auth): HMAC sessions, CSRF tokens and login rate limiting"
```

---

### Task 2: API 错误契约与 AppState

**Files:**
- Create: `crates/nuwa/src/api/mod.rs`
- Modify: `crates/nuwa/src/main.rs`（`mod api;`）
- Test: `api/mod.rs` tests

**Interfaces:**
- `AppState { store, ui_token, ingest_token, session, limiter, started_at_ms, cookie_secure }`（放在 `serve.rs` 或 `api/mod.rs`；本计划放在 `serve.rs`，`api` 使用 `crate::serve::AppState`）。
- `ApiError { status, code, message, fields }` + `IntoResponse`，错误体 `{ "error": { "code", "message", "fields"? } }`。
- `pub fn json_error(status, code, message) -> ApiError`。

- [ ] **Step 1: 测试**

```rust
#[test]
fn error_serializes_shape() {
    let body = ErrorBody::from(ApiError::not_found("issue not found"));
    let value = serde_json::to_value(body).expect("json");
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(value["error"]["message"], "issue not found");
}
```

- [ ] **Step 2: 实现**

`api/mod.rs`:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub fields: Option<BTreeMap<String, String>>,
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, String>>,
}

impl From<ApiError> for ErrorBody {
    fn from(e: ApiError) -> Self {
        Self {
            error: ErrorDetail {
                code: e.code,
                message: e.message,
                fields: e.fields,
            },
        }
    }
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), fields: None }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "invalid_state", message)
    }
    pub fn validation(fields: BTreeMap<String, String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_failed",
            message: "validation failed".into(),
            fields: Some(fields),
        }
    }
    pub fn rate_limited(retry_after_ms: i64) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!("retry after {retry_after_ms}ms"),
        )
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "upstream_error", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(ErrorBody::from(self))).into_response()
    }
}

pub fn api_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "unknown API endpoint")
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test -p nuwa api::tests -- --nocapture`
Expected: PASS.

- [ ] **Step 4: 提交**

```bash
git add crates/nuwa/src/api/mod.rs crates/nuwa/src/main.rs
git commit -m "feat(api): unified JSON error contract"
```

---

### Task 3: serve.rs、AppState、健康检查与旧路由共存

**Files:**
- Create: `crates/nuwa/src/serve.rs`
- Create: `crates/nuwa/src/web.rs`（stub，Task 6 扩展）
- Modify: `crates/nuwa/src/api/mod.rs`（增加空的 API router stub，Task 4 扩展）
- Modify: `crates/nuwa/src/main.rs`（`mod serve; mod web;`，`Some("serve") => serve::run_serve(...)`，删除原 `run_serve`/`check_auth`/`ingest_endpoint` 或改为 `pub(crate)`供 serve 使用）
- Test: `serve.rs` 内嵌 tests（健康检查/旧路由 401/静态占位/API 404）

**Interfaces:**
- `#[derive(Clone)] pub struct AppState { pub store: Arc<Store>, pub ui_token: Arc<Option<String>>, pub ingest_token: Arc<Option<String>>, pub session: Arc<SessionManager>, pub limiter: Arc<LoginLimiter>, pub started_at_ms: i64, pub cookie_secure: bool }`
- `pub fn build_app(state: AppState) -> Router`（完整 app，含 legacy + `/api/v1` + static fallback）
- `pub async fn run_serve(args: Vec<String>) -> anyhow::Result<()>`

- [ ] **Step 1: 移动 serve 代码**

把 `main.rs` 的 `run_serve`、`check_auth`、`ingest_endpoint` 移到 `serve.rs`；`main.rs` 的 `run_serve` 调用改为 `serve::run_serve`。`parse_signals`/`report_one` 改为 `pub(crate)` 供 serve 使用；`now_ms` 改为 `pub(crate)`。

`serve.rs` 读取：

```rust
let ingest_token = Arc::new(std::env::var("NUWA_INGEST_TOKEN").ok().filter(|t| !t.is_empty()));
let ui_token = std::env::var("NUWA_UI_TOKEN").ok().filter(|t| !t.is_empty())
    .map(|t| Arc::new(Some(t)) as Arc<Option<String>>)
    .unwrap_or_else(|| Arc::clone(&ingest_token));
let ttl_secs: i64 = std::env::var("NUWA_SESSION_TTL_SECS").ok()
    .and_then(|v| v.parse().ok()).unwrap_or(86_400);
let cookie_secure = std::env::var("NUWA_COOKIE_SECURE").ok().as_deref() == Some("always");
let session = Arc::new(crate::session::SessionManager::new(
    ui_token.as_deref().unwrap_or(""), ttl_secs * 1000,
));
let state = AppState { store: Arc::clone(&store), ui_token: Arc::clone(&ui_token), ingest_token: Arc::clone(&ingest_token), session, limiter: Arc::new(crate::session::LoginLimiter::new()), started_at_ms: crate::now_ms(), cookie_secure };
```

- [ ] **Step 2: build_app**

```rust
pub fn build_app(state: AppState) -> axum::Router {
    let legacy_state = (Arc::clone(&state.store), Arc::clone(&state.ui_token));
    let legacy_auth = axum::middleware::from_fn_with_state(
        (Arc::clone(&state.store), Arc::clone(&state.ingest_token)),
        check_auth,
    );
    let ui_auth = axum::middleware::from_fn_with_state(
        (Arc::clone(&state.store), Arc::clone(&state.ui_token)),
        crate::ui::ui_auth,
    );
    let legacy = axum::Router::new()
        .route("/ingest", axum::routing::post(ingest_endpoint).route_layer(legacy_auth))
        .route("/health", axum::routing::get(|| async { "nuwa serve ok" }))
        .route("/ui", axum::routing::get(crate::ui::dashboard))
        .route("/ui/reopen", axum::routing::post(crate::ui::reopen))
        .route("/ui/sync", axum::routing::post(crate::ui::sync_now))
        .route("/issue/{fp}", axum::routing::get(crate::ui::issue_detail))
        .route("/settings", axum::routing::get(crate::settings::get_settings))
        .route("/settings/save", axum::routing::post(crate::settings::save_settings))
        .route("/settings/restart", axum::routing::post(crate::settings::restart_services))
        .route_layer(ui_auth)
        .route_layer(axum::middleware::from_fn(crate::ui::origin_guard))
        .with_state(legacy_state);

    let api = crate::api::router(Arc::new(state.clone()));
    axum::Router::new()
        .merge(legacy)
        .nest("/api/v1", api)
        .fallback(crate::web::serve)
}
```

Task 3 does not add `security_headers` yet; Task 5 adds the layer after defining the middleware. For Task 3, create a minimal `web.rs`:

```rust
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn serve() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))],
        "<!doctype html><html lang=\"zh\"><meta charset=\"utf-8\"><title>Nuwa</title><body><h1>Nuwa console</h1><p>frontend not built yet</p></body></html>",
    )
        .into_response()
}
```

And add an empty API router stub to `api/mod.rs` (Task 4 replaces it):

```rust
pub fn router(state: std::sync::Arc<crate::serve::AppState>) -> axum::Router {
    axum::Router::new()
        .fallback(|| async { api_not_found() })
        .with_state((*state).clone())
}
```

- [ ] **Step 3: 测试**

`serve.rs` tests:
- `build_app` with a temp DB and `ui_token=Some("secret")`; `oneshot` GET `/health` → 200.
- API unknown `/api/v1/nope` → 404 JSON (empty API stub fallback; auth 401 is tested in Task 4).
- Legacy `/ui` without Basic → 401.
- Static `/` → 200 placeholder.

- [ ] **Step 4: 提交**

```bash
git add crates/nuwa/src/serve.rs crates/nuwa/src/main.rs crates/nuwa/src/web.rs
git commit -m "refactor(serve): app assembly with API and legacy routers"
```

---

### Task 4: 登录/退出/me、会话与 CSRF 中间件

**Files:**
- Create: `crates/nuwa/src/api/auth.rs`
- Modify: `crates/nuwa/src/api/mod.rs`（router）
- Modify: `crates/nuwa/src/session.rs`（`require_session`/`csrf_guard`）
- Test: `api/auth.rs` tests + `serve.rs` integration

**Interfaces:**
- `LoginRequest { token: String }`
- `POST /api/v1/auth/login` → 204 + cookies
- `POST /api/v1/auth/logout` → 204 + clear cookies（受会话/CSRF 保护）
- `GET /api/v1/auth/me` → `{ authenticated, local_mode, expires_at_ms }`

- [ ] **Step 1: 实现 cookie 助手与 handlers**

`api/auth.rs`:

```rust
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

use crate::api::ApiError;
use crate::serve::AppState;
use crate::session::{now_ms, SessionManager};

#[derive(Deserialize)]
pub struct LoginRequest { pub token: String }

#[derive(Serialize)]
pub struct MeResponse {
    pub authenticated: bool,
    pub local_mode: bool,
    pub expires_at_ms: Option<i64>,
}

fn secure_cookie(state: &AppState, headers: &HeaderMap) -> bool {
    state.cookie_secure
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            == Some("https")
}

fn session_cookie(token: String, ttl_ms: i64, secure: bool) -> Cookie<'static> {
    Cookie::build(("nuwa_session", token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(Duration::from_millis(ttl_ms as u64))
        .secure(secure)
        .build()
}

fn csrf_cookie(token: String, ttl_ms: i64, secure: bool) -> Cookie<'static> {
    Cookie::build(("nuwa_csrf", token))
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(Duration::from_millis(ttl_ms as u64))
        .secure(secure)
        .build()
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> Response {
    let now = now_ms();
    if let Err(retry) = state.limiter.check(peer.ip(), now) {
        return ApiError::rate_limited(retry).into_response();
    }
    let expected = state.ui_token.as_deref().unwrap_or("");
    if !expected.is_empty() && !SessionManager::token_eq(&req.token, expected) {
        state.limiter.failure(peer.ip(), now);
        return ApiError::unauthorized("invalid token").into_response();
    }
    state.limiter.success(peer.ip());
    let issued = state.session.issue(now);
    let ttl = state.session.ttl_ms();
    let secure = secure_cookie(&state, &headers);
    let jar = jar
        .add(session_cookie(issued.token, ttl, secure))
        .add(csrf_cookie(issued.csrf, ttl, secure));
    (jar, StatusCode::NO_CONTENT).into_response()
}

pub async fn logout(jar: CookieJar) -> Response {
    let jar = jar
        .remove(Cookie::from("nuwa_session"))
        .remove(Cookie::from("nuwa_csrf"));
    (jar, StatusCode::NO_CONTENT).into_response()
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Response {
    let now = now_ms();
    let local_mode = state.ui_token.is_none();
    let expires_at_ms = if local_mode {
        None
    } else {
        let Some(cookie) = jar.get("nuwa_session") else {
            return ApiError::unauthorized("not logged in").into_response();
        };
        match state.session.verify(cookie.value(), now) {
            Some(exp) => Some(exp),
            None => return ApiError::unauthorized("session expired").into_response(),
        }
    };
    let mut jar = jar;
    if jar.get("nuwa_csrf").is_none() {
        let secure = secure_cookie(&state, &headers);
        jar = jar.add(csrf_cookie(
            uuid::Uuid::new_v4().simple().to_string(),
            state.session.ttl_ms(),
            secure,
        ));
    }
    (jar, Json(MeResponse { authenticated: true, local_mode, expires_at_ms })).into_response()
}
```

在 `SessionManager` 增加 `pub fn ttl_ms(&self) -> i64 { self.ttl_ms }` 和 `pub fn now_ms() -> i64`（或 `session::now_ms`）使用系统时间：

```rust
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 2: 会话/CSRF 中间件**

在 `session.rs` 增加（替代 Task 5 中的同名实现；Task 5 只做安全头与测试）：

```rust
pub async fn require_session(
    State(state): State<Arc<crate::serve::AppState>>,
    jar: CookieJar,
    req: Request,
    next: Next,
) -> Response {
    if state.ui_token.is_none() {
        return next.run(req).await;
    }
    let Some(cookie) = jar.get("nuwa_session") else {
        return crate::api::ApiError::unauthorized("not logged in").into_response();
    };
    if state.session.verify(cookie.value(), now_ms()).is_none() {
        return crate::api::ApiError::unauthorized("session expired").into_response();
    }
    next.run(req).await
}

pub async fn csrf_guard(
    State(_state): State<Arc<crate::serve::AppState>>,
    headers: HeaderMap,
    jar: CookieJar,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let cookie = jar.get("nuwa_csrf").map(|c| c.value()).unwrap_or("");
    let header = headers.get("x-nuwa-csrf").and_then(|v| v.to_str().ok()).unwrap_or("");
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let referer = headers.get(header::REFERER).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if cookie.is_empty() || !SessionManager::token_eq(cookie, header) || !origin_ok(origin, referer, host) {
        return crate::api::ApiError::forbidden("CSRF check failed").into_response();
    }
    next.run(req).await
}
```

Use imports: `axum::extract::{Request, State}`, `axum::http::{header, HeaderMap, Method}`, `axum::middleware::Next`, `axum::response::{IntoResponse, Response}`, `axum_extra::extract::cookie::CookieJar`, `std::sync::Arc`.

- [ ] **Step 3: API 路由**

在 `api/mod.rs` 增加（替换 Task 3 的 stub）：

```rust
pub mod auth;

pub fn router(state: Arc<crate::serve::AppState>) -> axum::Router {
    use axum::routing::{get, post};
    let public = axum::Router::new()
        .route("/auth/login", post(auth::login));
    let protected = axum::Router::new()
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            crate::session::require_session,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            crate::session::csrf_guard,
        ));
    public
        .merge(protected)
        .fallback(|| async { api_not_found() })
        .with_state((*state).clone())
}
```

- [ ] **Step 4: 测试**

`serve.rs` integration:
- login with correct token returns 204 and Set-Cookie has `nuwa_session` (HttpOnly) and `nuwa_csrf`.
- me with session cookie returns 200 and local_mode false.
- me without cookie 401.
- logout without CSRF 403; with cookie+header 204.
- wrong token 5 times → 429.
- local mode (ui_token None): me 200 local_mode true.
- Cookie attributes: session HttpOnly; csrf not HttpOnly (Set-Cookie string checks).
- Use `ConnectInfo` extension on requests.

- [ ] **Step 5: 提交**

```bash
git add crates/nuwa/src/api/auth.rs crates/nuwa/src/api/mod.rs crates/nuwa/src/session.rs crates/nuwa/src/serve.rs
git commit -m "feat(api): session login, logout, me and CSRF middleware"
```

---

### Task 5: 安全响应头与安全测试

**Files:**
- Modify: `crates/nuwa/src/serve.rs`（`security_headers` + build_app layer）
- Test: `serve.rs`

**Interfaces:**
- `security_headers(req, next)`
- Strict CSP for API/SPA; relaxed CSP for legacy `/ui`/`/issue`/`/settings`.

- [ ] **Step 1: 安全头**

Add `.layer(axum::middleware::from_fn(crate::serve::security_headers))` to `build_app` (after the static fallback) and implement:

`security_headers` in `serve.rs`:

```rust
pub async fn security_headers(req: Request, next: Next) -> Response {
    let legacy = req.uri().path().starts_with("/ui")
        || req.uri().path().starts_with("/issue/")
        || req.uri().path().starts_with("/settings");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    let csp = if legacy {
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'self'"
    } else {
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
    };
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(csp));
    res
}
```

- [ ] **Step 2: 测试**

- API response has CSP/nosniff/DENY.
- Legacy `/ui` response CSP contains `unsafe-inline` (even a 401 response should carry the headers).
- Static/placeholder response has the strict CSP.
- CSRF behavior is covered in Task 4; do not duplicate it here.

- [ ] **Step 3: 提交**

```bash
git add crates/nuwa/src/serve.rs
git commit -m "feat(security): response headers with legacy/API CSP split"
```

---

### Task 6: 静态资源占位与 SPA fallback

**Files:**
- Create: `crates/nuwa/src/web.rs`
- Create: `web/dist/.gitkeep`
- Modify: `.gitignore`（`/web/dist/*` + `!/web/dist/.gitkeep`）
- Modify: `crates/nuwa/src/main.rs`（`mod web;`）
- Test: `web.rs` tests

- [ ] **Step 1: 实现**

```rust
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

const PLACEHOLDER: &str = "<!doctype html><html lang=\"zh\"><meta charset=\"utf-8\"><title>Nuwa</title><body><h1>Nuwa console</h1><p>frontend not built yet</p></body></html>";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(asset) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap_or(HeaderValue::from_static("application/octet-stream"))),
                (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            ],
            asset.data,
        )
            .into_response();
    }
    if path.contains('.') || path.starts_with("assets/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if Assets::get("index.html").is_some() {
        // SPA fallback: serve the built index.
        let asset = Assets::get("index.html").expect("index");
        return (StatusCode::OK, [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")), (header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))], asset.data).into_response();
    }
    (StatusCode::OK, [(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))], PLACEHOLDER).into_response()
}
```

- [ ] **Step 2: `.gitignore`**

Append:

```
/web/dist/*
!/web/dist/.gitkeep
```

Create `web/dist/.gitkeep` (empty). `rust-embed` compiles with the folder present.

- [ ] **Step 3: 测试**

- GET `/` → 200, body contains "frontend not built" (since only .gitkeep).
- GET `/assets/missing.js` → 404.
- GET `/api/v1/unknown` → JSON 404 (API fallback).
- Security headers on `/`.

- [ ] **Step 4: 提交**

```bash
git add .gitignore web/dist/.gitkeep crates/nuwa/src/web.rs crates/nuwa/src/main.rs
git commit -m "feat(web): embedded SPA placeholder and static fallback"
```

---

### Task 7: 集成测试与文档

**Files:**
- Modify: `serve.rs` tests / `session.rs` tests
- Modify: `docs/07-open-items.md`（§D）
- Modify: `docs/08-onboarding.md`（API 登录说明）

- [ ] **Step 1: 完整集成测试**

Use `build_app` with a temp DB, UI token `"secret"`, cookie secure false. Cover:
- health 200.
- `/api/v1/auth/login` correct token → 204; Set-Cookie session HttpOnly + csrf non-HttpOnly.
- `/api/v1/auth/me` with cookies → 200 local_mode false.
- `/api/v1/auth/me` without cookies → 401 JSON.
- `/api/v1/auth/logout` without CSRF → 403; with CSRF → 204 and cleared cookies.
- wrong token 5 times → 429.
- local mode (UI token None) → me 200 local_mode true.
- `/ui` without Basic → 401; security headers present.
- static `/` 200 placeholder; missing asset 404.
- `/api/v1/unknown` 404 JSON.

- [ ] **Step 2: docs**

docs/07 append M2a bullet: API auth foundation, 401/403/429, headers, placeholder; tests count.
docs/08 add:

```markdown
**API 登录（v3 控制台）**：`POST /api/v1/auth/login`，body `{"token":"<NUWA_UI_TOKEN>"}`；成功 204 并设置 `nuwa_session`（HttpOnly）与 `nuwa_csrf`。后续非 GET 请求带 `X-NUWA-CSRF` 头。旧 `/ui` Basic 鉴权保留至 M6。
```

- [ ] **Step 3: 提交**

```bash
git add docs/07-open-items.md docs/08-onboarding.md
git commit -m "docs: record M2 auth foundation and API login"
```

---

### Task 8: 全量验证

- [ ] **Step 1: 检查**

```bash
taskkill //F //IM nuwa.exe 2>/dev/null || true
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
```

Expected: clippy clean; tests green.

- [ ] **Step 2: 手工冒烟**

Start serve with `NUWA_UI_TOKEN=secret`, temp DB; use curl:
- `GET /health` 200.
- `POST /api/v1/auth/login` JSON token → 204 + Set-Cookie.
- `GET /api/v1/auth/me` with cookie → 200.
- Without cookie → 401.
- `POST /api/v1/auth/logout` without CSRF → 403; with CSRF → 204.
- `GET /api/v1/unknown` → 404 JSON.
- `GET /` → placeholder.
Stop serve.

- [ ] **Step 3: 提交（若无改动跳过）**

## 自审记录

- Spec coverage: §6 auth/CSRF/headers, §9.1 conventions, §9.2 auth endpoints; settings/system/read/action endpoints are M2b/M2c.
- No placeholders; all tests and interfaces are concrete.
- Signature consistency: `AppState`, `SessionManager`, `ApiError`, `build_app`, `api::router` used consistently across tasks.
- Known edge: `build_app`/`SessionManager` `expect`/`unwrap_or_else` patterns must keep clippy clean; prefer non-panicking fallbacks where possible.
- Legacy `/ui` remains Basic with a relaxed CSP until M6; strict CSP applies to API/SPA.
