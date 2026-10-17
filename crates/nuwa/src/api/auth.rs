//! `/api/v1/auth` — login, logout and session introspection.
//!
//! `nuwa_session` is an HttpOnly HMAC-signed token; `nuwa_csrf` is the
//! readable half of the double-submit CSRF pair (docs/09 §6.1–6.4).

use std::net::SocketAddr;

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::serve::AppState;
use crate::session::{now_ms, SessionManager};

const SESSION_COOKIE: &str = "nuwa_session";
const CSRF_COOKIE: &str = "nuwa_csrf";

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
}

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
            .and_then(|value| value.to_str().ok())
            == Some("https")
}

fn session_cookie(token: String, ttl_ms: i64, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(cookie::time::Duration::milliseconds(ttl_ms))
        .secure(secure)
        .build()
}

fn csrf_cookie(token: String, ttl_ms: i64, secure: bool) -> Cookie<'static> {
    Cookie::build((CSRF_COOKIE, token))
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(cookie::time::Duration::milliseconds(ttl_ms))
        .secure(secure)
        .build()
}

/// Removal cookie must carry the same Path as the original, else the browser
/// keeps the live cookie while the response claims to have logged out.
fn clear_cookie(name: &'static str, secure: bool) -> Cookie<'static> {
    Cookie::build((name, ""))
        .same_site(SameSite::Strict)
        .path("/")
        .secure(secure)
        .build()
}

/// 429 with `Retry-After` in whole seconds, rounded up and at least 1
/// (docs/09 §6.4).
fn rate_limited_response(retry_after_ms: i64) -> Response {
    let seconds = (retry_after_ms.max(1) as u64).div_ceil(1000);
    let mut response = ApiError::rate_limited(retry_after_ms).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    jar: CookieJar,
    payload: Result<Json<LoginRequest>, JsonRejection>,
) -> Response {
    // Keep extractor rejections inside the JSON error contract: axum's default
    // rejection is plain text, which the SPA cannot parse.
    let Json(req) = match payload {
        Ok(payload) => payload,
        Err(rejection) => {
            let status = rejection.status();
            let message = rejection.body_text();
            return if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
                ApiError::new(status, "unsupported_media_type", message)
            } else {
                ApiError::invalid_request(message)
            }
            .into_response();
        }
    };
    let now = now_ms();
    if let Err(retry) = state.limiter.check(peer.ip(), now) {
        return rate_limited_response(retry);
    }
    // Local mode (token unset or empty) accepts any login token, matching the
    // session middleware and `me` (F5).
    if !state.local_mode() {
        let expected = state.ui_token.as_deref().unwrap_or("");
        if !SessionManager::token_eq(&req.token, expected) {
            state.limiter.failure(peer.ip(), now);
            return ApiError::unauthorized("invalid token").into_response();
        }
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

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Response {
    let secure = secure_cookie(&state, &headers);
    let jar = jar
        .remove(clear_cookie(SESSION_COOKIE, secure))
        .remove(clear_cookie(CSRF_COOKIE, secure));
    (jar, StatusCode::NO_CONTENT).into_response()
}

pub async fn me(State(state): State<AppState>, headers: HeaderMap, jar: CookieJar) -> Response {
    let now = now_ms();
    let local_mode = state.local_mode();
    let expires_at_ms = if local_mode {
        None
    } else {
        let Some(cookie) = jar.get(SESSION_COOKIE) else {
            return ApiError::unauthorized("not logged in").into_response();
        };
        match state.session.verify(cookie.value(), now) {
            Some(expires_at_ms) => Some(expires_at_ms),
            None => return ApiError::unauthorized("session expired").into_response(),
        }
    };
    let mut jar = jar;
    // A present-but-empty cookie is as useless as a missing one (the CSRF
    // guard rejects empty), so reissue it.
    if jar
        .get(CSRF_COOKIE)
        .map_or(true, |cookie| cookie.value().is_empty())
    {
        let secure = secure_cookie(&state, &headers);
        jar = jar.add(csrf_cookie(
            uuid::Uuid::new_v4().simple().to_string(),
            state.session.ttl_ms(),
            secure,
        ));
    }
    (
        jar,
        Json(MeResponse {
            authenticated: true,
            local_mode,
            expires_at_ms,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_is_httponly_and_csrf_is_readable() {
        let session = session_cookie("s".into(), 60_000, false).to_string();
        for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=60"] {
            assert!(session.contains(attr), "session cookie missing {attr}: {session}");
        }
        assert!(!session.contains("Secure"), "{session}");

        let csrf = csrf_cookie("c".into(), 60_000, false).to_string();
        assert!(!csrf.contains("HttpOnly"), "{csrf}");
        for attr in ["SameSite=Strict", "Path=/", "Max-Age=60"] {
            assert!(csrf.contains(attr), "csrf cookie missing {attr}: {csrf}");
        }
        assert!(!csrf.contains("Secure"), "{csrf}");
    }

    #[test]
    fn secure_attribute_is_opt_in() {
        assert!(session_cookie("s".into(), 1_000, true)
            .to_string()
            .contains("; Secure"));
        assert!(csrf_cookie("c".into(), 1_000, true)
            .to_string()
            .contains("; Secure"));
        assert!(clear_cookie(SESSION_COOKIE, true)
            .to_string()
            .contains("; Secure"));
    }

    #[test]
    fn retry_after_rounds_up_with_one_second_floor() {
        let response = rate_limited_response(1_001);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .expect("Retry-After")
                .to_str()
                .expect("ascii"),
            "2"
        );

        let response = rate_limited_response(0);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .expect("Retry-After")
                .to_str()
                .expect("ascii"),
            "1"
        );
    }
}
