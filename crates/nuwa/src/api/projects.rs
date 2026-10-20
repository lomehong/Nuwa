//! `GET /api/v1/projects` — the cross-project rollup.
//!
//! The console treats a project (a repository under repair) as a first-class
//! object: one row per project with the numbers an owner triages on — what
//! needs a human, what is queued, what is running, what the runner and PR
//! quota look like, and when the project was last heard from.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::api::common::{
    attention_kind, issue_is_stuck, runner_intervals, DAY_MS, RUNNER_OFFLINE_MS, STATES,
};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{IssueRow, RunnerRow, ShanResult, Store};

#[derive(Debug, Serialize)]
pub struct ProjectDto {
    /// Repository identity — the project's stable key.
    pub repo: String,
    /// Display name from the registry, when the owner registered it.
    pub name: Option<String>,
    pub url: Option<String>,
    pub description: Option<String>,
    /// Registered in the console's repository registry.
    pub registered: bool,
    /// Open issue counts by lifecycle state.
    pub states: BTreeMap<String, i64>,
    /// Needs a human: escalated, rejected, stuck, or a CI timeout.
    pub attention: i64,
    /// Highest severity among the project's open issues — the stone mark.
    pub worst_severity: Option<String>,
    pub queued: i64,
    pub running: i64,
    pub pr_open: i64,
    pub escalated: i64,
    pub ignored: i64,
    pub merged: i64,
    pub rejected: i64,
    pub runners: i64,
    pub runners_online: i64,
    pub prs_today: i64,
    /// Daily PR quota shared by the project's runners.
    pub pr_limit: i64,
    pub ci_green: i64,
    pub ci_red: i64,
    pub reports_24h: i64,
    pub tokens_24h: i64,
    pub last_signal_ms: i64,
    pub last_delivery_ms: Option<i64>,
    pub last_pr_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectsResponse {
    pub data: Vec<ProjectDto>,
}

/// `GET /api/v1/projects` — every project the instance knows about.
pub async fn projects(State(state): State<AppState>) -> Response {
    let now = crate::now_ms();
    let store = std::sync::Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_projects(&store, now)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("projects: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("projects: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read projects",
    )
    .into_response()
}

/// Severity ranking for the stone mark: the most severe open issue wins.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

struct ProjectAccumulator {
    states: BTreeMap<String, i64>,
    attention: i64,
    worst_severity: Option<String>,
    worst_rank: u8,
    last_signal_ms: i64,
    runners: i64,
    runners_online: i64,
    pr_limit: i64,
    ci_green: i64,
    ci_red: i64,
    reports_24h: i64,
    tokens_24h: i64,
    last_delivery_ms: Option<i64>,
    last_pr_url: Option<String>,
}

impl ProjectAccumulator {
    fn new() -> Self {
        Self {
            states: STATES
                .iter()
                .map(|state| ((*state).to_string(), 0))
                .collect(),
            attention: 0,
            worst_severity: None,
            worst_rank: 0,
            last_signal_ms: 0,
            runners: 0,
            runners_online: 0,
            pr_limit: 0,
            ci_green: 0,
            ci_red: 0,
            reports_24h: 0,
            tokens_24h: 0,
            last_delivery_ms: None,
            last_pr_url: None,
        }
    }

    fn count(&mut self, state: &str, amount: i64) {
        *self.states.entry(state.to_string()).or_insert(0) += amount;
    }
}

fn build_projects(store: &Store, now: i64) -> ShanResult<ProjectsResponse> {
    let issues: Vec<IssueRow> = store.list_issues()?;
    let runners: Vec<RunnerRow> = store.list_runners()?;
    let intervals = runner_intervals(&runners);
    let day_start = now - now.rem_euclid(DAY_MS);
    let yesterday = shanhaijing::utc_day(day_start - DAY_MS);
    let signals = store.signal_counts_since_day(&yesterday)?;
    let runs = store.runs_since(now - DAY_MS)?;

    let mut projects: BTreeMap<String, ProjectAccumulator> = BTreeMap::new();
    for issue in &issues {
        let entry = projects.entry(issue.repo.clone()).or_insert_with(ProjectAccumulator::new);
        entry.count(&issue.state, 1);
        entry.last_signal_ms = entry.last_signal_ms.max(issue.last_seen);
        let open = !matches!(issue.state.as_str(), "merged" | "ignored");
        if open {
            let rank = severity_rank(&issue.severity);
            if rank > entry.worst_rank {
                entry.worst_rank = rank;
                entry.worst_severity = Some(issue.severity.clone());
            }
        }
        let stuck = issue_is_stuck(store, issue, &intervals, now)?;
        if attention_kind(store, issue, stuck, now)?.is_some() {
            entry.attention += 1;
        }
    }

    for runner in &runners {
        let entry = projects
            .entry(runner.repo.clone())
            .or_insert_with(ProjectAccumulator::new);
        entry.runners += 1;
        if now.saturating_sub(runner.heartbeat_at) <= RUNNER_OFFLINE_MS {
            entry.runners_online += 1;
        }
        if let Some(limit) = runner.daily_pr_limit {
            entry.pr_limit = entry.pr_limit.max(limit);
        }
    }

    for run in &runs {
        let Some(entry) = projects.get_mut(&run.repo) else {
            continue;
        };
        match run.ci_verdict.as_deref() {
            Some("success") => entry.ci_green += 1,
            Some("failure") => entry.ci_red += 1,
            _ => {}
        }
        entry.tokens_24h += (run.tokens_uncached + run.tokens_cached).max(0);
        if let Some(opened_at) = run.pr_opened_at {
            let newer = match entry.last_delivery_ms {
                Some(last) => opened_at > last,
                None => true,
            };
            if newer {
                entry.last_delivery_ms = Some(opened_at);
                entry.last_pr_url = run.pr_url.clone();
            }
        }
    }

    for (repo, count) in &signals {
        if let Some(entry) = projects.get_mut(repo) {
            entry.reports_24h += *count as i64;
        }
    }

    let registry: BTreeMap<String, (String, String, String)> = store
        .list_repos()?
        .into_iter()
        .map(|repo| {
            (
                repo.repo.clone(),
                (repo.name, repo.url, repo.description),
            )
        })
        .collect();

    let mut data = Vec::with_capacity(projects.len());
    for (repo, entry) in &projects {
        let (name, url, description) = registry
            .get(repo)
            .cloned()
            .unwrap_or_else(|| (repo.clone(), String::new(), String::new()));
        let prs_today = store.prs_opened_since(repo, day_start)?;
        data.push(ProjectDto {
            repo: repo.clone(),
            name: (!name.is_empty()).then_some(name),
            url: (!url.is_empty()).then_some(url),
            description: (!description.is_empty()).then_some(description),
            registered: registry.contains_key(repo),
            states: entry.states.clone(),
            attention: entry.attention,
            worst_severity: entry.worst_severity.clone(),
            queued: *entry.states.get("new").unwrap_or(&0),
            running: entry.states.get("triggered").copied().unwrap_or(0)
                + entry.states.get("fixing").copied().unwrap_or(0),
            pr_open: *entry.states.get("pr_open").unwrap_or(&0),
            escalated: *entry.states.get("escalated").unwrap_or(&0),
            ignored: *entry.states.get("ignored").unwrap_or(&0),
            merged: *entry.states.get("merged").unwrap_or(&0),
            rejected: *entry.states.get("rejected").unwrap_or(&0),
            runners: entry.runners,
            runners_online: entry.runners_online,
            prs_today: prs_today as i64,
            pr_limit: entry.pr_limit,
            ci_green: entry.ci_green,
            ci_red: entry.ci_red,
            reports_24h: entry.reports_24h,
            tokens_24h: entry.tokens_24h,
            last_signal_ms: entry.last_signal_ms,
            last_delivery_ms: entry.last_delivery_ms,
            last_pr_url: entry.last_pr_url.clone(),
        });
    }

    // Triage order: what needs a human first, then what is in flight, then
    // the most recently heard-from project.
    data.sort_by(|a, b| {
        b.attention
            .cmp(&a.attention)
            .then(b.running.cmp(&a.running))
            .then(b.queued.cmp(&a.queued))
            .then(b.last_signal_ms.cmp(&a.last_signal_ms))
            .then(a.repo.cmp(&b.repo))
    });

    Ok(ProjectsResponse { data })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shanhaijing::{Signal, State};

    fn signal(repo: &str, title: &str, severity: &str) -> Signal {
        Signal {
            repo: repo.into(),
            language: "go".into(),
            title: title.into(),
            severity: severity.into(),
            message: None,
            stack_trace: format!("panic: {title}\ngoroutine 1 [running]:\nexample.com/app.F()\n\t/app/a.go:1 +0x1"),
        }
    }

    fn fp_of(store: &Store, title: &str) -> String {
        store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|issue| issue.title == title)
            .expect("issue")
            .fingerprint
    }

    #[test]
    fn projects_roll_up_per_repo_numbers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 1_000_000_000;

        store.report_signal(&signal("app/alpha", "alpha one", "critical"), now).expect("a");
        store.report_signal(&signal("app/alpha", "alpha two", "low"), now).expect("b");
        store.report_signal(&signal("app/beta", "beta one", "high"), now).expect("c");
        let alpha_one = fp_of(&store, "alpha one");
        store
            .set_state(&alpha_one, State::Triggered, now, None)
            .expect("triggered");
        store
            .set_state(&alpha_one, State::Escalated, now, None)
            .expect("escalated");

        store
            .upsert_runner(&shanhaijing::RunnerUpsert {
                id: "runner-alpha".into(),
                repo: "app/alpha".into(),
                host: "host".into(),
                pid: None,
                started_at: now - 1_000,
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
        store
            .insert_repo("app/alpha", "Alpha", "https://example.com/alpha", "核心服务", now)
            .expect("register");

        let response = build_projects(&store, now).expect("projects");
        let alpha = response
            .data
            .iter()
            .find(|project| project.repo == "app/alpha")
            .expect("alpha");
        assert!(alpha.registered);
        assert_eq!(alpha.name.as_deref(), Some("Alpha"));
        assert_eq!(alpha.escalated, 1);
        assert_eq!(alpha.attention, 1, "an escalated issue needs a human");
        assert_eq!(alpha.queued, 1);
        assert_eq!(alpha.runners, 1);
        assert_eq!(alpha.runners_online, 1);
        assert_eq!(alpha.pr_limit, 5);
        assert_eq!(alpha.worst_severity.as_deref(), Some("critical"));

        let beta = response
            .data
            .iter()
            .find(|project| project.repo == "app/beta")
            .expect("beta");
        assert!(!beta.registered);
        assert_eq!(beta.attention, 0);
        assert_eq!(beta.queued, 1);
        assert_eq!(beta.runners, 0);
    }

    #[test]
    fn projects_are_ordered_by_attention_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 1_000_000_000;
        store.report_signal(&signal("app/quiet", "quiet one", "low"), now).expect("q");
        store
            .report_signal(&signal("app/loud", "loud one", "high"), now)
            .expect("l");
        let loud = fp_of(&store, "loud one");
        store
            .set_state(&loud, State::Triggered, now, None)
            .expect("triggered");
        store
            .set_state(&loud, State::Escalated, now, None)
            .expect("escalated");

        let response = build_projects(&store, now).expect("projects");
        assert_eq!(response.data[0].repo, "app/loud");
    }
}
