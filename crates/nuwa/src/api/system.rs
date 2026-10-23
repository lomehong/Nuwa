//! `GET /api/v1/system` — runtime version, build, paths and service liveness
//! (docs/09 §9.2 system).

use std::path::Path;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::api::common::{configured_units, run_bounded};
use crate::api::ApiError;
use crate::serve::AppState;

#[derive(Debug, Serialize)]
pub struct SystemResponse {
    pub version: &'static str,
    pub build_commit: &'static str,
    pub started_at_ms: i64,
    pub uptime_secs: u64,
    pub db_path: String,
    pub db_size_bytes: u64,
    pub local_mode: bool,
    pub services: Vec<ServiceState>,
}

#[derive(Debug, Serialize)]
pub struct ServiceState {
    pub unit: String,
    /// `systemctl is-active` output; `n/a` when systemd is unavailable.
    pub active: String,
}

/// `GET /api/v1/system` — read-only runtime facts for the settings/system
/// page. Subprocess probes run on the blocking pool.
pub async fn system(State(state): State<AppState>) -> Response {
    let db_path = state.db_path.clone();
    let started_at_ms = state.started_at_ms;
    let local_mode = state.local_mode();
    let units = configured_units(&state.serve_unit, &state.runner_units);
    let now = crate::now_ms();
    match tokio::task::spawn_blocking(move || {
        build_system(&db_path, started_at_ms, now, local_mode, units)
    })
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(error) => {
            eprintln!("system: blocking task failed: {error}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "failed to read system info",
            )
            .into_response()
        }
    }
}

fn build_system(
    db_path: &Path,
    started_at_ms: i64,
    now: i64,
    local_mode: bool,
    units: Vec<String>,
) -> SystemResponse {
    SystemResponse {
        version: env!("CARGO_PKG_VERSION"),
        build_commit: option_env!("NUWA_BUILD_COMMIT").unwrap_or("dev"),
        started_at_ms,
        uptime_secs: u64::try_from(now.saturating_sub(started_at_ms)).unwrap_or(0) / 1_000,
        db_path: db_path.display().to_string(),
        db_size_bytes: std::fs::metadata(db_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        local_mode,
        services: units
            .into_iter()
            .map(|unit| ServiceState {
                active: service_state(&unit),
                unit,
            })
            .collect(),
    }
}

/// `systemctl is-active <unit>` on hosts with systemd; anything else (spawn
/// failure, a helper that exceeds the 3s budget, empty output) reports `n/a`
/// and is not an error (docs/09 §9.2).
fn service_state(unit: &str) -> String {
    match run_bounded("systemctl", &["is-active", unit], true) {
        Ok(output) => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if state.is_empty() {
                "n/a".to_string()
            } else {
                state
            }
        }
        Err(_) => "n/a".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use shanhaijing::Store;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    fn test_app(
        started_at_ms: i64,
        serve_unit: &str,
        runner_units: &[&str],
    ) -> (tempfile::TempDir, Router, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("s.db");
        let store = Arc::new(Store::open(&db_path).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(None),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms,
            env_path: dir.path().join("nuwa.env"),
            db_path: db_path.clone(),
            cookie_secure: false,
            cookie_secure_never: false,
            serve_unit: serve_unit.to_string(),
            runner_units: runner_units
                .iter()
                .map(|unit| (*unit).to_string())
                .collect(),
        };
        let app = build_app(state);
        (dir, app, db_path)
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

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn system_reports_version_build_paths_and_services() {
        let now = crate::now_ms();
        let started = now - 125_000;
        let (_dir, app, db_path) = test_app(started, "nuwa-serve", &["nuwa-work"]);

        let response = get(&app, "/api/v1/system").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /api/v1/system must be wired"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let value = json_body(response).await;

        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            value["build_commit"],
            option_env!("NUWA_BUILD_COMMIT").unwrap_or("dev")
        );
        assert_eq!(value["started_at_ms"], started);
        let uptime = value["uptime_secs"].as_u64().expect("uptime_secs");
        assert!(
            (125..=3_600).contains(&uptime),
            "uptime must follow started_at_ms: {uptime}"
        );
        assert_eq!(value["db_path"], db_path.display().to_string());
        let size = std::fs::metadata(&db_path).expect("db metadata").len();
        assert_eq!(value["db_size_bytes"], size);
        assert_eq!(value["local_mode"], true, "no UI token means local mode");

        let services = value["services"].as_array().expect("services");
        assert_eq!(services.len(), 2);
        assert_eq!(services[0]["unit"], "nuwa-serve");
        assert_eq!(services[1]["unit"], "nuwa-work");
        for service in services {
            let active = service["active"].as_str().expect("active");
            assert!(
                !active.is_empty(),
                "a service state is always a string, `n/a` when systemd is absent"
            );
        }
    }

    #[tokio::test]
    async fn system_services_follow_the_configured_units() {
        let (_dir, app, _db_path) = test_app(
            crate::now_ms(),
            "nuwa-serve@blue",
            &["nuwa-work@1", "nuwa-work@2"],
        );

        let value = json_body(get(&app, "/api/v1/system").await).await;
        let units: Vec<&str> = value["services"]
            .as_array()
            .expect("services")
            .iter()
            .map(|service| service["unit"].as_str().expect("unit"))
            .collect();
        assert_eq!(units, vec!["nuwa-serve@blue", "nuwa-work@1", "nuwa-work@2"]);
    }
}
