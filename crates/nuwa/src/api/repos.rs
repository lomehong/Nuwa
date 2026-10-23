//! `/api/v1/repos` — the console repository registry.
//!
//! Only the basic display identity is editable here. Repository content
//! (`failing_check`, `forbidden_paths`, `notes`) stays read-only in the target
//! repo's `nuwa.repo.json`; this module neither reads nor writes it.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::parse_json_body;
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{RepoRow, ShanError, Store};

const MAX_REPO_LEN: usize = 256;
const MAX_NAME_LEN: usize = 128;
const MAX_URL_LEN: usize = 512;
const MAX_DESCRIPTION_LEN: usize = 1000;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepoListResponse {
    pub data: Vec<RepoDto>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepoResponse {
    pub data: RepoDto,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepoDto {
    pub repo: String,
    pub name: String,
    pub url: String,
    pub description: String,
    #[serde(rename = "created_at_ms")]
    pub created_at: i64,
    #[serde(rename = "updated_at_ms")]
    pub updated_at: i64,
}

impl From<RepoRow> for RepoDto {
    fn from(row: RepoRow) -> Self {
        Self {
            repo: row.repo,
            name: row.name,
            url: row.url,
            description: row.description,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct CreateRepoRequest {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// The identity comes from the path and is immutable, so an update body never
/// carries `repo` — a stray value is ignored by serde.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct UpdateRepoRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

struct ValidatedRepo {
    repo: String,
    name: String,
    url: String,
    description: String,
}

struct RepoFields {
    name: String,
    url: String,
    description: String,
}

/// `GET /api/v1/repos` — every registered repository, name order.
pub async fn list(State(state): State<AppState>) -> Response {
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_list(&store)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("repos: store read failed: {error}");
            internal_error("read").into_response()
        }
        Err(error) => {
            eprintln!("repos: blocking task failed: {error}");
            internal_error("read").into_response()
        }
    }
}

/// `POST /api/v1/repos` — register a repository. A duplicate identity is a
/// 409; use PUT to edit one.
pub async fn create(
    State(state): State<AppState>,
    payload: Result<Json<CreateRepoRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let input = match validate_create(request) {
        Ok(input) => input,
        Err(error) => return error.into_response(),
    };
    let now = crate::now_ms();
    let row = RepoRow {
        repo: input.repo.clone(),
        name: input.name.clone(),
        url: input.url.clone(),
        description: input.description.clone(),
        created_at: now,
        updated_at: now,
    };
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        store.insert_repo(
            &input.repo,
            &input.name,
            &input.url,
            &input.description,
            now,
        )
    })
    .await
    {
        Ok(Ok(())) => (
            StatusCode::CREATED,
            Json(RepoResponse {
                data: RepoDto::from(row),
            }),
        )
            .into_response(),
        Ok(Err(error)) => store_error("create", error).into_response(),
        Err(error) => {
            eprintln!("repos create: blocking task failed: {error}");
            internal_error("create").into_response()
        }
    }
}

/// `PUT /api/v1/repos/{repo}` — edit display fields; identity is immutable.
pub async fn update(
    State(state): State<AppState>,
    Path(repo): Path<String>,
    payload: Result<Json<UpdateRepoRequest>, JsonRejection>,
) -> Response {
    let request = match parse_json_body(payload) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let fields = match validate_update(request) {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || -> Result<Option<RepoRow>, ShanError> {
        let changed =
            store.update_repo(&repo, &fields.name, &fields.url, &fields.description, now)?;
        if !changed {
            return Ok(None);
        }
        store.get_repo(&repo)
    })
    .await
    {
        Ok(Ok(Some(row))) => Json(RepoResponse {
            data: RepoDto::from(row),
        })
        .into_response(),
        Ok(Ok(None)) => ApiError::not_found("repository not found").into_response(),
        Ok(Err(error)) => store_error("update", error).into_response(),
        Err(error) => {
            eprintln!("repos update: blocking task failed: {error}");
            internal_error("update").into_response()
        }
    }
}

/// `DELETE /api/v1/repos/{repo}` — unregister a repository.
pub async fn delete(State(state): State<AppState>, Path(repo): Path<String>) -> Response {
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || store.delete_repo(&repo)).await {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => ApiError::not_found("repository not found").into_response(),
        Ok(Err(error)) => store_error("delete", error).into_response(),
        Err(error) => {
            eprintln!("repos delete: blocking task failed: {error}");
            internal_error("delete").into_response()
        }
    }
}

fn build_list(store: &Store) -> Result<RepoListResponse, ShanError> {
    Ok(RepoListResponse {
        data: store.list_repos()?.into_iter().map(RepoDto::from).collect(),
    })
}

fn store_error(action: &'static str, error: ShanError) -> ApiError {
    match error {
        ShanError::RepoExists(repo) => {
            ApiError::conflict(format!("repository {repo} is already registered"))
        }
        other => {
            eprintln!("repos {action}: store failed: {other}");
            internal_error(action)
        }
    }
}

fn internal_error(action: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        format!("failed to {action} repositories"),
    )
}

fn validate_create(request: CreateRepoRequest) -> Result<ValidatedRepo, ApiError> {
    let mut fields = BTreeMap::new();
    let repo = validate_repo(request.repo, &mut fields);
    let (name, url, description) =
        validate_fields(request.name, request.url, request.description, &mut fields);
    finish(
        fields,
        ValidatedRepo {
            repo,
            name,
            url,
            description,
        },
    )
}

fn validate_update(request: UpdateRepoRequest) -> Result<RepoFields, ApiError> {
    let mut fields = BTreeMap::new();
    let (name, url, description) =
        validate_fields(request.name, request.url, request.description, &mut fields);
    finish(
        fields,
        RepoFields {
            name,
            url,
            description,
        },
    )
}

fn finish<T>(fields: BTreeMap<String, String>, value: T) -> Result<T, ApiError> {
    if fields.is_empty() {
        Ok(value)
    } else {
        Err(ApiError::validation(fields))
    }
}

fn validate_repo(raw: Option<String>, fields: &mut BTreeMap<String, String>) -> String {
    let repo = raw.unwrap_or_default().trim().to_string();
    if repo.is_empty() {
        fields.insert("repo".to_string(), "is required".to_string());
    } else if repo.chars().count() > MAX_REPO_LEN {
        fields.insert(
            "repo".to_string(),
            format!("must be at most {MAX_REPO_LEN} characters"),
        );
    } else if !repo
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '/'))
    {
        fields.insert(
            "repo".to_string(),
            "may only contain ASCII letters, digits and - . _ : /".to_string(),
        );
    }
    repo
}

fn validate_fields(
    name: Option<String>,
    url: Option<String>,
    description: Option<String>,
    fields: &mut BTreeMap<String, String>,
) -> (String, String, String) {
    let name = name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        fields.insert("name".to_string(), "is required".to_string());
    } else if name.chars().count() > MAX_NAME_LEN {
        fields.insert(
            "name".to_string(),
            format!("must be at most {MAX_NAME_LEN} characters"),
        );
    } else if name.chars().any(char::is_control) {
        fields.insert(
            "name".to_string(),
            "must not contain control characters".to_string(),
        );
    }

    let url = url.unwrap_or_default().trim().to_string();
    if url.is_empty() {
        fields.insert("url".to_string(), "is required".to_string());
    } else if url.chars().count() > MAX_URL_LEN {
        fields.insert(
            "url".to_string(),
            format!("must be at most {MAX_URL_LEN} characters"),
        );
    } else if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        fields.insert(
            "url".to_string(),
            "must not contain whitespace or control characters".to_string(),
        );
    } else if !valid_repo_url(&url) {
        fields.insert(
            "url".to_string(),
            "must be an http(s), ssh or git URL".to_string(),
        );
    }

    let description = description.unwrap_or_default().trim().to_string();
    if description.chars().count() > MAX_DESCRIPTION_LEN {
        fields.insert(
            "description".to_string(),
            format!("must be at most {MAX_DESCRIPTION_LEN} characters"),
        );
    }

    (name, url, description)
}

/// Link schemes a repo card may carry.
///
/// The console renders this URL as an anchor, so `javascript:` / `data:` /
/// `file:` must never reach the store — the frontend validates too, but the
/// API is the trust boundary. The scp-like `git@host:owner/repo.git` form has
/// no scheme and is accepted as-is.
fn valid_repo_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    match lower.find(':') {
        Some(index) if !lower[..index].contains('/') => {
            let head = &lower[..index];
            if head.contains('@') {
                return true;
            }
            matches!(head, "http" | "https" | "ssh" | "git")
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use serde_json::json;
    use shanhaijing::Store;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

    fn test_app() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("repos.db");
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
            cookie_secure_never: false,
            serve_unit: "nuwa-serve".to_string(),
            runner_units: vec!["nuwa-work".to_string()],
        };
        let app = build_app(state);
        (dir, app)
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
        let response = get(app, "/api/v1/auth/me").await;
        assert_eq!(response.status(), StatusCode::OK);
        let token = set_cookie_value(&response, "nuwa_csrf");
        let value = json_body(response).await;
        assert_eq!(value["authenticated"], true);
        assert_eq!(value["local_mode"], true);
        token
    }

    fn repo_path(repo: &str) -> String {
        // `/` is already legal inside the identity; percent-encode it so the
        // path stays one segment for `{repo}`.
        format!("/api/v1/repos/{}", repo.replace('/', "%2F"))
    }

    async fn get(app: &Router, uri: &str) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    fn csrf_headers(
        request: axum::http::request::Builder,
        csrf: Option<&str>,
    ) -> axum::http::request::Builder {
        let request = request.header(header::HOST, "localhost:7321");
        match csrf {
            Some(token) => request
                .header(header::COOKIE, format!("nuwa_csrf={token}"))
                .header("x-nuwa-csrf", token),
            None => request,
        }
    }

    async fn post_json(app: &Router, uri: &str, csrf: Option<&str>, body: &str) -> Response {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        app.clone()
            .oneshot(
                csrf_headers(request, csrf)
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    async fn put_json(app: &Router, uri: &str, csrf: Option<&str>, body: &str) -> Response {
        let request = Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        app.clone()
            .oneshot(
                csrf_headers(request, csrf)
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    async fn delete(app: &Router, uri: &str, csrf: Option<&str>) -> Response {
        let request = Request::builder().method("DELETE").uri(uri);
        app.clone()
            .oneshot(
                csrf_headers(request, csrf)
                    .body(Body::empty())
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

    fn header_str(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
            .to_string()
    }

    fn assert_security_headers(response: &Response) {
        assert_eq!(header_str(response, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_str(response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
    }

    fn assert_api_headers(response: &Response) {
        assert_security_headers(response);
        assert!(
            header_str(response, header::CONTENT_TYPE).starts_with("application/json"),
            "API responses must be JSON"
        );
    }

    #[tokio::test]
    async fn create_list_update_and_delete_round_trip() {
        let (_dir, app) = test_app();
        let csrf = csrf_token(&app).await;

        let response = post_json(
            &app,
            "/api/v1/repos",
            Some(&csrf),
            r#"{"repo":" github.com/acme/app ","name":" Acme App ","url":" git@github.com:acme/app.git ","description":" primary "}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_api_headers(&response);
        let value = json_body(response).await;
        let row = &value["data"];
        assert_eq!(row["repo"], "github.com/acme/app");
        assert_eq!(row["name"], "Acme App", "values are trimmed");
        assert_eq!(row["url"], "git@github.com:acme/app.git");
        assert_eq!(row["description"], "primary");
        let created_at = row["created_at_ms"].as_i64().expect("created_at_ms");
        assert!(created_at > 0);
        assert_eq!(row["updated_at_ms"].as_i64(), Some(created_at));

        let response = get(&app, "/api/v1/repos").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        let rows = value["data"].as_array().expect("data array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["repo"], "github.com/acme/app");

        // Identity is path-owned and immutable; a stray body `repo` is ignored.
        let response = put_json(
            &app,
            &repo_path("github.com/acme/app"),
            Some(&csrf),
            r#"{"repo":"evil/other","name":"Acme Console","url":"https://github.com/acme/app","description":""}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        let row = &value["data"];
        assert_eq!(row["repo"], "github.com/acme/app");
        assert_eq!(row["name"], "Acme Console");
        assert_eq!(row["url"], "https://github.com/acme/app");
        assert_eq!(row["description"], "");
        assert_eq!(row["created_at_ms"].as_i64(), Some(created_at));
        assert!(row["updated_at_ms"].as_i64().expect("updated_at_ms") >= created_at);

        let response = delete(&app, &repo_path("github.com/acme/app"), Some(&csrf)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_security_headers(&response);

        let value = json_body(get(&app, "/api/v1/repos").await).await;
        assert!(value["data"].as_array().expect("data array").is_empty());
    }

    #[tokio::test]
    async fn duplicate_identity_is_409_conflict() {
        let (_dir, app) = test_app();
        let csrf = csrf_token(&app).await;
        let body = r#"{"repo":"example.com/app","name":"App","url":"https://example.com/app"}"#;
        assert_eq!(
            post_json(&app, "/api/v1/repos", Some(&csrf), body)
                .await
                .status(),
            StatusCode::CREATED
        );
        let response = post_json(
            &app,
            "/api/v1/repos",
            Some(&csrf),
            r#"{"repo":"example.com/app","name":"Other","url":"https://other"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "conflict");

        let value = json_body(get(&app, "/api/v1/repos").await).await;
        assert_eq!(value["data"].as_array().expect("data array").len(), 1);
    }

    #[tokio::test]
    async fn create_validation_reports_field_names() {
        let (_dir, app) = test_app();
        let csrf = csrf_token(&app).await;
        let cases = vec![
            (json!({}), "repo"),
            (json!({"repo": "", "name": "n", "url": "https://x"}), "repo"),
            (
                json!({"repo": "has space", "name": "n", "url": "https://x"}),
                "repo",
            ),
            (
                json!({"repo": "a".repeat(257), "name": "n", "url": "https://x"}),
                "repo",
            ),
            (
                json!({"repo": "ok/repo", "name": "bad\nname", "url": "https://x"}),
                "name",
            ),
            (
                json!({"repo": "ok/repo", "name": "n".repeat(129), "url": "https://x"}),
                "name",
            ),
            (
                json!({"repo": "ok/repo", "name": "n", "url": "https://x y"}),
                "url",
            ),
            (
                json!({"repo": "ok/repo", "name": "n", "url": "u".repeat(513)}),
                "url",
            ),
            // The console renders the URL as an anchor: only link schemes a
            // browser can safely follow are stored.
            (
                json!({"repo": "ok/repo", "name": "n", "url": "javascript:alert(1)"}),
                "url",
            ),
            (
                json!({"repo": "ok/repo", "name": "n", "url": "data:text/html,<script>x</script>"}),
                "url",
            ),
            (
                json!({"repo": "ok/repo", "name": "n", "url": "file:///etc/passwd"}),
                "url",
            ),
            (
                json!({
                    "repo": "ok/repo",
                    "name": "n",
                    "url": "https://x",
                    "description": "d".repeat(1001)
                }),
                "description",
            ),
        ];
        for (body, field) in cases {
            let response = post_json(&app, "/api/v1/repos", Some(&csrf), &body.to_string()).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "body {body}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"].get(field).is_some(),
                "expected field {field} in {value}"
            );
        }
    }

    #[tokio::test]
    async fn update_validation_reports_field_names() {
        let (_dir, app) = test_app();
        let csrf = csrf_token(&app).await;
        let body = r#"{"repo":"example.com/app","name":"App","url":"https://x"}"#;
        assert_eq!(
            post_json(&app, "/api/v1/repos", Some(&csrf), body)
                .await
                .status(),
            StatusCode::CREATED
        );
        let path = repo_path("example.com/app");
        let cases = vec![
            (json!({"name": "", "url": "https://x"}), "name"),
            (json!({"name": "n", "url": "bad url"}), "url"),
            (
                json!({"name": "n", "url": "https://x", "description": "d".repeat(1001)}),
                "description",
            ),
        ];
        for (body, field) in cases {
            let response = put_json(&app, &path, Some(&csrf), &body.to_string()).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "body {body}"
            );
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed");
            assert!(
                value["error"]["fields"].get(field).is_some(),
                "expected field {field} in {value}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_repo_update_and_delete_are_404() {
        let (_dir, app) = test_app();
        let csrf = csrf_token(&app).await;
        let path = repo_path("example.com/missing");
        let response = put_json(
            &app,
            &path,
            Some(&csrf),
            r#"{"name":"N","url":"https://x"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");

        let response = delete(&app, &path, Some(&csrf)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn mutations_require_csrf_even_in_local_mode() {
        let (_dir, app) = test_app();
        let response = post_json(&app, "/api/v1/repos", None, "{}").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        let path = repo_path("example.com/app");
        let response = put_json(&app, &path, None, "{}").await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        let response = delete(&app, &path, None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "forbidden");

        // Reads stay available without the token.
        assert_eq!(get(&app, "/api/v1/repos").await.status(), StatusCode::OK);
    }
}
