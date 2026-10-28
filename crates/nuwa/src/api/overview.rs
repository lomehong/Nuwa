//! `GET /api/v1/overview` — the console home summary: state funnel, the
//! "needs attention" queue, active runs, the pickable queue, runner health
//! and 24h CI counters (docs/09 §9.2).

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::{
    attention_kind, issue_is_stuck, runner_intervals, DAY_MS, RUNNER_OFFLINE_MS, STATES,
};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{IssueRow, RunnerRow, ShanResult, Store};

const QUEUE_NEXT_LIMIT: u32 = 3;

/// `?repo=` scopes issues, runners and active runs; an empty value means all.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct OverviewQuery {
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct OverviewResponse {
    pub counts: BTreeMap<String, usize>,
    pub attention: Vec<AttentionItem>,
    pub active_runs: Vec<ActiveRun>,
    pub queue: Queue,
    pub health: Health,
    pub ci_24h: CiCounts,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct AttentionItem {
    pub fingerprint: String,
    pub title: String,
    pub state: String,
    pub severity: String,
    pub repo: String,
    pub last_seen_ms: i64,
    pub stuck: bool,
    pub owner_runner_id: Option<String>,
    pub reason_code: String,
    pub reason_text: String,
    pub pr_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ActiveRun {
    pub id: String,
    pub fingerprint: String,
    pub title: String,
    pub repo: String,
    pub runner_id: String,
    pub phase: String,
    pub started_at_ms: i64,
    pub turns: i64,
    /// Uncached + cached tokens for the run.
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Queue {
    pub ready_count: u64,
    pub next: Vec<QueueItem>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct QueueItem {
    pub fingerprint: String,
    pub title: String,
    pub severity: String,
    pub occurrences: u64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Health {
    pub runners_total: usize,
    pub runners_online: usize,
    pub working: usize,
    pub idle: usize,
    pub standing_down: usize,
    pub error: usize,
    pub reports_today: u64,
    /// Historical PR creation/local observation metric, not admission usage.
    pub prs_today: u64,
    /// UTC admission-day slots, including in-flight/ambiguous work, in the
    /// same scope as `prs_today` (runner repos unless `repo` is explicit).
    pub pr_slots_today: u64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct CiCounts {
    pub green: u64,
    pub red: u64,
    pub timeout: u64,
    pub pending: usize,
}

/// Read the console summary. Store work runs on the blocking pool: the
/// handler is async and SQLite calls must not park a runtime worker.
pub async fn overview(
    State(state): State<AppState>,
    Query(query): Query<OverviewQuery>,
) -> Response {
    let now = crate::now_ms();
    let repo = query.repo.filter(|repo| !repo.is_empty());
    let store = std::sync::Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_overview(&store, repo.as_deref(), now)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("overview: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("overview: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read overview",
    )
    .into_response()
}

fn build_overview(store: &Store, repo: Option<&str>, now: i64) -> ShanResult<OverviewResponse> {
    let issues: Vec<IssueRow> = store
        .list_issues()?
        .into_iter()
        .filter(|issue| repo.is_none_or(|repo| issue.repo == repo))
        .collect();
    let by_fingerprint: BTreeMap<&str, &IssueRow> = issues
        .iter()
        .map(|issue| (issue.fingerprint.as_str(), issue))
        .collect();
    // Both the health panel and the per-issue stuck window need the runner
    // rows; read them once per request.
    let runners = store.list_runners()?;
    let active_runs = build_active_runs(store, &by_fingerprint, repo)?;
    Ok(OverviewResponse {
        counts: build_counts(&issues),
        attention: build_attention(store, &issues, &runner_intervals(&runners), now)?,
        queue: build_queue(store, now)?,
        health: build_health(store, &runners, repo, now)?,
        ci_24h: build_ci(store, &active_runs, repo, now)?,
        active_runs,
    })
}

fn build_counts(issues: &[IssueRow]) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = STATES
        .iter()
        .map(|state| ((*state).to_string(), 0))
        .collect();
    for issue in issues {
        *counts.entry(issue.state.clone()).or_insert(0) += 1;
    }
    counts
}

struct AttentionCandidate {
    rank: u8,
    item: AttentionItem,
}

/// Attention = escalated, stuck, rejected, or pr_open with a `ci_timeout`
/// event in the last 24h. Order: escalated > stuck > rejected > ci_timeout,
/// then oldest `last_seen` first (docs/09 §9.2). The predicate and the stuck
/// window follow the owner runner's configured poll interval (docs/09 §8.4)
/// and are shared with the issue list.
fn build_attention(
    store: &Store,
    issues: &[IssueRow],
    runner_intervals: &BTreeMap<&str, u64>,
    now: i64,
) -> ShanResult<Vec<AttentionItem>> {
    let mut candidates = Vec::new();
    for issue in issues {
        let stuck = issue_is_stuck(store, issue, runner_intervals, now)?;
        let Some(kind) = attention_kind(store, issue, stuck, now)? else {
            continue;
        };
        let (reason_code, reason_text) = kind.reason(store, issue)?;
        candidates.push(AttentionCandidate {
            rank: kind.rank(),
            item: AttentionItem {
                fingerprint: issue.fingerprint.clone(),
                title: issue.title.clone(),
                state: issue.state.clone(),
                severity: issue.severity.clone(),
                repo: issue.repo.clone(),
                last_seen_ms: issue.last_seen,
                stuck,
                owner_runner_id: issue.owner_runner_id.clone(),
                reason_code: reason_code.to_string(),
                reason_text: reason_text.to_string(),
                pr_url: issue.pr_url.clone(),
            },
        });
    }
    candidates.sort_by(|left, right| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| left.item.last_seen_ms.cmp(&right.item.last_seen_ms))
            .then_with(|| left.item.fingerprint.cmp(&right.item.fingerprint))
    });
    Ok(candidates
        .into_iter()
        .map(|candidate| candidate.item)
        .collect())
}

fn build_active_runs(
    store: &Store,
    issues: &BTreeMap<&str, &IssueRow>,
    repo: Option<&str>,
) -> ShanResult<Vec<ActiveRun>> {
    let mut runs = store.active_runs()?;
    if let Some(repo) = repo {
        runs.retain(|run| run.repo == repo);
    }
    Ok(runs
        .into_iter()
        .map(|run| ActiveRun {
            title: issues
                .get(run.fingerprint.as_str())
                .map_or_else(String::new, |issue| issue.title.clone()),
            id: run.id,
            fingerprint: run.fingerprint,
            repo: run.repo,
            runner_id: run.runner_id,
            phase: run.phase,
            started_at_ms: run.started_at,
            turns: run.turns,
            tokens: run.tokens_uncached.saturating_add(run.tokens_cached),
        })
        .collect())
}

/// The claim queue is global: it is what the runner pool will pick next, so
/// a console repo filter must not change it.
fn build_queue(store: &Store, now: i64) -> ShanResult<Queue> {
    Ok(Queue {
        ready_count: store.count_ready(now)?,
        next: store
            .ready_issues(now, QUEUE_NEXT_LIMIT)?
            .into_iter()
            .map(|issue| QueueItem {
                fingerprint: issue.fingerprint,
                title: issue.title,
                severity: issue.severity,
                occurrences: issue.occurrences,
            })
            .collect(),
    })
}

fn build_health(
    store: &Store,
    runners: &[RunnerRow],
    repo: Option<&str>,
    now: i64,
) -> ShanResult<Health> {
    let runners: Vec<&RunnerRow> = runners
        .iter()
        .filter(|runner| repo.is_none_or(|repo| runner.repo == repo))
        .collect();
    let mut health = Health {
        runners_total: runners.len(),
        runners_online: 0,
        working: 0,
        idle: 0,
        standing_down: 0,
        error: 0,
        reports_today: store.reports_on_repo(&shanhaijing::utc_day(now), repo)?,
        prs_today: 0,
        pr_slots_today: 0,
    };
    for runner in &runners {
        if !runner_is_online(runner.heartbeat_at, now) {
            continue;
        }
        health.runners_online += 1;
        // Offline rows keep their last status in the DB; counting that stale
        // value as "working" would misreport health. Buckets cover online
        // runners only (`starting` is online but has no bucket).
        match runner.status.as_str() {
            "working" => health.working += 1,
            "idle" => health.idle += 1,
            "standing_down" => health.standing_down += 1,
            "error" => health.error += 1,
            _ => {}
        }
    }
    let day_start = now - now.rem_euclid(DAY_MS);
    let repos: BTreeSet<&str> = match repo {
        Some(repo) => [repo].into_iter().collect(),
        None => runners.iter().map(|runner| runner.repo.as_str()).collect(),
    };
    for repo in repos {
        health.prs_today += store.prs_opened_since(repo, day_start)?;
        health.pr_slots_today += store.pr_slots_used(repo, day_start)?;
    }
    Ok(health)
}

fn runner_is_online(heartbeat_at: i64, now: i64) -> bool {
    now.saturating_sub(heartbeat_at) <= RUNNER_OFFLINE_MS
}

fn build_ci(
    store: &Store,
    active_runs: &[ActiveRun],
    repo: Option<&str>,
    now: i64,
) -> ShanResult<CiCounts> {
    let since = now - DAY_MS;
    Ok(CiCounts {
        green: store.count_events_since_repo(&["ci_green"], since, repo)?,
        red: store.count_events_since_repo(&["ci_red"], since, repo)?,
        timeout: store.count_events_since_repo(&["ci_timeout"], since, repo)?,
        pending: active_runs.iter().filter(|run| run.phase == "ci").count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_include_every_state_even_when_zero() {
        let counts = build_counts(&[]);
        assert_eq!(counts.len(), STATES.len());
        for state in STATES {
            assert_eq!(counts[state], 0, "{state}");
        }
    }

    #[test]
    fn health_counts_repo_slots_once_and_keeps_pr_creation_separate() {
        let store = Store::open(":memory:").expect("store");
        let now = DAY_MS + 1_000;
        let claim = store
            .claim_delivery(
                &shanhaijing::DeliveryStart {
                    id: "pending".into(),
                    run_id: None,
                    fingerprint: "pending-fp".into(),
                    repo: "app/alpha".into(),
                    branch: "agent/fix/pending".into(),
                    base: "main".into(),
                    head_sha: "pending-sha".into(),
                    now,
                },
                "owner",
                DAY_MS,
            )
            .expect("claim")
            .expect("owned");
        assert!(store
            .reserve_pr_slot_owned(&claim, 5, now)
            .expect("reserve"));
        for id in ["first", "second"] {
            store
                .upsert_runner(&shanhaijing::RunnerUpsert {
                    id: id.into(),
                    repo: "app/alpha".into(),
                    host: "host".into(),
                    pid: None,
                    started_at: now,
                    heartbeat_at: now,
                    status: "idle".into(),
                    provider: None,
                    model: None,
                    daily_pr_limit: Some(5),
                    repo_path: None,
                    service_unit: None,
                    config_json: "{}".into(),
                })
                .expect("runner");
        }
        let runners = store.list_runners().expect("runners");
        for repo in [None, Some("app/alpha")] {
            let health = build_health(&store, &runners, repo, now).expect("health");
            assert_eq!(health.pr_slots_today, 1);
            assert_eq!(health.prs_today, 0, "no PR creation evidence");
        }
        let other = build_health(&store, &runners, Some("app/other"), now).expect("other repo");
        assert_eq!(other.pr_slots_today, 0);
    }

    #[test]
    fn stuck_attention_uses_the_owner_runner_interval() {
        use shanhaijing::{ReportOutcome, RunStart, RunnerUpsert, Signal, State};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 10_000_000_000;
        // Older than the 300s floor, younger than the 1800s slow-runner window.
        let stale = now - 500_000;

        let report = |title: &str| -> String {
            match store
                .report_signal(
                    &Signal {
                        repo: "example.com/app".to_string(),
                        language: "go".to_string(),
                        title: title.to_string(),
                        severity: "high".to_string(),
                        message: None,
                        log_window: Vec::new(),
                        stack_trace: format!("panic: {title}"),
                        ..Default::default()
                    },
                    stale,
                )
                .expect("report")
            {
                ReportOutcome::NewIssue { fingerprint } => fingerprint,
                other => panic!("expected a new issue, got {other:?}"),
            }
        };
        let own = |fingerprint: &str, runner_id: &str| {
            store
                .set_state(fingerprint, State::Triggered, stale, None)
                .expect("triggered");
            store
                .set_state(fingerprint, State::Fixing, stale, None)
                .expect("fixing");
            // start_run stamps owner_runner_id on the issue.
            store
                .start_run(&RunStart {
                    id: format!("run-{runner_id}"),
                    fingerprint: fingerprint.to_string(),
                    runner_id: runner_id.to_string(),
                    repo: "example.com/app".to_string(),
                    started_at: stale,
                    provider: None,
                    model: None,
                })
                .expect("start run");
        };
        let runner = |id: &str, config_json: &str| {
            store
                .upsert_runner(&RunnerUpsert {
                    id: id.to_string(),
                    repo: "example.com/app".to_string(),
                    host: "test-host".to_string(),
                    pid: None,
                    started_at: stale,
                    heartbeat_at: stale,
                    status: "working".to_string(),
                    provider: None,
                    model: None,
                    daily_pr_limit: None,
                    repo_path: None,
                    service_unit: None,
                    config_json: config_json.to_string(),
                })
                .expect("upsert runner");
        };

        // 600s interval → 1800s window: 500s of silence is not stuck.
        runner("runner-slow", r#"{"interval_secs":600}"#);
        let slow = report("slow interval");
        own(&slow, "runner-slow");
        // No config → 30s interval → 300s floor: the same silence is stuck.
        runner("runner-default", "{}");
        let default = report("default interval");
        own(&default, "runner-default");

        let issues = store.list_issues().expect("issues");
        let runners = store.list_runners().expect("runners");
        let attention =
            build_attention(&store, &issues, &runner_intervals(&runners), now).expect("attention");

        assert!(
            attention.iter().all(|item| item.fingerprint != slow),
            "a 500s-old issue owned by a 600s-interval runner must not be attention: {attention:?}"
        );
        let stuck = attention
            .iter()
            .find(|item| item.fingerprint == default)
            .expect("the default-interval issue stays attention");
        assert!(stuck.stuck);
        assert_eq!(stuck.reason_code, "stuck");
    }

    #[test]
    fn runner_online_uses_the_ninety_second_window() {
        let now = 1_000_000_000;
        assert!(runner_is_online(now, now));
        assert!(runner_is_online(now - RUNNER_OFFLINE_MS + 1, now));
        assert!(
            runner_is_online(now - RUNNER_OFFLINE_MS, now),
            "the boundary is inclusive"
        );
        assert!(!runner_is_online(now - RUNNER_OFFLINE_MS - 1, now));
        assert!(
            runner_is_online(now + 5_000, now),
            "a future heartbeat (clock skew) stays online"
        );
    }
}
