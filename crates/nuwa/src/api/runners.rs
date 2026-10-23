//! `GET /api/v1/runners` — runner fleet health for the console (docs/09 §9.2):
//! heartbeat freshness, per-repo daily PR usage and the effective config the
//! runner last reported.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::api::common::{DAY_MS, RUNNER_OFFLINE_MS};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{RunnerRow, ShanResult, Store};

#[derive(Debug, Serialize)]
pub struct RunnerListResponse {
    pub data: Vec<RunnerDto>,
}

#[derive(Debug, Serialize)]
pub struct RunnerDto {
    pub id: String,
    pub repo: String,
    pub host: String,
    pub pid: Option<i64>,
    #[serde(rename = "started_at_ms")]
    pub started_at: i64,
    #[serde(rename = "heartbeat_at_ms")]
    pub heartbeat_at: i64,
    #[serde(rename = "progress_at_ms")]
    pub progress_at: i64,
    pub status: String,
    pub offline: bool,
    pub current_fingerprint: Option<String>,
    pub phase: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub daily_pr_limit: Option<i64>,
    pub prs_today: u64,
    pub repo_path: Option<String>,
    pub service_unit: Option<String>,
    pub last_error: Option<String>,
    pub effective: EffectiveConfig,
}

/// The config a runner reported in its last heartbeat. Missing/malformed keys
/// stay `null` instead of guessing a default: the console must show what the
/// runner actually runs with.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct EffectiveConfig {
    pub interval_secs: Option<u64>,
    pub ci_timeout_secs: Option<u64>,
    pub max_turns: Option<u64>,
    pub max_tokens: Option<u64>,
    pub failing_check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduce_check: Option<String>,
    pub forbidden_paths_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_first_n: Option<u64>,
    /// `nuwa.repo.json` 的 skills 白名单；空数组 = 自动选择。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}

/// `GET /api/v1/runners` — every registered runner, newest heartbeat first.
pub async fn list(State(state): State<AppState>) -> Response {
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_runners(&store, now)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("runners: store read failed: {error}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "failed to read runners",
            )
            .into_response()
        }
        Err(error) => {
            eprintln!("runners: blocking task failed: {error}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "failed to read runners",
            )
            .into_response()
        }
    }
}

fn build_runners(store: &Store, now: i64) -> ShanResult<RunnerListResponse> {
    let day_start = now - now.rem_euclid(DAY_MS);
    // Quota is per repo; runners of the same repo share one count.
    let mut prs_by_repo: BTreeMap<String, u64> = BTreeMap::new();
    let mut data = Vec::new();
    for runner in store.list_runners()? {
        let prs_today = match prs_by_repo.get(&runner.repo) {
            Some(count) => *count,
            None => {
                let count = store.prs_opened_since(&runner.repo, day_start)?;
                prs_by_repo.insert(runner.repo.clone(), count);
                count
            }
        };
        data.push(runner_dto(&runner, now, prs_today));
    }
    Ok(RunnerListResponse { data })
}

fn runner_dto(runner: &RunnerRow, now: i64, prs_today: u64) -> RunnerDto {
    RunnerDto {
        id: runner.id.clone(),
        repo: runner.repo.clone(),
        host: runner.host.clone(),
        pid: runner.pid,
        started_at: runner.started_at,
        heartbeat_at: runner.heartbeat_at,
        progress_at: runner.progress_at,
        status: runner.status.clone(),
        offline: now.saturating_sub(runner.heartbeat_at) > RUNNER_OFFLINE_MS,
        current_fingerprint: runner.current_fingerprint.clone(),
        phase: runner.phase.clone(),
        provider: runner.provider.clone(),
        model: runner.model.clone(),
        daily_pr_limit: runner.daily_pr_limit,
        prs_today,
        repo_path: runner.repo_path.clone(),
        service_unit: runner.service_unit.clone(),
        last_error: runner.last_error.clone(),
        effective: effective_config(&runner.config_json),
    }
}

pub(crate) fn effective_config(config_json: &str) -> EffectiveConfig {
    let Ok(config) = serde_json::from_str::<serde_json::Value>(config_json) else {
        return EffectiveConfig::default();
    };
    let number = |key: &str| config.get(key).and_then(serde_json::Value::as_u64);
    EffectiveConfig {
        interval_secs: number("interval_secs"),
        ci_timeout_secs: number("ci_timeout_secs"),
        max_turns: number("max_turns"),
        max_tokens: number("max_tokens"),
        failing_check: config
            .get("failing_check")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        reproduce_check: config
            .get("reproduce_check")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        forbidden_paths_count: number("forbidden_paths_count"),
        max_attempts: number("max_attempts"),
        draft_first_n: number("draft_first_n"),
        skills: config
            .get("skills")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    const APP: &str = "example.com/app";
    const OTHER: &str = "example.com/other";
    const DAY_MS: i64 = 86_400_000;
    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
    const LIVE_CONFIG: &str = r#"{"interval_secs":600,"ci_timeout_secs":900,"max_turns":30,"max_tokens":1500000,"failing_check":"go test ./...","forbidden_paths_count":3}"#;

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("s.db");
        let store = Arc::new(shanhaijing::Store::open(&db_path).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(Some("ingest-secret".into())),
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
        (dir, state)
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

    fn header_str(response: &Response, name: header::HeaderName) -> String {
        response
            .headers()
            .get(&name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("ascii header")
            .to_string()
    }

    fn assert_api_headers(response: &Response) {
        assert_eq!(header_str(response, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_str(response, header::CONTENT_SECURITY_POLICY),
            STRICT_CSP
        );
        assert_eq!(
            header_str(response, header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
    }

    fn report(store: &shanhaijing::Store, title: &str, repo: &str, at: i64) -> String {
        let signal = shanhaijing::Signal {
            repo: repo.to_string(),
            language: "go".to_string(),
            title: title.to_string(),
            severity: "high".to_string(),
            message: Some(format!("boom {title}")),
            log_window: Vec::new(),
            stack_trace: format!(
                "panic: {title}\ngoroutine 1 [running]:\nexample.com/app.Fn_{}()\n\t/app/x.go:1 +0x1",
                title.replace(' ', "_")
            ),
        };
        match store.report_signal(&signal, at).expect("report signal") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{title} must be a new issue, got {other:?}"),
        }
    }

    fn upsert_runner(
        store: &shanhaijing::Store,
        id: &str,
        repo: &str,
        started_at: i64,
        heartbeat_at: i64,
        status: &str,
        config_json: &str,
    ) {
        store
            .upsert_runner(&shanhaijing::RunnerUpsert {
                id: id.to_string(),
                repo: repo.to_string(),
                host: "test-host".to_string(),
                pid: Some(4242),
                started_at,
                heartbeat_at,
                status: status.to_string(),
                provider: Some("zai".to_string()),
                model: Some("glm-5.3-flash".to_string()),
                daily_pr_limit: Some(5),
                repo_path: Some("/opt/nuwa/repo/app".to_string()),
                service_unit: Some("nuwa-work".to_string()),
                config_json: config_json.to_string(),
            })
            .expect("upsert runner");
    }

    fn heartbeat(
        store: &shanhaijing::Store,
        id: &str,
        heartbeat_at: i64,
        status: &str,
        current_fingerprint: Option<&str>,
        phase: Option<&str>,
        last_error: Option<&str>,
    ) {
        store
            .heartbeat_runner(
                id,
                &shanhaijing::RunnerHeartbeat {
                    heartbeat_at,
                    status: status.to_string(),
                    current_fingerprint: current_fingerprint.map(str::to_string),
                    phase: phase.map(str::to_string),
                    config_json: None,
                    last_error: last_error.map(str::to_string),
                },
            )
            .expect("runner heartbeat");
    }

    #[tokio::test]
    async fn runners_return_health_quota_and_effective_config() {
        let (_dir, state) = test_state();
        let now = crate::now_ms();
        let day_start = now - now.rem_euclid(DAY_MS);

        // Two APP PRs opened today (issue fallback + run ledger) and one in
        // another repo: `prs_today` must follow each runner's own repo.
        let fp_a = report(&state.store, "alpha", APP, now - 50_000);
        state
            .store
            .set_pr(
                &fp_a,
                "https://github.com/acme/app/pull/1",
                day_start + 1_000,
            )
            .expect("issue pr");
        let fp_b = report(&state.store, "beta", APP, now - 40_000);
        state
            .store
            .start_run(&shanhaijing::RunStart {
                id: "run-b".to_string(),
                fingerprint: fp_b,
                runner_id: "runner-live".to_string(),
                repo: APP.to_string(),
                started_at: now - 30_000,
                provider: None,
                model: None,
            })
            .expect("start run");
        state
            .store
            .update_run(
                "run-b",
                &shanhaijing::RunUpdate {
                    pr_url: Some("https://github.com/acme/app/pull/2".to_string()),
                    pr_opened_at: Some(day_start + 2_000),
                    ..shanhaijing::RunUpdate::default()
                },
            )
            .expect("update run");
        let fp_c = report(&state.store, "gamma", OTHER, now - 20_000);
        state
            .store
            .set_pr(
                &fp_c,
                "https://github.com/acme/other/pull/3",
                day_start + 3_000,
            )
            .expect("other pr");

        upsert_runner(
            &state.store,
            "runner-live",
            APP,
            now - 600_000,
            now - 1_000,
            "working",
            LIVE_CONFIG,
        );
        heartbeat(
            &state.store,
            "runner-live",
            now - 1_000,
            "working",
            Some(&fp_a),
            Some("repair"),
            None,
        );
        upsert_runner(
            &state.store,
            "runner-stale",
            OTHER,
            now - 600_000,
            now - 200_000,
            "idle",
            "{}",
        );
        heartbeat(
            &state.store,
            "runner-stale",
            now - 200_000,
            "idle",
            None,
            None,
            Some("repair failed"),
        );

        let app = build_app(state);
        let response = get(&app, "/api/v1/runners").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        let data = value["data"].as_array().expect("runner data");
        assert_eq!(data.len(), 2);
        let live = &data[0];
        assert_eq!(live["id"].as_str(), Some("runner-live"));
        assert_eq!(live["repo"].as_str(), Some(APP));
        assert_eq!(live["host"].as_str(), Some("test-host"));
        assert_eq!(live["pid"].as_i64(), Some(4242));
        assert_eq!(live["started_at_ms"].as_i64(), Some(now - 600_000));
        assert_eq!(live["heartbeat_at_ms"].as_i64(), Some(now - 1_000));
        assert_eq!(live["status"].as_str(), Some("working"));
        assert_eq!(live["offline"].as_bool(), Some(false));
        assert_eq!(live["current_fingerprint"].as_str(), Some(fp_a.as_str()));
        assert_eq!(live["phase"].as_str(), Some("repair"));
        assert_eq!(live["provider"].as_str(), Some("zai"));
        assert_eq!(live["model"].as_str(), Some("glm-5.3-flash"));
        assert_eq!(live["daily_pr_limit"].as_i64(), Some(5));
        assert_eq!(
            live["prs_today"].as_u64(),
            Some(2),
            "both APP PRs count for the APP runner"
        );
        assert_eq!(live["repo_path"].as_str(), Some("/opt/nuwa/repo/app"));
        assert_eq!(live["service_unit"].as_str(), Some("nuwa-work"));
        assert_eq!(live["last_error"], serde_json::Value::Null);
        assert_eq!(
            live["effective"],
            serde_json::json!({
                "interval_secs": 600,
                "ci_timeout_secs": 900,
                "max_turns": 30,
                "max_tokens": 1_500_000,
                "failing_check": "go test ./...",
                "forbidden_paths_count": 3,
            })
        );

        let stale = &data[1];
        assert_eq!(stale["id"].as_str(), Some("runner-stale"));
        assert_eq!(stale["repo"].as_str(), Some(OTHER));
        assert_eq!(stale["heartbeat_at_ms"].as_i64(), Some(now - 200_000));
        assert_eq!(stale["offline"].as_bool(), Some(true));
        assert_eq!(stale["status"].as_str(), Some("idle"));
        assert_eq!(stale["current_fingerprint"], serde_json::Value::Null);
        assert_eq!(stale["phase"], serde_json::Value::Null);
        assert_eq!(stale["last_error"].as_str(), Some("repair failed"));
        assert_eq!(stale["prs_today"].as_u64(), Some(1));
        assert_eq!(
            stale["effective"],
            serde_json::json!({
                "interval_secs": null,
                "ci_timeout_secs": null,
                "max_turns": null,
                "max_tokens": null,
                "failing_check": null,
                "forbidden_paths_count": null,
            }),
            "a config without the effective keys degrades to nulls"
        );
    }

    #[tokio::test]
    async fn runners_empty_store_returns_an_empty_list() {
        let (_dir, state) = test_state();
        let app = build_app(state);
        let response = get(&app, "/api/v1/runners").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert!(value["data"].as_array().expect("runner data").is_empty());
    }
}
