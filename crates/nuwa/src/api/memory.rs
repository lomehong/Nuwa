//! `/api/v1/projects/{repo}/memory` — the repair-memory ledger.
//!
//! Memory is written by the runner only after verification + CI proof. The
//! owner can disable a row (it stops entering prompts but keeps its provenance)
//! or delete it outright. Both actions are audited as owner events.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::parse_json_body;
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::RepairMemoryView;

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;
const MAX_FINGERPRINT_LEN: usize = 128;
const MAX_AUDIT_DETAIL: usize = 300;

#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct MemoryListResponse {
    pub data: Vec<MemoryDto>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct MemoryDto {
    pub fingerprint: String,
    pub title: String,
    pub state: String,
    pub commit_sha: String,
    pub pr_url: Option<String>,
    pub root_cause: String,
    pub change_stat: String,
    pub files: i64,
    #[serde(rename = "recorded_at_ms")]
    pub recorded_at: i64,
    pub disabled: bool,
}

impl From<RepairMemoryView> for MemoryDto {
    fn from(row: RepairMemoryView) -> Self {
        Self {
            fingerprint: row.fingerprint,
            title: row.title,
            state: row.state,
            commit_sha: row.commit_sha,
            pr_url: row.pr_url,
            root_cause: row.root_cause,
            change_stat: row.change_stat,
            files: row.files,
            recorded_at: row.recorded_at,
            disabled: row.disabled,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct UpdateMemoryRequest {
    #[serde(default)]
    pub disabled: Option<bool>,
}

/// `GET /api/v1/projects/{repo}/memory` — newest first, paginated.
pub async fn list(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    Query(query): Query<ListQuery>,
) -> Response {
    let repo = match validate_repo(&repo) {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        let (rows, total) = store.list_repair_memory(&repo, limit, offset)?;
        Ok::<_, shanhaijing::ShanError>(MemoryListResponse {
            data: rows.into_iter().map(MemoryDto::from).collect(),
            total,
            limit,
            offset,
        })
    })
    .await
    {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("memory list: store read failed: {error}");
            internal_error("read")
        }
        Err(error) => {
            eprintln!("memory list: blocking task failed: {error}");
            internal_error("read")
        }
    }
}

/// `PATCH /api/v1/projects/{repo}/memory/{fp}` — disable/enable a memory.
pub async fn update(
    State(state): State<AppState>,
    Path((repo, fingerprint)): Path<(String, String)>,
    payload: Result<Json<UpdateMemoryRequest>, JsonRejection>,
) -> Response {
    let repo = match validate_repo(&repo) {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let fingerprint = match validate_fingerprint(&fingerprint) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return error.into_response(),
    };
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Some(disabled) = request.disabled else {
        return ApiError::validation(BTreeMap::from([(
            "disabled".to_string(),
            "is required".to_string(),
        )]))
        .into_response();
    };
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    let event_repo = repo.clone();
    let event_fp = fingerprint.clone();
    match tokio::task::spawn_blocking(move || {
        if !store.set_repair_memory_disabled(&repo, &fingerprint, disabled)? {
            return Ok::<_, shanhaijing::ShanError>(false);
        }
        let detail = if disabled {
            "repair memory disabled by owner"
        } else {
            "repair memory enabled by owner"
        };
        let _ = store.record_event(
            &event_fp,
            now,
            Some(&event_repo),
            "owner",
            "memory_updated",
            detail,
        );
        Ok(true)
    })
    .await
    {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => ApiError::not_found("repair memory not found").into_response(),
        Ok(Err(error)) => {
            eprintln!("memory update: store failed: {error}");
            internal_error("update")
        }
        Err(error) => {
            eprintln!("memory update: blocking task failed: {error}");
            internal_error("update")
        }
    }
}

/// `DELETE /api/v1/projects/{repo}/memory/{fp}` — remove the memory.
pub async fn delete(
    State(state): State<AppState>,
    Path((repo, fingerprint)): Path<(String, String)>,
) -> Response {
    let repo = match validate_repo(&repo) {
        Ok(repo) => repo,
        Err(error) => return error.into_response(),
    };
    let fingerprint = match validate_fingerprint(&fingerprint) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return error.into_response(),
    };
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    let event_repo = repo.clone();
    let event_fp = fingerprint.clone();
    match tokio::task::spawn_blocking(move || {
        if !store.delete_repair_memory(&repo, &fingerprint)? {
            return Ok::<_, shanhaijing::ShanError>(false);
        }
        let details: String = "repair memory deleted by owner"
            .chars()
            .take(MAX_AUDIT_DETAIL)
            .collect();
        let _ = store.record_event(
            &event_fp,
            now,
            Some(&event_repo),
            "owner",
            "memory_deleted",
            &details,
        );
        Ok(true)
    })
    .await
    {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => ApiError::not_found("repair memory not found").into_response(),
        Ok(Err(error)) => {
            eprintln!("memory delete: store failed: {error}");
            internal_error("delete")
        }
        Err(error) => {
            eprintln!("memory delete: blocking task failed: {error}");
            internal_error("delete")
        }
    }
}

fn validate_repo(raw: &str) -> Result<String, ApiError> {
    let repo = raw.trim();
    if repo.is_empty() || repo.chars().count() > 256 {
        return Err(ApiError::validation(BTreeMap::from([(
            "repo".to_string(),
            "must be a non-empty repository identity".to_string(),
        )])));
    }
    Ok(repo.to_string())
}

fn validate_fingerprint(raw: &str) -> Result<String, ApiError> {
    let fingerprint = raw.trim();
    if fingerprint.is_empty() || fingerprint.chars().count() > MAX_FINGERPRINT_LEN {
        return Err(ApiError::validation(BTreeMap::from([(
            "fingerprint".to_string(),
            "is invalid".to_string(),
        )])));
    }
    Ok(fingerprint.to_string())
}

fn internal_error(action: &'static str) -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        format!("failed to {action} repair memory"),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shanhaijing::{RepairMemoryEntry, Store};

    fn seed(store: &Store) {
        store
            .record_repair_memory(&RepairMemoryEntry {
                repo: "example.com/app".into(),
                fingerprint: "fp-a".into(),
                commit_sha: "0123456789abcdef".into(),
                pr_url: Some("https://github.com/acme/app/pull/1".into()),
                root_cause: "ROOT CAUSE: loop bound".into(),
                change_stat: "calc.go | 2 +-".into(),
                files: 1,
                recorded_at: 10,
            })
            .expect("memory");
    }

    #[test]
    fn list_projects_every_field_the_ui_needs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        seed(&store);
        let (rows, total) = store
            .list_repair_memory("example.com/app", DEFAULT_LIMIT, 0)
            .expect("list");
        assert_eq!(total, 1);
        let dto = MemoryDto::from(rows[0].clone());
        assert_eq!(dto.fingerprint, "fp-a");
        assert!(dto.pr_url.is_some());
        assert_eq!(dto.files, 1);
        assert!(!dto.disabled);
        // The issue was never ingested, so the join leaves the title empty.
        assert_eq!(dto.title, "");
    }

    mod routes {
        use std::sync::Arc;

        use axum::body::Body;
        use axum::http::{header, Request, StatusCode};
        use axum::response::Response;
        use axum::Router;
        use shanhaijing::Store;
        use tower::ServiceExt;

        use crate::serve::{build_app, AppState};

        fn test_app() -> (tempfile::TempDir, Router, Arc<Store>) {
            let dir = tempfile::tempdir().expect("tempdir");
            let db_path = dir.path().join("memory.db");
            let store = Arc::new(Store::open(&db_path).expect("open"));
            super::seed(&store);
            let state = AppState {
                store: Arc::clone(&store),
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
            (dir, build_app(state), store)
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
            set_cookie_value(&response, "nuwa_csrf")
        }

        const MEMORY: &str = "/api/v1/projects/example.com%2Fapp/memory";

        async fn request(
            app: &Router,
            method: &str,
            uri: &str,
            csrf: Option<&str>,
            body: Option<&str>,
        ) -> Response {
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "localhost:7321");
            if let Some(token) = csrf {
                builder = builder
                    .header(header::COOKIE, format!("nuwa_csrf={token}"))
                    .header("x-nuwa-csrf", token);
            }
            if body.is_some() {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
            }
            app.clone()
                .oneshot(
                    builder
                        .body(Body::from(body.unwrap_or_default().to_string()))
                        .expect("request"),
                )
                .await
                .expect("response")
        }

        async fn json_body(response: Response) -> serde_json::Value {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            serde_json::from_slice(&bytes).expect("json")
        }

        #[tokio::test]
        async fn list_disable_enable_and_delete_are_audited() {
            let (_dir, app, store) = test_app();
            let csrf = csrf_token(&app).await;

            let response = request(&app, "GET", MEMORY, None, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            let value = json_body(response).await;
            assert_eq!(value["total"], 1);
            assert_eq!(value["data"][0]["fingerprint"], "fp-a");
            assert_eq!(value["data"][0]["disabled"], false);

            // Mutations require the CSRF token even in local mode.
            let path = format!("{MEMORY}/fp-a");
            let response = request(&app, "PATCH", &path, None, Some(r#"{"disabled":true}"#)).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);

            let response = request(
                &app,
                "PATCH",
                &path,
                Some(&csrf),
                Some(r#"{"disabled":true}"#),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let value = json_body(request(&app, "GET", MEMORY, None, None).await).await;
            assert_eq!(value["data"][0]["disabled"], true);

            let response = request(
                &app,
                "PATCH",
                &path,
                Some(&csrf),
                Some(r#"{"disabled":false}"#),
            )
            .await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);

            let response = request(&app, "DELETE", &path, Some(&csrf), None).await;
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let value = json_body(request(&app, "GET", MEMORY, None, None).await).await;
            assert_eq!(value["total"], 0);

            let response = request(&app, "DELETE", &path, Some(&csrf), None).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);

            let events = store.events_for("fp-a", 20).expect("events");
            assert!(events.iter().any(|event| {
                event.kind == "memory_updated" && event.actor.as_deref() == Some("owner")
            }));
            assert!(events.iter().any(|event| {
                event.kind == "memory_deleted" && event.actor.as_deref() == Some("owner")
            }));
        }

        #[tokio::test]
        async fn malformed_patch_is_a_validation_error() {
            let (_dir, app, _store) = test_app();
            let csrf = csrf_token(&app).await;
            let path = format!("{MEMORY}/fp-a");
            let response = request(&app, "PATCH", &path, Some(&csrf), Some("{}")).await;
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let value = json_body(response).await;
            assert!(value["error"]["fields"].get("disabled").is_some());
        }
    }
}
