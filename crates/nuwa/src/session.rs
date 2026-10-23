use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MAX_LOGIN_FAILURES: u32 = 5;
pub const LOGIN_WINDOW_MS: i64 = 10 * 60 * 1000;

pub struct SessionManager {
    key: [u8; 32],
    ttl_ms: i64,
    /// Session ids revoked by an explicit logout, with the moment they would
    /// have expired anyway. Without this, logging out only cleared the cookie:
    /// a copied cookie stayed valid until its TTL.
    ///
    /// In memory on purpose: a restart forgets the list, but a restart also
    /// re-reads `NUWA_UI_TOKEN`, and rotating the token invalidates every
    /// session immediately.
    revoked: Mutex<HashMap<String, i64>>,
}

pub struct IssuedSession {
    pub token: String,
    pub csrf: String,
}

impl SessionManager {
    pub fn new(ui_token: &str, ttl_ms: i64) -> Self {
        let key: [u8; 32] = Sha256::digest(ui_token.as_bytes()).into();
        Self {
            key,
            ttl_ms,
            revoked: Mutex::new(HashMap::new()),
        }
    }

    pub fn issue(&self, now_ms: i64) -> IssuedSession {
        let expires_at_ms = now_ms + self.ttl_ms;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let payload = format!("v1.{expires_at_ms}.{nonce}");
        let token = format!("{payload}.{}", self.sign(&payload));
        let csrf = uuid::Uuid::new_v4().simple().to_string();
        IssuedSession { token, csrf }
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
        {
            let mut revoked = self.revoked.lock().unwrap_or_else(|p| p.into_inner());
            revoked.retain(|_, expires_at| *expires_at > now_ms);
            if revoked.contains_key(parts[2]) {
                return None;
            }
        }
        Some(expires_at_ms)
    }

    /// Revoke the session a token belongs to (explicit logout). The token is
    /// verified first, so an attacker cannot use logout to fill the list.
    pub fn revoke(&self, token: &str, now_ms: i64) -> bool {
        let Some(expires_at_ms) = self.verify(token, now_ms) else {
            return false;
        };
        let Some(nonce) = token.split('.').nth(2) else {
            return false;
        };
        let mut revoked = self.revoked.lock().unwrap_or_else(|p| p.into_inner());
        revoked.retain(|_, expires_at| *expires_at > now_ms);
        revoked.insert(nonce.to_string(), expires_at_ms);
        true
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
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, ip: IpAddr, now_ms: i64) -> Result<(), i64> {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Drop buckets for every IP whose window has closed, not just the
        // caller's, so the map cannot grow without bound. `check` and
        // `failure` take separate locks, so a burst can overshoot
        // MAX_LOGIN_FAILURES by the number of in-flight requests; strict
        // atomic reservation is out of scope for M2a.
        map.retain(|_, bucket| bucket.reset_at_ms > now_ms);
        if let Some(b) = map.get(&ip) {
            if b.failures >= MAX_LOGIN_FAILURES {
                return Err(b.reset_at_ms - now_ms);
            }
        }
        Ok(())
    }

    pub fn failure(&self, ip: IpAddr, now_ms: i64) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|_, bucket| bucket.reset_at_ms > now_ms);
        let bucket = map.entry(ip).or_insert(Bucket {
            failures: 0,
            reset_at_ms: now_ms + LOGIN_WINDOW_MS,
        });
        bucket.failures += 1;
    }

    pub fn success(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(&ip);
    }
}

fn authority(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub fn origin_ok(origin: Option<&str>, referer: Option<&str>, host: Option<&str>) -> bool {
    // An empty Host can never match a real authority, and accepting it would
    // let a header-less request skip the same-origin check entirely.
    let Some(host) = host.filter(|host| !host.is_empty()) else {
        return false;
    };
    let candidate = match origin {
        Some(o) => authority(o),
        None => referer.and_then(authority),
    };
    match candidate {
        Some(h) => h.eq_ignore_ascii_case(host),
        None => origin.is_none() && referer.is_none(),
    }
}

/// Session gate for protected `/api/v1` routes. Local mode (no UI token or an
/// empty one) is implicitly authenticated; otherwise a valid signed
/// `nuwa_session` cookie is required.
pub async fn require_session(
    State(state): State<Arc<crate::serve::AppState>>,
    jar: CookieJar,
    req: Request,
    next: Next,
) -> Response {
    if state.local_mode() {
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

/// Double-submit CSRF gate: mutating requests need the `nuwa_csrf` cookie and
/// the `X-NUWA-CSRF` header to match, plus an Origin/Referer (when present)
/// whose authority equals Host. Local mode is *not* exempt.
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
    let cookie = jar
        .get("nuwa_csrf")
        .map(|cookie| cookie.value())
        .unwrap_or("");
    let header = headers
        .get("x-nuwa-csrf")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let referer = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if cookie.is_empty()
        || !SessionManager::token_eq(cookie, header)
        || !origin_ok(origin, referer, host)
    {
        return crate::api::ApiError::forbidden("CSRF check failed").into_response();
    }
    next.run(req).await
}

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

    /// 登出必须让会话立即失效：只清 cookie 的话，被复制的 cookie 在 TTL
    /// 内仍然有效。
    #[test]
    fn logout_revokes_the_session_immediately() {
        let m = SessionManager::new("ui-secret", 60_000);
        let first = m.issue(1_000);
        let second = m.issue(1_000);
        assert!(m.verify(&first.token, 1_001).is_some());

        assert!(m.revoke(&first.token, 1_002));
        assert!(
            m.verify(&first.token, 1_003).is_none(),
            "a revoked session must not verify"
        );
        assert!(
            m.verify(&second.token, 1_003).is_some(),
            "other sessions stay valid"
        );
        // Revoking a forged token is a no-op and must not poison the list.
        assert!(!m.revoke("v1.9999999999.forged.sig", 1_004));
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
    fn limiter_prunes_stale_buckets_for_other_ips() {
        let l = LoginLimiter::new();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        for _ in 0..3 {
            l.failure(a, 1_000);
        }
        let later = 1_000 + LOGIN_WINDOW_MS + 1;
        assert!(l.check(b, later).is_ok());
        {
            let map = l.inner.lock().expect("lock");
            assert!(!map.contains_key(&a), "expired bucket pruned by check");
            assert!(!map.contains_key(&b), "check does not insert a bucket");
        }
        l.failure(b, later);
        let map = l.inner.lock().expect("lock");
        assert!(!map.contains_key(&a), "expired bucket pruned by failure");
        assert_eq!(map.get(&b).map(|bucket| bucket.failures), Some(1));
    }

    #[test]
    fn origin_ok_rules() {
        assert!(origin_ok(None, None, Some("h:1")));
        assert!(origin_ok(Some("https://h:1"), None, Some("h:1")));
        assert!(!origin_ok(Some("https://evil"), None, Some("h:1")));
        assert!(!origin_ok(Some("https://h:1"), None, None));
    }

    #[test]
    fn origin_ok_rejects_empty_host() {
        assert!(!origin_ok(None, None, Some("")));
        assert!(!origin_ok(Some("https://h:1"), None, Some("")));
        assert!(!origin_ok(None, Some("https://h:1/"), Some("")));
    }
}
