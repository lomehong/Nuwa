//! `GET /api/v1/benchmarks` — repair-benchmark runs and per-scenario results.
//!
//! Benchmarks are executed by `nuwa benchmark` (or CI), not by serve: the run
//! needs a dedicated process for provider keys and the harness' turn budget.
//! The console is the read/analysis surface.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::benchmark::{self, ScenarioMeta};
use crate::serve::AppState;
use shanhaijing::{BenchmarkResultRow, BenchmarkRunRow, Store};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkRunsResponse {
    pub data: Vec<BenchmarkRunDto>,
    pub scenarios: Vec<ScenarioMeta>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkRunDto {
    pub id: String,
    #[serde(rename = "started_at_ms")]
    pub started_at: i64,
    #[serde(rename = "finished_at_ms")]
    pub finished_at: Option<i64>,
    pub provider: String,
    pub model: Option<String>,
    pub mode: String,
    pub status: String,
    pub scenarios: i64,
    pub repaired: i64,
    pub claimed: i64,
    pub false_positive: i64,
    pub delivery_ok: i64,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
}

impl From<BenchmarkRunRow> for BenchmarkRunDto {
    fn from(run: BenchmarkRunRow) -> Self {
        Self {
            id: run.id,
            started_at: run.started_at,
            finished_at: run.finished_at,
            provider: run.provider,
            model: run.model,
            mode: run.mode,
            status: run.status,
            scenarios: run.scenarios,
            repaired: run.repaired,
            claimed: run.claimed,
            false_positive: run.false_positive,
            delivery_ok: run.delivery_ok,
            turns: run.turns,
            tokens_uncached: run.tokens_uncached,
            tokens_cached: run.tokens_cached,
            duration_ms: run.duration_ms,
            error: run.error,
        }
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkDetailResponse {
    pub run: BenchmarkRunDto,
    pub results: Vec<BenchmarkResultDto>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkResultDto {
    pub scenario: String,
    pub language: String,
    pub expected_fixed: bool,
    pub repaired: bool,
    pub claimed_fixed: bool,
    pub delivery_ok: bool,
    /// `repaired == expected_fixed`; false marks a benchmark signal that needs
    /// attention (an unexpected repair or a missed positive scenario).
    pub matches_expectation: bool,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    pub duration_ms: i64,
    pub root_cause: Option<String>,
    pub final_report: Option<String>,
    pub error: Option<String>,
}

impl From<BenchmarkResultRow> for BenchmarkResultDto {
    fn from(result: BenchmarkResultRow) -> Self {
        Self {
            matches_expectation: result.repaired == result.expected_fixed,
            scenario: result.scenario,
            language: result.language,
            expected_fixed: result.expected_fixed,
            repaired: result.repaired,
            claimed_fixed: result.claimed_fixed,
            delivery_ok: result.delivery_ok,
            turns: result.turns,
            tokens_uncached: result.tokens_uncached,
            tokens_cached: result.tokens_cached,
            duration_ms: result.duration_ms,
            root_cause: result.root_cause,
            final_report: result.final_report,
            error: result.error,
        }
    }
}

pub async fn list(State(state): State<AppState>, Query(query): Query<ListQuery>) -> Response {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_list(&store, limit)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("benchmarks: store read failed: {error}");
            internal_error("read")
        }
        Err(error) => {
            eprintln!("benchmarks: blocking task failed: {error}");
            internal_error("read")
        }
    }
}

fn build_list(store: &Store, limit: u32) -> anyhow::Result<BenchmarkRunsResponse> {
    Ok(BenchmarkRunsResponse {
        data: store
            .list_benchmark_runs(limit)?
            .into_iter()
            .map(BenchmarkRunDto::from)
            .collect(),
        scenarios: benchmark::catalog()?,
    })
}

pub async fn detail(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || {
        let Some(run) = store.get_benchmark_run(&id)? else {
            return Ok::<_, shanhaijing::ShanError>(None);
        };
        let results = store.benchmark_results(&id)?;
        Ok(Some(BenchmarkDetailResponse {
            run: BenchmarkRunDto::from(run),
            results: results.into_iter().map(BenchmarkResultDto::from).collect(),
        }))
    })
    .await
    {
        Ok(Ok(Some(body))) => Json(body).into_response(),
        Ok(Ok(None)) => ApiError::not_found("benchmark run not found").into_response(),
        Ok(Err(error)) => {
            eprintln!("benchmark detail: store read failed: {error}");
            internal_error("read")
        }
        Err(error) => {
            eprintln!("benchmark detail: blocking task failed: {error}");
            internal_error("read")
        }
    }
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ScenariosResponse {
    pub data: Vec<ScenarioMeta>,
}

pub async fn scenarios() -> Response {
    match tokio::task::spawn_blocking(benchmark::catalog).await {
        Ok(Ok(data)) => Json(ScenariosResponse { data }).into_response(),
        Ok(Err(error)) => {
            eprintln!("benchmark scenarios: {error:#}");
            internal_error("read")
        }
        Err(error) => {
            eprintln!("benchmark scenarios: blocking task failed: {error}");
            internal_error("read")
        }
    }
}

fn internal_error(action: &'static str) -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        format!("failed to {action} benchmark runs"),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shanhaijing::{BenchmarkFinish, BenchmarkResultInput, BenchmarkStart};

    fn seed(store: &Store) {
        store
            .start_benchmark_run(&BenchmarkStart {
                id: "bench-1".into(),
                started_at: 10,
                provider: "faux".into(),
                model: Some("faux".into()),
                mode: "faux".into(),
                scenarios: 2,
            })
            .expect("start");
        store
            .insert_benchmark_result(&BenchmarkResultInput {
                run_id: "bench-1".into(),
                scenario: "node-empty-average".into(),
                language: "node".into(),
                expected_fixed: true,
                repaired: true,
                claimed_fixed: true,
                delivery_ok: true,
                turns: 2,
                tokens_uncached: 100,
                tokens_cached: 0,
                duration_ms: 500,
                root_cause: Some("ROOT CAUSE: empty input".into()),
                final_report: None,
                error: None,
            })
            .expect("result");
        store
            .insert_benchmark_result(&BenchmarkResultInput {
                run_id: "bench-1".into(),
                scenario: "node-noop-claim".into(),
                language: "node".into(),
                expected_fixed: false,
                repaired: false,
                claimed_fixed: true,
                delivery_ok: false,
                turns: 2,
                tokens_uncached: 50,
                tokens_cached: 0,
                duration_ms: 300,
                root_cause: None,
                final_report: None,
                error: None,
            })
            .expect("result");
        store
            .finish_benchmark_run(
                "bench-1",
                &BenchmarkFinish {
                    finished_at: 20,
                    status: "completed".into(),
                    repaired: 1,
                    claimed: 2,
                    false_positive: 1,
                    delivery_ok: 1,
                    turns: 4,
                    tokens_uncached: 150,
                    tokens_cached: 0,
                    duration_ms: 800,
                    error: None,
                },
            )
            .expect("finish");
    }

    #[test]
    fn list_and_detail_expose_scores_and_expectation_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        seed(&store);
        let list = build_list(&store, 20).expect("list");
        assert_eq!(list.data.len(), 1);
        assert_eq!(list.data[0].false_positive, 1);
        assert!(!list.scenarios.is_empty(), "scenario catalog is embedded");
        assert!(list.scenarios.iter().any(|scenario| !scenario.expect_fixed));

        let run = store
            .get_benchmark_run("bench-1")
            .expect("run")
            .expect("present");
        let results: Vec<BenchmarkResultDto> = store
            .benchmark_results("bench-1")
            .expect("results")
            .into_iter()
            .map(BenchmarkResultDto::from)
            .collect();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.matches_expectation));
        let body = BenchmarkDetailResponse {
            run: BenchmarkRunDto::from(run),
            results,
        };
        assert_eq!(body.run.claimed, 2);
        assert_eq!(body.run.false_positive, 1);
    }
}
