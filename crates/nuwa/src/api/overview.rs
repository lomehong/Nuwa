//! `GET /api/v1/overview` — the console home summary: state funnel, the
//! "needs attention" queue, active runs, the pickable queue, runner health
//! and 24h CI counters (docs/09 §9.2).

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{EventQuery, IssueRow, RunnerRow, ShanResult, Store};

/// A runner is offline once its heartbeat is older than this (docs/09 §9.2).
const RUNNER_OFFLINE_MS: i64 = 90_000;
/// Poll interval assumed when a runner's `config_json` has no usable
/// `interval_secs` (docs/09 §8.4).
const DEFAULT_INTERVAL_SECS: u64 = 30;
/// Stuck window floor: `stuck_after = max(3 × interval, 300s)` (docs/09 §8.4).
const STUCK_AFTER_FLOOR_MS: i64 = 300_000;
/// `NUWA_INTERVAL` is documented at ≤ 86400s, so `3 × interval` tops out at
/// 72h; clamp there so a corrupt config cannot disable stuck detection.
const STUCK_AFTER_CEILING_MS: i64 = 259_200_000;
const DAY_MS: i64 = 86_400_000;
const QUEUE_NEXT_LIMIT: u32 = 3;

/// Funnel keys; every key is present even when its count is zero.
const STATES: [&str; 7] = [
    "new",
    "triggered",
    "fixing",
    "pr_open",
    "merged",
    "rejected",
    "escalated",
];

const REASON_REPAIR_FAILED: (&str, &str) = ("repair_failed", "修复 runner 失败，需要复核");
const REASON_DELIVERY_FAILED: (&str, &str) = ("delivery_failed", "验证/交付失败，需要复核");
const REASON_BUDGET_EXCEEDED: (&str, &str) = ("budget_exceeded", "token 预算超限，需要复核");
const REASON_CI_RED: (&str, &str) = ("ci_red", "CI 红灯，PR 已关闭");
const REASON_PR_CLOSED: (&str, &str) = ("pr_closed", "PR 未合并被关闭");
const REASON_STUCK: (&str, &str) = ("stuck", "疑似卡单，runner 心跳已过期");
const REASON_CI_TIMEOUT: (&str, &str) = ("ci_timeout", "CI 超时，需要人工查看");

/// `?repo=` scopes issues, runners and active runs; an empty value means all.
#[derive(Debug, Default, Deserialize)]
pub struct OverviewQuery {
    #[serde(default)]
    pub repo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub counts: BTreeMap<String, usize>,
    pub attention: Vec<AttentionItem>,
    pub active_runs: Vec<ActiveRun>,
    pub queue: Queue,
    pub health: Health,
    pub ci_24h: CiCounts,
}

#[derive(Debug, Serialize)]
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
pub struct Queue {
    pub ready_count: u64,
    pub next: Vec<QueueItem>,
}

#[derive(Debug, Serialize)]
pub struct QueueItem {
    pub fingerprint: String,
    pub title: String,
    pub severity: String,
    pub occurrences: u64,
}

#[derive(Debug, Serialize)]
pub struct Health {
    pub runners_total: usize,
    pub runners_online: usize,
    pub working: usize,
    pub idle: usize,
    pub standing_down: usize,
    pub error: usize,
    pub reports_today: u64,
    pub prs_today: u64,
}

#[derive(Debug, Serialize)]
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
        .filter(|issue| repo.map_or(true, |repo| issue.repo == repo))
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
        ci_24h: build_ci(store, &active_runs, now)?,
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
/// then oldest `last_seen` first (docs/09 §9.2). The stuck window follows the
/// owning runner's configured poll interval (docs/09 §8.4).
fn build_attention(
    store: &Store,
    issues: &[IssueRow],
    runner_intervals: &BTreeMap<&str, u64>,
    now: i64,
) -> ShanResult<Vec<AttentionItem>> {
    let mut candidates = Vec::new();
    for issue in issues {
        let (rank, stuck, (reason_code, reason_text)) = match issue.state.as_str() {
            "escalated" | "rejected" => {
                let detail = store
                    .latest_event(&issue.fingerprint)?
                    .map(|event| event.detail)
                    .unwrap_or_default();
                let rank = if issue.state == "escalated" { 0 } else { 2 };
                (rank, false, reason_for_detail(&detail, &issue.state))
            }
            "triggered" | "fixing" => {
                let interval_secs = issue
                    .owner_runner_id
                    .as_deref()
                    .and_then(|owner| runner_intervals.get(owner).copied())
                    .unwrap_or(DEFAULT_INTERVAL_SECS);
                if !store.stuck(&issue.fingerprint, now, stuck_after_ms(interval_secs))? {
                    continue;
                }
                (1, true, REASON_STUCK)
            }
            "pr_open" => {
                if !ci_timeout_within_day(store, &issue.fingerprint, now)? {
                    continue;
                }
                (3, false, REASON_CI_TIMEOUT)
            }
            _ => continue,
        };
        candidates.push(AttentionCandidate {
            rank,
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

/// Owner runner id → configured poll interval, used to size each issue's
/// stuck window. Runners with absent/malformed configs keep the 30s default.
fn runner_intervals(runners: &[RunnerRow]) -> BTreeMap<&str, u64> {
    runners
        .iter()
        .map(|runner| {
            (
                runner.id.as_str(),
                runner_interval_secs(&runner.config_json),
            )
        })
        .collect()
}

/// Read `interval_secs` from a runner's effective `config_json`; anything
/// missing, malformed or non-numeric falls back to the 30s default.
fn runner_interval_secs(config_json: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(config_json)
        .ok()
        .and_then(|config| {
            config
                .get("interval_secs")
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

/// `stuck_after = max(3 × interval, 300s)` (docs/09 §8.4), saturated and
/// clamped so a corrupt interval can neither overflow nor disable detection.
fn stuck_after_ms(interval_secs: u64) -> i64 {
    let window_ms = interval_secs.saturating_mul(3).saturating_mul(1_000);
    let window_ms = i64::try_from(window_ms).unwrap_or(i64::MAX);
    window_ms.clamp(STUCK_AFTER_FLOOR_MS, STUCK_AFTER_CEILING_MS)
}

/// Map the newest event detail to a stable reason. Escalation details carry
/// a phase prefix (`repair_failed: …`, docs/09 §8.3); a rejected issue
/// without one is a PR closed by `sync`.
fn reason_for_detail(detail: &str, state: &str) -> (&'static str, &'static str) {
    for (prefix, reason) in [
        ("repair_failed:", REASON_REPAIR_FAILED),
        ("delivery_failed:", REASON_DELIVERY_FAILED),
        ("budget_exceeded:", REASON_BUDGET_EXCEEDED),
        ("ci_red:", REASON_CI_RED),
    ] {
        if detail.starts_with(prefix) {
            return reason;
        }
    }
    if state == "rejected" {
        REASON_PR_CLOSED
    } else {
        // Legacy escalations without a phase prefix still mean the repair
        // pipeline gave up; repair_failed is the generic bucket.
        REASON_REPAIR_FAILED
    }
}

fn ci_timeout_within_day(store: &Store, fingerprint: &str, now: i64) -> ShanResult<bool> {
    let query = EventQuery {
        kinds: vec!["ci_timeout".to_string()],
        fingerprint: Some(fingerprint.to_string()),
        from_ms: Some(now - DAY_MS),
        ..EventQuery::default()
    };
    Ok(!store.list_events(&query, 1)?.is_empty())
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
        .filter(|runner| repo.map_or(true, |repo| runner.repo == repo))
        .collect();
    let mut health = Health {
        runners_total: runners.len(),
        runners_online: 0,
        working: 0,
        idle: 0,
        standing_down: 0,
        error: 0,
        reports_today: store.reports_on(&shanhaijing::utc_day(now))?,
        prs_today: 0,
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
    health.prs_today = match repo {
        Some(repo) => store.prs_opened_since(repo, day_start)?,
        None => {
            let repos: BTreeSet<&str> = runners.iter().map(|runner| runner.repo.as_str()).collect();
            let mut total = 0;
            for repo in repos {
                total += store.prs_opened_since(repo, day_start)?;
            }
            total
        }
    };
    Ok(health)
}

fn runner_is_online(heartbeat_at: i64, now: i64) -> bool {
    now.saturating_sub(heartbeat_at) <= RUNNER_OFFLINE_MS
}

fn build_ci(store: &Store, active_runs: &[ActiveRun], now: i64) -> ShanResult<CiCounts> {
    let since = now - DAY_MS;
    Ok(CiCounts {
        green: store.count_events_since(&["ci_green"], since)?,
        red: store.count_events_since(&["ci_red"], since)?,
        timeout: store.count_events_since(&["ci_timeout"], since)?,
        pending: active_runs.iter().filter(|run| run.phase == "ci").count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_for_detail_maps_every_documented_prefix() {
        let cases = [
            (
                "repair_failed: boom",
                "escalated",
                "repair_failed",
                "修复 runner 失败，需要复核",
            ),
            (
                "delivery_failed: verify failed",
                "escalated",
                "delivery_failed",
                "验证/交付失败，需要复核",
            ),
            (
                "budget_exceeded: 900 > 800",
                "escalated",
                "budget_exceeded",
                "token 预算超限，需要复核",
            ),
            (
                "ci_red: CI red — PR #7 closed",
                "rejected",
                "ci_red",
                "CI 红灯，PR 已关闭",
            ),
        ];
        for (detail, state, code, text) in cases {
            assert_eq!(
                reason_for_detail(detail, state),
                (code, text),
                "detail {detail:?}"
            );
        }
    }

    #[test]
    fn reason_for_detail_requires_the_prefix_and_falls_back() {
        // A prefix must start the detail, not merely appear inside it.
        for detail in [
            "repair failed: repair_failed: boom",
            "repair_failedness",
            "repair_failed",
        ] {
            assert_eq!(
                reason_for_detail(detail, "escalated"),
                REASON_REPAIR_FAILED,
                "detail {detail:?}"
            );
        }
        // Rejections without a phase prefix come from `sync` closing a PR.
        assert_eq!(reason_for_detail("", "rejected"), REASON_PR_CLOSED);
        assert_eq!(
            reason_for_detail("PR closed without merge", "rejected"),
            REASON_PR_CLOSED
        );
    }

    #[test]
    fn counts_include_every_state_even_when_zero() {
        let counts = build_counts(&[]);
        assert_eq!(counts.len(), STATES.len());
        for state in STATES {
            assert_eq!(counts[state], 0, "{state}");
        }
    }

    #[test]
    fn stuck_window_follows_the_runner_interval_with_a_300s_floor() {
        assert_eq!(stuck_after_ms(30), STUCK_AFTER_FLOOR_MS);
        assert_eq!(stuck_after_ms(600), 1_800_000);
        assert_eq!(stuck_after_ms(u64::MAX), STUCK_AFTER_CEILING_MS);
        assert_eq!(
            runner_interval_secs(r#"{"interval_secs":600}"#),
            600,
            "the configured interval is honoured"
        );
        for config in ["{}", "", "not json", r#"{"interval_secs":"600"}"#, "[]"] {
            assert_eq!(
                runner_interval_secs(config),
                DEFAULT_INTERVAL_SECS,
                "config {config:?} falls back to the default"
            );
        }
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
                        stack_trace: format!("panic: {title}"),
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
