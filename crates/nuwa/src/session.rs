// M2a Task 1 lands the auth primitives before the routes that consume them.
// Remove this allow once the auth/CSRF middleware and login handlers are wired.
#![allow(dead_code)]

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
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
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
