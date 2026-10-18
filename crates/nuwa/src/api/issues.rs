//! `GET /api/v1/issues` and `GET /api/v1/issues/{fp}` — the console issue
//! list and detail read endpoints (docs/09 §9.2).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::api::common::{
    attention_kind, issue_is_stuck, non_empty, parse_flag, parse_page_number, runner_intervals,
    PageMeta, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, STATES,
};
use crate::api::ApiError;
use crate::serve::AppState;
use shanhaijing::{EventRow, IssueRow, RunRow, ShanResult, Store};

/// Detail timeline depth (docs/09 §9.2).
const TIMELINE_LIMIT: u32 = 200;
/// Detail run history depth (docs/09 §9.2).
const RUNS_LIMIT: u32 = 10;
/// Detail "similar problems" cap (docs/09 §9.2).
const SIMILAR_LIMIT: u32 = 5;

/// `q` stops at 128 characters so a hostile query cannot scan huge blobs.
const MAX_Q_CHARS: usize = 128;

const SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];

/// Raw list query. Every value stays a string so invalid input maps to the
/// `validation_failed` contract instead of axum's plain-text rejection.
#[derive(Debug, Default, Deserialize)]
pub struct IssueListQuery {
    state: Option<String>,
    severity: Option<String>,
    repo: Option<String>,
    language: Option<String>,
    q: Option<String>,
    stuck: Option<String>,
    attention: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    page: Option<String>,
    page_size: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    LastSeen,
    FirstSeen,
    Occurrences,
    Severity,
    State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Asc,
    Desc,
}

struct ListParams {
    states: BTreeSet<String>,
    severity: Option<String>,
    repo: Option<String>,
    language: Option<String>,
    /// Lowercased once per request; `q` matches title/fingerprint contains.
    q_lower: Option<String>,
    stuck: bool,
    attention: bool,
    sort: Sort,
    order: Order,
    page: u64,
    page_size: u64,
}

impl ListParams {
    fn parse(query: &IssueListQuery) -> Result<Self, ApiError> {
        let mut fields = BTreeMap::new();
        let states = parse_states(query.state.as_deref(), &mut fields);
        let severity = parse_choice(
            "severity",
            query.severity.as_deref(),
            &SEVERITIES,
            &mut fields,
        );
        let repo = non_empty(query.repo.as_deref());
        let language = non_empty(query.language.as_deref());
        let q_lower = match query.q.as_deref().filter(|q| !q.is_empty()) {
            Some(q) if q.chars().count() > MAX_Q_CHARS => {
                fields.insert(
                    "q".to_string(),
                    format!("must be at most {MAX_Q_CHARS} characters"),
                );
                None
            }
            Some(q) => Some(q.to_lowercase()),
            None => None,
        };
        let stuck = parse_flag("stuck", query.stuck.as_deref(), &mut fields);
        let attention = parse_flag("attention", query.attention.as_deref(), &mut fields);
        let sort = match non_empty(query.sort.as_deref()).as_deref() {
            None => Sort::LastSeen,
            Some("last_seen") => Sort::LastSeen,
            Some("first_seen") => Sort::FirstSeen,
            Some("occurrences") => Sort::Occurrences,
            Some("severity") => Sort::Severity,
            Some("state") => Sort::State,
            Some(other) => {
                fields.insert("sort".to_string(), format!("unknown sort: {other}"));
                Sort::LastSeen
            }
        };
        let order = match non_empty(query.order.as_deref()).as_deref() {
            None => Order::Desc,
            Some("asc") => Order::Asc,
            Some("desc") => Order::Desc,
            Some(other) => {
                fields.insert("order".to_string(), format!("unknown order: {other}"));
                Order::Desc
            }
        };
        let page = parse_page_number("page", query.page.as_deref(), 1, None, &mut fields);
        let page_size = parse_page_number(
            "page_size",
            query.page_size.as_deref(),
            DEFAULT_PAGE_SIZE,
            Some(MAX_PAGE_SIZE),
            &mut fields,
        );
        if !fields.is_empty() {
            return Err(ApiError::validation(fields));
        }
        Ok(Self {
            states,
            severity,
            repo,
            language,
            q_lower,
            stuck,
            attention,
            sort,
            order,
            page,
            page_size,
        })
    }

    /// Scalar filters applied before the per-issue stuck/attention lookups.
    fn matches(&self, issue: &IssueRow) -> bool {
        if !self.states.is_empty() && !self.states.contains(&issue.state) {
            return false;
        }
        if self
            .severity
            .as_deref()
            .is_some_and(|severity| issue.severity != severity)
        {
            return false;
        }
        if self.repo.as_deref().is_some_and(|repo| issue.repo != repo) {
            return false;
        }
        if self
            .language
            .as_deref()
            .is_some_and(|language| issue.language != language)
        {
            return false;
        }
        if let Some(q) = &self.q_lower {
            if !issue.title.to_lowercase().contains(q)
                && !issue.fingerprint.to_lowercase().contains(q)
            {
                return false;
            }
        }
        true
    }
}

fn parse_states(raw: Option<&str>, fields: &mut BTreeMap<String, String>) -> BTreeSet<String> {
    let Some(raw) = raw else {
        return BTreeSet::new();
    };
    let mut states = BTreeSet::new();
    let mut unknown = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if STATES.contains(&part) {
            states.insert(part.to_string());
        } else {
            unknown.push(part);
        }
    }
    if !unknown.is_empty() {
        fields.insert(
            "state".to_string(),
            format!("unknown state: {}", unknown.join(",")),
        );
    }
    states
}

fn parse_choice(
    field: &str,
    raw: Option<&str>,
    allowed: &[&str],
    fields: &mut BTreeMap<String, String>,
) -> Option<String> {
    let value = non_empty(raw)?;
    if allowed.contains(&value.as_str()) {
        Some(value)
    } else {
        fields.insert(field.to_string(), format!("unknown {field}: {value}"));
        None
    }
}

#[derive(Debug, Serialize)]
pub struct IssueListResponse {
    pub data: Vec<IssueListItem>,
    pub meta: PageMeta,
}

/// The list/detail issue shape: `IssueRow` (already ms-suffixed) plus the
/// per-owner `stuck` flag.
#[derive(Debug, Serialize)]
pub struct IssueListItem {
    #[serde(flatten)]
    pub issue: IssueRow,
    pub stuck: bool,
}

#[derive(Debug, Serialize)]
pub struct IssueDetailResponse {
    pub issue: IssueListItem,
    pub stack_sample: String,
    pub timeline: Vec<TimelineItem>,
    pub runs: Vec<RunDto>,
    pub similar: SimilarIssues,
}

#[derive(Debug, Serialize)]
pub struct TimelineItem {
    pub kind: String,
    pub detail: String,
    #[serde(rename = "ts_ms")]
    pub ts: i64,
    pub actor: Option<String>,
    pub repo: String,
}

impl From<EventRow> for TimelineItem {
    fn from(event: EventRow) -> Self {
        Self {
            kind: event.kind,
            detail: event.detail,
            ts: event.ts,
            actor: event.actor,
            repo: event.repo,
        }
    }
}

/// Run projection for the detail page. `session_dir` stays server-side and
/// the issue fingerprint/runner id are already implied by the response
/// (docs/09 §9.2).
#[derive(Debug, Serialize)]
pub struct RunDto {
    pub id: String,
    #[serde(rename = "started_at_ms")]
    pub started_at: i64,
    #[serde(rename = "finished_at_ms")]
    pub finished_at: Option<i64>,
    pub phase: String,
    pub outcome: Option<String>,
    pub ci_verdict: Option<String>,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub root_cause: Option<String>,
    pub change_stat: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    #[serde(rename = "pr_opened_at_ms")]
    pub pr_opened_at: Option<i64>,
    pub verification_ok: Option<bool>,
    pub verification_detail: Option<String>,
    pub error: Option<String>,
}

impl From<RunRow> for RunDto {
    fn from(run: RunRow) -> Self {
        Self {
            id: run.id,
            started_at: run.started_at,
            finished_at: run.finished_at,
            phase: run.phase,
            outcome: run.outcome,
            ci_verdict: run.ci_verdict,
            turns: run.turns,
            tokens_uncached: run.tokens_uncached,
            tokens_cached: run.tokens_cached,
            provider: run.provider,
            model: run.model,
            root_cause: run.root_cause,
            change_stat: run.change_stat,
            branch: run.branch,
            commit_sha: run.commit_sha,
            pr_url: run.pr_url,
            pr_opened_at: run.pr_opened_at,
            verification_ok: run.verification_ok,
            verification_detail: run.verification_detail,
            error: run.error,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SimilarIssues {
    pub count: usize,
    pub items: Vec<SimilarItem>,
}

#[derive(Debug, Serialize)]
pub struct SimilarItem {
    pub fingerprint: String,
    pub state: String,
    #[serde(rename = "last_seen_ms")]
    pub last_seen: i64,
}

impl From<IssueRow> for SimilarItem {
    fn from(issue: IssueRow) -> Self {
        Self {
            fingerprint: issue.fingerprint,
            state: issue.state,
            last_seen: issue.last_seen,
        }
    }
}

/// `GET /api/v1/issues` — filtered, sorted, paginated issue list.
pub async fn list(State(state): State<AppState>, Query(query): Query<IssueListQuery>) -> Response {
    let params = match ListParams::parse(&query) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_list(&store, &params, now)).await {
        Ok(Ok(body)) => Json(body).into_response(),
        Ok(Err(error)) => {
            eprintln!("issues: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("issues: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn build_list(store: &Store, params: &ListParams, now: i64) -> ShanResult<IssueListResponse> {
    // Stuck/attention are computed in Rust over the issue and runner rows
    // (R25): fine for the single-owner scale, revisit if the store grows.
    let runners = store.list_runners()?;
    let intervals = runner_intervals(&runners);
    let mut items = Vec::new();
    for issue in store.list_issues()? {
        if !params.matches(&issue) {
            continue;
        }
        let stuck = issue_is_stuck(store, &issue, &intervals, now)?;
        if params.stuck && !stuck {
            continue;
        }
        if params.attention && attention_kind(store, &issue, stuck, now)?.is_none() {
            continue;
        }
        items.push(IssueListItem { issue, stuck });
    }
    items.sort_by(|left, right| compare(left, right, params.sort, params.order));

    let total = items.len();
    // A huge page number must be an empty page, never an overflow/panic.
    let start = (params.page - 1)
        .checked_mul(params.page_size)
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(usize::MAX);
    let data = if start >= total {
        Vec::new()
    } else {
        items
            .into_iter()
            .skip(start)
            .take(params.page_size as usize)
            .collect()
    };
    Ok(IssueListResponse {
        data,
        meta: PageMeta {
            page: params.page,
            page_size: params.page_size,
            total,
        },
    })
}

fn compare(left: &IssueListItem, right: &IssueListItem, sort: Sort, order: Order) -> Ordering {
    let primary = match sort {
        Sort::LastSeen => left.issue.last_seen.cmp(&right.issue.last_seen),
        Sort::FirstSeen => left.issue.first_seen.cmp(&right.issue.first_seen),
        Sort::Occurrences => left.issue.occurrences.cmp(&right.issue.occurrences),
        Sort::Severity => {
            severity_weight(&left.issue.severity).cmp(&severity_weight(&right.issue.severity))
        }
        Sort::State => state_rank(&left.issue.state).cmp(&state_rank(&right.issue.state)),
    };
    let primary = if order == Order::Desc {
        primary.reverse()
    } else {
        primary
    };
    // Fingerprint is the stable tie-breaker so pages cannot shuffle.
    primary.then_with(|| left.issue.fingerprint.cmp(&right.issue.fingerprint))
}

/// Same weights as the runner claim priority (Critical > High > Medium > Low).
fn severity_weight(severity: &str) -> u8 {
    match severity {
        "critical" => 10,
        "high" => 5,
        "medium" => 2,
        _ => 1,
    }
}

/// Lifecycle position used by `sort=state`.
fn state_rank(state: &str) -> usize {
    STATES
        .iter()
        .position(|known| *known == state)
        .unwrap_or(STATES.len())
}

/// `GET /api/v1/issues/{fp}` — issue detail with stack sample, timeline,
/// recent runs and same-title neighbours.
pub async fn detail(State(state): State<AppState>, Path(fingerprint): Path<String>) -> Response {
    let now = crate::now_ms();
    let store = Arc::clone(&state.store);
    match tokio::task::spawn_blocking(move || build_detail(&store, &fingerprint, now)).await {
        Ok(Ok(Some(body))) => Json(body).into_response(),
        Ok(Ok(None)) => ApiError::not_found("issue not found").into_response(),
        Ok(Err(error)) => {
            eprintln!("issue detail: store read failed: {error}");
            internal_error()
        }
        Err(error) => {
            eprintln!("issue detail: blocking task failed: {error}");
            internal_error()
        }
    }
}

fn build_detail(
    store: &Store,
    fingerprint: &str,
    now: i64,
) -> ShanResult<Option<IssueDetailResponse>> {
    let Some(snapshot) = store.get(fingerprint)? else {
        return Ok(None);
    };
    // Store has no single-row full projection yet; detail is rare and the
    // single-owner issue table is small (R25).
    let Some(issue) = store
        .list_issues()?
        .into_iter()
        .find(|issue| issue.fingerprint == fingerprint)
    else {
        return Ok(None);
    };
    let runners = store.list_runners()?;
    let intervals = runner_intervals(&runners);
    let stuck = issue_is_stuck(store, &issue, &intervals, now)?;
    let timeline = store
        .events_for(fingerprint, TIMELINE_LIMIT)?
        .into_iter()
        .map(TimelineItem::from)
        .collect();
    let runs = store
        .runs_for(fingerprint, RUNS_LIMIT)?
        .into_iter()
        .map(RunDto::from)
        .collect();
    let similar_count = store.similar_count(&issue.title, fingerprint)?;
    let similar = store.similar_issues(&issue.title, fingerprint, SIMILAR_LIMIT)?;
    Ok(Some(IssueDetailResponse {
        stack_sample: snapshot.stack_sample,
        issue: IssueListItem { issue, stuck },
        timeline,
        runs,
        similar: SimilarIssues {
            count: usize::try_from(similar_count).unwrap_or(usize::MAX),
            items: similar.into_iter().map(SimilarItem::from).collect(),
        },
    }))
}

fn internal_error() -> Response {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "failed to read issues",
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::response::Response;
    use axum::Router;
    use tower::ServiceExt;

    use crate::serve::{build_app, AppState};

    const APP: &str = "example.com/app";
    const OTHER: &str = "example.com/other";
    const SORT: &str = "example.com/sort";
    const SIMILAR: &str = "example.com/similar";
    const HEAVY: &str = "example.com/heavy";
    const STRICT_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

    // ---- app / response helpers -------------------------------------------

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
        let state = AppState {
            store,
            ui_token: Arc::new(None),
            ingest_token: Arc::new(Some("ingest-secret".into())),
            session: Arc::new(crate::session::SessionManager::new("", 86_400_000)),
            limiter: Arc::new(crate::session::LoginLimiter::new()),
            started_at_ms: crate::now_ms(),
            cookie_secure: false,
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

    fn fingerprints(value: &serde_json::Value) -> Vec<String> {
        value["data"]
            .as_array()
            .unwrap_or_else(|| panic!("data array missing: {value}"))
            .iter()
            .map(|item| {
                item["fingerprint"]
                    .as_str()
                    .unwrap_or_else(|| panic!("fingerprint missing: {item}"))
                    .to_string()
            })
            .collect()
    }

    fn fingerprint_set(value: &serde_json::Value) -> BTreeSet<String> {
        fingerprints(value).into_iter().collect()
    }

    // ---- seed helpers ------------------------------------------------------

    struct Seed {
        now: i64,
        esc: String,
        rejected: String,
        stuck_default: String,
        slow: String,
        pr_recent: String,
        pr_stale: String,
        healthy_fixing: String,
        detail: String,
        detail_pr_url: String,
        detail_stack: String,
        other: String,
        sort_a: String,
        sort_b: String,
        sort_c: String,
        sim_new: String,
        sim_merged: String,
        sim_extras: Vec<String>,
        heavy: String,
    }

    fn signal(
        title: &str,
        repo: &str,
        language: &str,
        severity: &str,
        variant: &str,
    ) -> shanhaijing::Signal {
        shanhaijing::Signal {
            repo: repo.to_string(),
            language: language.to_string(),
            title: title.to_string(),
            severity: severity.to_string(),
            message: Some(format!("boom {title} {variant}")),
            stack_trace: format!(
                "panic: {title} [{variant}]\ngoroutine 1 [running]:\nexample.com/{language}.Fn_{variant}()\n\t/app/{language}.go:1 +0x1"
            ),
        }
    }

    fn report(store: &shanhaijing::Store, signal: &shanhaijing::Signal, at: i64) -> String {
        match store.report_signal(signal, at).expect("report signal") {
            shanhaijing::ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("{} must be a new issue, got {other:?}", signal.title),
        }
    }

    fn set_state(
        store: &shanhaijing::Store,
        fingerprint: &str,
        state: shanhaijing::State,
        at: i64,
        detail: Option<&str>,
    ) {
        store
            .set_state(fingerprint, state, at, detail)
            .expect("set state");
    }

    fn claim_and_own(
        store: &shanhaijing::Store,
        fingerprint: &str,
        runner_id: &str,
        run_id: &str,
        at: i64,
    ) {
        set_state(store, fingerprint, shanhaijing::State::Triggered, at, None);
        set_state(store, fingerprint, shanhaijing::State::Fixing, at + 1, None);
        store
            .start_run(&shanhaijing::RunStart {
                id: run_id.to_string(),
                fingerprint: fingerprint.to_string(),
                runner_id: runner_id.to_string(),
                repo: APP.to_string(),
                started_at: at,
                provider: None,
                model: None,
            })
            .expect("start run");
    }

    fn upsert_runner(
        store: &shanhaijing::Store,
        id: &str,
        repo: &str,
        heartbeat_at: i64,
        config_json: &str,
    ) {
        store
            .upsert_runner(&shanhaijing::RunnerUpsert {
                id: id.to_string(),
                repo: repo.to_string(),
                host: "test-host".to_string(),
                pid: None,
                started_at: heartbeat_at,
                heartbeat_at,
                status: "working".to_string(),
                provider: Some("faux".to_string()),
                model: Some("faux".to_string()),
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: config_json.to_string(),
            })
            .expect("upsert runner");
    }

    /// Seeds attention candidates, a stuck and a healthy slow-runner issue, a
    /// sortable trio, same-title similar rows, 11 runs and a 205-event
    /// timeline. `now` is captured once so the relative timestamps line up
    /// with the handler's clock.
    fn seed(store: &shanhaijing::Store) -> Seed {
        use shanhaijing::State;

        let now = crate::now_ms();

        // Escalated with a reason prefix on the newest event.
        let esc = report(
            store,
            &signal("attention escalated", APP, "go", "critical", "esc"),
            now - 20_000,
        );
        claim_and_own(store, &esc, "runner-gone", "run-esc", now - 20_000);
        set_state(
            store,
            &esc,
            State::Escalated,
            now - 18_000,
            Some("repair_failed: boom"),
        );

        // Rejected without a reason prefix (sync closed the PR).
        let rejected = report(
            store,
            &signal("attention rejected", APP, "go", "high", "rej"),
            now - 40_000,
        );
        set_state(store, &rejected, State::Triggered, now - 39_000, None);
        set_state(store, &rejected, State::Fixing, now - 38_000, None);
        set_state(store, &rejected, State::PrOpen, now - 37_000, None);
        set_state(
            store,
            &rejected,
            State::Rejected,
            now - 36_000,
            Some("PR closed without merge"),
        );

        // pr_open with a `ci_timeout` inside and outside the 24h window.
        let pr_recent = report(
            store,
            &signal("attention pr timeout", APP, "go", "medium", "prr"),
            now - 30_000,
        );
        set_state(store, &pr_recent, State::Triggered, now - 29_000, None);
        set_state(store, &pr_recent, State::Fixing, now - 28_000, None);
        set_state(store, &pr_recent, State::PrOpen, now - 27_000, None);
        store
            .record_event(
                &pr_recent,
                now - 1_000,
                Some(APP),
                "runner",
                "ci_timeout",
                "head-recent",
            )
            .expect("recent ci_timeout");

        let pr_stale = report(
            store,
            &signal("attention pr stale", APP, "go", "medium", "prs"),
            now - 50_000,
        );
        set_state(store, &pr_stale, State::Triggered, now - 49_000, None);
        set_state(store, &pr_stale, State::Fixing, now - 48_000, None);
        set_state(store, &pr_stale, State::PrOpen, now - 47_000, None);
        store
            .record_event(
                &pr_stale,
                now - 90_000_000,
                Some(APP),
                "runner",
                "ci_timeout",
                "head-stale",
            )
            .expect("stale ci_timeout");

        // A healthy fixing issue: no owner, recent state change → not stuck.
        let healthy_fixing = report(
            store,
            &signal("fixing healthy", APP, "go", "medium", "healthy"),
            now - 10_000,
        );
        set_state(store, &healthy_fixing, State::Triggered, now - 9_000, None);
        set_state(store, &healthy_fixing, State::Fixing, now - 8_000, None);

        // Same 500s of silence, two runner intervals: the default-config
        // runner is stuck, the 600s runner (1800s window) is not.
        let stuck_default = report(
            store,
            &signal("stuck default", APP, "go", "high", "stuck"),
            now - 600_000,
        );
        claim_and_own(
            store,
            &stuck_default,
            "runner-default",
            "run-stuck",
            now - 500_000,
        );
        upsert_runner(store, "runner-default", APP, now - 500_000, "{}");

        let slow = report(
            store,
            &signal("slow runner healthy", APP, "rust", "high", "slow"),
            now - 600_000,
        );
        claim_and_own(store, &slow, "runner-slow", "run-slow", now - 500_000);
        upsert_runner(
            store,
            "runner-slow",
            APP,
            now - 500_000,
            r#"{"interval_secs":600}"#,
        );

        // Detail subject: merged PR, 11 runs, same-title neighbours.
        let detail_signal = signal("detail crash", APP, "go", "high", "detail");
        let detail_stack = detail_signal.stack_trace.clone();
        let detail = report(store, &detail_signal, now - 200_000);
        set_state(store, &detail, State::Triggered, now - 190_000, None);
        set_state(store, &detail, State::Fixing, now - 180_000, None);
        set_state(store, &detail, State::PrOpen, now - 170_000, None);
        let detail_pr_url = "https://github.com/example/app/pull/9".to_string();
        store
            .set_pr(&detail, &detail_pr_url, now - 165_000)
            .expect("set pr");
        set_state(
            store,
            &detail,
            State::Merged,
            now - 150_000,
            Some(&detail_pr_url),
        );
        for index in 0..11 {
            store
                .start_run(&shanhaijing::RunStart {
                    id: format!("run-{index}"),
                    fingerprint: detail.clone(),
                    runner_id: "runner-detail".to_string(),
                    repo: APP.to_string(),
                    started_at: now - 100_000 + index * 1_000,
                    provider: Some("faux".to_string()),
                    model: Some("faux".to_string()),
                })
                .expect("start detail run");
        }
        store
            .update_run(
                "run-10",
                &shanhaijing::RunUpdate {
                    turns: Some(6),
                    tokens_uncached: Some(17_000),
                    tokens_cached: Some(500),
                    harness_run_id: Some("harness-1".to_string()),
                    root_cause: Some("nil dereference".to_string()),
                    verification_ok: Some(true),
                    verification_detail: Some("verification passed".to_string()),
                    change_stat: Some("1 file changed".to_string()),
                    branch: Some("agent/fix/abc".to_string()),
                    commit_sha: Some("deadbeef".to_string()),
                    pr_url: Some(detail_pr_url.clone()),
                    pr_opened_at: Some(now - 2_000),
                    ci_verdict: Some("success".to_string()),
                    session_dir: Some("sessions/run-10".to_string()),
                    ..Default::default()
                },
            )
            .expect("update detail run");
        store
            .finish_run(
                "run-10",
                &shanhaijing::RunFinish {
                    finished_at: now - 1_000,
                    outcome: "success".to_string(),
                    ci_verdict: Some("success".to_string()),
                    error: None,
                },
            )
            .expect("finish detail run");

        let sim_new = report(
            store,
            &signal("detail crash", SIMILAR, "go", "medium", "sim1"),
            now - 400_000,
        );
        let sim_merged = report(
            store,
            &signal("detail crash", SIMILAR, "go", "low", "sim2"),
            now - 450_000,
        );
        set_state(store, &sim_merged, State::Triggered, now - 445_000, None);
        set_state(store, &sim_merged, State::Fixing, now - 444_000, None);
        set_state(store, &sim_merged, State::PrOpen, now - 443_000, None);
        set_state(store, &sim_merged, State::Merged, now - 440_000, None);

        // Four more same-title neighbours: the strip caps at five items while
        // the count reports the whole population.
        let sim_extras: Vec<String> = [460_000, 470_000, 480_000, 490_000]
            .iter()
            .enumerate()
            .map(|(index, offset)| {
                report(
                    store,
                    &signal("detail crash", SIMILAR, "go", "low", &format!("simx{index}")),
                    now - offset,
                )
            })
            .collect();

        // `q=sort` isolates this trio for the sort/pagination matrix.
        let sort_a = report(
            store,
            &signal("Sort A", SORT, "go", "low", "sa"),
            now - 32_400_000,
        );
        store
            .report_signal(&signal("Sort A", SORT, "go", "low", "sa"), now - 3_600_000)
            .expect("second Sort A sighting");
        let sort_b = report(
            store,
            &signal("Sort B", SORT, "go", "critical", "sb"),
            now - 28_800_000,
        );
        set_state(store, &sort_b, State::Triggered, now - 1_000, None);
        let sort_c = report(
            store,
            &signal("Sort C", SORT, "go", "high", "sc"),
            now - 25_200_000,
        );
        store
            .report_signal(&signal("Sort C", SORT, "go", "high", "sc"), now - 7_201_000)
            .expect("second Sort C sighting");
        store
            .report_signal(&signal("Sort C", SORT, "go", "high", "sc"), now - 7_200_000)
            .expect("third Sort C sighting");
        set_state(store, &sort_c, State::Triggered, now - 1_500, None);
        set_state(store, &sort_c, State::Fixing, now - 1_000, None);

        let other = report(
            store,
            &signal("other alpha", OTHER, "python", "medium", "other"),
            now - 70_000,
        );

        // 205 extra events: `events_for(…, 200)` keeps the newest 200.
        let heavy = report(
            store,
            &signal("timeline heavy", HEAVY, "go", "low", "heavy"),
            now - 5_000,
        );
        for index in 0..205 {
            store
                .record_event(
                    &heavy,
                    now - 4_000 + index,
                    Some(HEAVY),
                    "system",
                    "note",
                    &format!("n{index}"),
                )
                .expect("record heavy event");
        }

        Seed {
            now,
            esc,
            rejected,
            stuck_default,
            slow,
            pr_recent,
            pr_stale,
            healthy_fixing,
            detail,
            detail_pr_url,
            detail_stack,
            other,
            sort_a,
            sort_b,
            sort_c,
            sim_new,
            sim_merged,
            sim_extras,
            heavy,
        }
    }

    fn seeded_app() -> (tempfile::TempDir, Router, Seed) {
        let (dir, state) = test_state();
        let seed = seed(&state.store);
        let app = build_app(state);
        (dir, app, seed)
    }

    // ---- list --------------------------------------------------------------

    #[tokio::test]
    async fn list_returns_rows_with_stuck_and_pagination_meta() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues?repo={SORT}")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        assert_eq!(
            value["meta"],
            serde_json::json!({ "page": 1, "page_size": 50, "total": 3 })
        );
        // Default order is last_seen desc.
        assert_eq!(
            fingerprints(&value),
            vec![
                seed.sort_a.as_str(),
                seed.sort_c.as_str(),
                seed.sort_b.as_str()
            ]
        );

        let first = &value["data"][0];
        assert_eq!(first["title"].as_str(), Some("Sort A"));
        assert_eq!(first["repo"].as_str(), Some(SORT));
        assert_eq!(first["language"].as_str(), Some("go"));
        assert_eq!(first["severity"].as_str(), Some("low"));
        assert_eq!(first["state"].as_str(), Some("new"));
        assert_eq!(first["occurrences"].as_u64(), Some(2));
        assert_eq!(first["stuck"].as_bool(), Some(false));
        assert_eq!(first["pr_url"], serde_json::Value::Null);
        assert_eq!(first["ci_verdict"], serde_json::Value::Null);
        assert_eq!(first["owner_runner_id"], serde_json::Value::Null);
        assert_eq!(first["first_seen_ms"].as_i64(), Some(seed.now - 32_400_000));
        assert_eq!(first["last_seen_ms"].as_i64(), Some(seed.now - 3_600_000));
        // A fresh `new` issue has not crossed the state machine yet; the
        // column is still 0, so the read view projects first_seen instead of
        // exposing the 1970 epoch.
        assert_eq!(
            first["state_changed_at_ms"].as_i64(),
            Some(seed.now - 32_400_000)
        );
        assert!(
            first.get("first_seen").is_none(),
            "snake_case ms columns must not leak: {first}"
        );
    }

    #[tokio::test]
    async fn fresh_issue_state_changed_at_falls_back_to_first_seen() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues?repo={OTHER}")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        let row = &value["data"][0];
        assert_eq!(row["fingerprint"].as_str(), Some(seed.other.as_str()));
        assert_eq!(row["state"].as_str(), Some("new"));
        assert_eq!(row["first_seen_ms"].as_i64(), Some(seed.now - 70_000));
        assert_eq!(
            row["state_changed_at_ms"].as_i64(),
            Some(seed.now - 70_000),
            "a fresh issue must not render a 1970 state change"
        );

        // The detail endpoint shares the same projection.
        let response = get(&app, &format!("/api/v1/issues/{}", seed.other)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            value["issue"]["state_changed_at_ms"].as_i64(),
            Some(seed.now - 70_000)
        );
    }

    #[tokio::test]
    async fn list_filters_state_severity_repo_language_and_q() {
        let (_dir, app, seed) = seeded_app();

        // state accepts a comma list.
        let response = get(
            &app,
            &format!("/api/v1/issues?repo={APP}&state=escalated,rejected"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            fingerprint_set(&value),
            BTreeSet::from([seed.esc.clone(), seed.rejected.clone()])
        );
        assert_eq!(value["meta"]["total"].as_u64(), Some(2));

        // severity is exact.
        let response = get(
            &app,
            &format!("/api/v1/issues?repo={APP}&severity=critical"),
        )
        .await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.esc.as_str()]);

        // repo is exact.
        let response = get(&app, &format!("/api/v1/issues?repo={OTHER}")).await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.other.as_str()]);
        assert_eq!(value["data"][0]["language"].as_str(), Some("python"));

        // language is exact.
        let response = get(&app, &format!("/api/v1/issues?repo={APP}&language=rust")).await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.slow.as_str()]);

        // q matches the title case-insensitively.
        let response = get(&app, "/api/v1/issues?q=STUCK").await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.stuck_default.as_str()]);

        // q also matches the fingerprint, case-insensitively.
        let prefix = seed.esc[..10].to_ascii_uppercase();
        let response = get(&app, &format!("/api/v1/issues?q={prefix}")).await;
        let value = json_body(response).await;
        assert!(
            fingerprints(&value).contains(&seed.esc),
            "uppercase fingerprint prefix must match: {value}"
        );

        // Filters combine with AND.
        let response = get(
            &app,
            &format!("/api/v1/issues?repo={APP}&state=fixing&severity=high"),
        )
        .await;
        let value = json_body(response).await;
        assert_eq!(
            fingerprint_set(&value),
            BTreeSet::from([seed.stuck_default.clone(), seed.slow.clone()])
        );
    }

    async fn assert_sort(app: &Router, sort: &str, order: &str, expected: Vec<&str>) {
        let uri = format!("/api/v1/issues?repo={SORT}&sort={sort}&order={order}");
        let response = get(app, &uri).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), expected, "{uri}");
    }

    #[tokio::test]
    async fn list_sorts_by_every_key_and_order() {
        let (_dir, app, seed) = seeded_app();
        let a = seed.sort_a.as_str();
        let b = seed.sort_b.as_str();
        let c = seed.sort_c.as_str();

        assert_sort(&app, "last_seen", "desc", vec![a, c, b]).await;
        assert_sort(&app, "last_seen", "asc", vec![b, c, a]).await;
        assert_sort(&app, "first_seen", "desc", vec![c, b, a]).await;
        assert_sort(&app, "first_seen", "asc", vec![a, b, c]).await;
        assert_sort(&app, "occurrences", "desc", vec![c, a, b]).await;
        assert_sort(&app, "occurrences", "asc", vec![b, a, c]).await;
        assert_sort(&app, "severity", "desc", vec![b, c, a]).await;
        assert_sort(&app, "severity", "asc", vec![a, c, b]).await;
        assert_sort(&app, "state", "asc", vec![a, b, c]).await;
        assert_sort(&app, "state", "desc", vec![c, b, a]).await;
    }

    #[tokio::test]
    async fn list_paginates_and_bounds_page_size() {
        let (_dir, app, seed) = seeded_app();

        let response = get(
            &app,
            &format!("/api/v1/issues?repo={SORT}&sort=last_seen&order=desc&page=1&page_size=2"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            fingerprints(&value),
            vec![seed.sort_a.as_str(), seed.sort_c.as_str()]
        );
        assert_eq!(
            value["meta"],
            serde_json::json!({ "page": 1, "page_size": 2, "total": 3 })
        );

        let response = get(
            &app,
            &format!("/api/v1/issues?repo={SORT}&sort=last_seen&order=desc&page=2&page_size=2"),
        )
        .await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.sort_b.as_str()]);
        assert_eq!(value["meta"]["total"].as_u64(), Some(3));

        // Past the end is an empty page, not an error.
        let response = get(
            &app,
            &format!("/api/v1/issues?repo={SORT}&page=3&page_size=2"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert!(value["data"].as_array().expect("data").is_empty());
        assert_eq!(value["meta"]["total"].as_u64(), Some(3));

        // The documented cap is accepted; beyond it is a field error.
        let response = get(&app, &format!("/api/v1/issues?repo={SORT}&page_size=200")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let response = get(&app, &format!("/api/v1/issues?repo={SORT}&page_size=201")).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "validation_failed");
        assert!(value["error"]["fields"]["page_size"].is_string());
    }

    #[tokio::test]
    async fn list_stuck_filter_uses_the_owner_runner_interval() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues?repo={APP}&stuck=1")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.stuck_default.as_str()]);
        assert_eq!(value["data"][0]["stuck"].as_bool(), Some(true));

        // Same silence, different intervals: only the default runner is stuck.
        let response = get(&app, &format!("/api/v1/issues?repo={APP}&state=fixing")).await;
        let value = json_body(response).await;
        let by_fp: std::collections::BTreeMap<&str, bool> = value["data"]
            .as_array()
            .expect("data")
            .iter()
            .map(|item| {
                (
                    item["fingerprint"].as_str().expect("fp"),
                    item["stuck"].as_bool().expect("stuck"),
                )
            })
            .collect();
        assert_eq!(by_fp.get(seed.stuck_default.as_str()), Some(&true));
        assert_eq!(by_fp.get(seed.slow.as_str()), Some(&false));
        assert_eq!(by_fp.get(seed.healthy_fixing.as_str()), Some(&false));

        // stuck=0 is "no filter".
        let response = get(&app, &format!("/api/v1/issues?repo={SORT}&stuck=0")).await;
        let value = json_body(response).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(3));
    }

    #[tokio::test]
    async fn list_attention_matches_the_overview_set() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, "/api/v1/issues?attention=1").await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(
            fingerprint_set(&value),
            BTreeSet::from([
                seed.esc.clone(),
                seed.rejected.clone(),
                seed.stuck_default.clone(),
                seed.pr_recent.clone(),
            ]),
            "attention: {value}"
        );
        assert!(
            !fingerprints(&value).contains(&seed.slow),
            "the slow-runner issue is not stuck and must not be attention"
        );
        assert!(
            !fingerprints(&value).contains(&seed.pr_stale),
            "ci_timeout older than 24h is not attention"
        );

        // attention combines with the other filters.
        let response = get(&app, "/api/v1/issues?attention=1&state=escalated").await;
        let value = json_body(response).await;
        assert_eq!(fingerprints(&value), vec![seed.esc.as_str()]);

        let response = get(&app, &format!("/api/v1/issues?attention=0&repo={SORT}")).await;
        let value = json_body(response).await;
        assert_eq!(value["meta"]["total"].as_u64(), Some(3));
    }

    #[tokio::test]
    async fn list_rejects_unknown_filter_values_with_field_errors() {
        let (_dir, app, _seed) = seeded_app();

        let cases = [
            ("state=bogus", "state"),
            ("severity=bogus", "severity"),
            ("sort=bogus", "sort"),
            ("order=bogus", "order"),
            ("page=0", "page"),
            ("page=abc", "page"),
            ("page_size=0", "page_size"),
            ("page_size=abc", "page_size"),
        ];
        for (query, field) in cases {
            let response = get(&app, &format!("/api/v1/issues?{query}")).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "{query}"
            );
            assert_api_headers(&response);
            let value = json_body(response).await;
            assert_eq!(value["error"]["code"], "validation_failed", "{query}");
            assert!(
                value["error"]["fields"][field].is_string(),
                "{query} must report the {field} field: {value}"
            );
        }

        // Every bad field is reported at once.
        let response = get(&app, "/api/v1/issues?state=bogus&severity=nope").await;
        let value = json_body(response).await;
        assert!(value["error"]["fields"]["state"].is_string());
        assert!(value["error"]["fields"]["severity"].is_string());

        // q has a documented length limit.
        let long_q = "x".repeat(129);
        let response = get(&app, &format!("/api/v1/issues?q={long_q}")).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let value = json_body(response).await;
        assert!(value["error"]["fields"]["q"].is_string());
    }

    // ---- detail ------------------------------------------------------------

    #[tokio::test]
    async fn detail_returns_issue_stack_timeline_runs_and_similar() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues/{}", seed.detail)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_api_headers(&response);
        let value = json_body(response).await;

        let issue = &value["issue"];
        assert_eq!(issue["fingerprint"].as_str(), Some(seed.detail.as_str()));
        assert_eq!(issue["title"].as_str(), Some("detail crash"));
        assert_eq!(issue["state"].as_str(), Some("merged"));
        assert_eq!(issue["severity"].as_str(), Some("high"));
        assert_eq!(issue["occurrences"].as_u64(), Some(1));
        assert_eq!(issue["pr_url"].as_str(), Some(seed.detail_pr_url.as_str()));
        assert_eq!(issue["ci_verdict"].as_str(), Some("success"));
        assert_eq!(issue["stuck"].as_bool(), Some(false));
        assert_eq!(issue["last_seen_ms"].as_i64(), Some(seed.now - 200_000));
        assert_eq!(
            issue["state_changed_at_ms"].as_i64(),
            Some(seed.now - 150_000)
        );

        assert_eq!(
            value["stack_sample"].as_str(),
            Some(seed.detail_stack.as_str())
        );

        // Timeline is newest first and carries ms timestamps. `set_pr`
        // journals its own `pr_opened` row between the lifecycle transitions.
        let timeline = value["timeline"].as_array().expect("timeline");
        assert_eq!(timeline.len(), 6, "timeline: {timeline:?}");
        assert_eq!(timeline[0]["kind"].as_str(), Some("state_changed"));
        assert_eq!(
            timeline[0]["detail"].as_str(),
            Some(seed.detail_pr_url.as_str())
        );
        assert_eq!(timeline[0]["ts_ms"].as_i64(), Some(seed.now - 150_000));
        assert_eq!(timeline[0]["actor"].as_str(), Some("system"));
        assert_eq!(timeline[0]["repo"].as_str(), Some(APP));
        assert_eq!(timeline[1]["kind"].as_str(), Some("pr_opened"));
        assert_eq!(
            timeline[1]["detail"].as_str(),
            Some(seed.detail_pr_url.as_str())
        );
        assert_eq!(timeline[1]["actor"].as_str(), Some("runner"));
        assert_eq!(timeline[5]["kind"].as_str(), Some("reported"));
        assert_eq!(timeline[5]["ts_ms"].as_i64(), Some(seed.now - 200_000));

        // Newest 10 of 11 runs; run-0 falls off.
        let runs = value["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 10);
        assert_eq!(runs[0]["id"].as_str(), Some("run-10"));
        assert_eq!(runs[9]["id"].as_str(), Some("run-1"));
        assert_eq!(runs[0]["started_at_ms"].as_i64(), Some(seed.now - 90_000));
        assert_eq!(runs[0]["finished_at_ms"].as_i64(), Some(seed.now - 1_000));
        assert_eq!(runs[0]["phase"].as_str(), Some("done"));
        assert_eq!(runs[0]["outcome"].as_str(), Some("success"));
        assert_eq!(runs[0]["ci_verdict"].as_str(), Some("success"));
        assert_eq!(runs[0]["turns"].as_i64(), Some(6));
        assert_eq!(runs[0]["tokens_uncached"].as_i64(), Some(17_000));
        assert_eq!(runs[0]["tokens_cached"].as_i64(), Some(500));
        assert_eq!(runs[0]["provider"].as_str(), Some("faux"));
        assert_eq!(runs[0]["model"].as_str(), Some("faux"));
        assert_eq!(runs[0]["root_cause"].as_str(), Some("nil dereference"));
        assert_eq!(runs[0]["change_stat"].as_str(), Some("1 file changed"));
        assert_eq!(runs[0]["branch"].as_str(), Some("agent/fix/abc"));
        assert_eq!(runs[0]["commit_sha"].as_str(), Some("deadbeef"));
        assert_eq!(
            runs[0]["pr_url"].as_str(),
            Some(seed.detail_pr_url.as_str())
        );
        assert_eq!(runs[0]["pr_opened_at_ms"].as_i64(), Some(seed.now - 2_000));
        assert_eq!(runs[0]["verification_ok"].as_bool(), Some(true));
        assert_eq!(
            runs[0]["verification_detail"].as_str(),
            Some("verification passed")
        );
        assert_eq!(runs[0]["error"], serde_json::Value::Null);
        let run_object = runs[0].as_object().expect("run object");
        assert!(
            !run_object.contains_key("session_dir"),
            "session_dir is server-side only"
        );
        assert!(!run_object.contains_key("fingerprint"));
        assert!(!run_object.contains_key("runner_id"));

        // Similar: exact title, self excluded, newest first. The strip caps
        // at 5 items, the count is the whole same-title population.
        let similar = &value["similar"];
        assert_eq!(similar["count"].as_u64(), Some(6));
        let items = similar["items"].as_array().expect("similar items");
        assert_eq!(items.len(), 5);
        assert_eq!(
            items
                .iter()
                .map(|item| item["fingerprint"].as_str().expect("fp"))
                .collect::<Vec<_>>(),
            vec![
                seed.sim_new.as_str(),
                seed.sim_merged.as_str(),
                seed.sim_extras[0].as_str(),
                seed.sim_extras[1].as_str(),
                seed.sim_extras[2].as_str(),
            ]
        );
        assert_eq!(items[0]["state"].as_str(), Some("new"));
        assert_eq!(items[1]["state"].as_str(), Some("merged"));
        assert_eq!(items[0]["last_seen_ms"].as_i64(), Some(seed.now - 400_000));
        assert_eq!(items[4]["last_seen_ms"].as_i64(), Some(seed.now - 480_000));
        assert!(
            !items[0].as_object().expect("item").contains_key("title"),
            "similar rows share the detail title already"
        );
    }

    #[tokio::test]
    async fn detail_stuck_flag_uses_the_owner_runner_interval() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues/{}", seed.stuck_default)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["issue"]["stuck"].as_bool(), Some(true));

        let response = get(&app, &format!("/api/v1/issues/{}", seed.slow)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;
        assert_eq!(value["issue"]["stuck"].as_bool(), Some(false));
    }

    #[tokio::test]
    async fn detail_timeline_is_capped_at_200_newest_first() {
        let (_dir, app, seed) = seeded_app();

        let response = get(&app, &format!("/api/v1/issues/{}", seed.heavy)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let value = json_body(response).await;

        let timeline = value["timeline"].as_array().expect("timeline");
        assert_eq!(timeline.len(), 200);
        assert_eq!(timeline[0]["detail"].as_str(), Some("n204"));
        assert_eq!(timeline[199]["detail"].as_str(), Some("n5"));
        assert!(
            timeline
                .windows(2)
                .all(|pair| pair[0]["ts_ms"].as_i64() >= pair[1]["ts_ms"].as_i64()),
            "timeline must be newest first"
        );
        assert!(value["runs"].as_array().expect("runs").is_empty());
        assert_eq!(value["similar"]["count"].as_u64(), Some(0));
        assert!(value["similar"]["items"]
            .as_array()
            .expect("items")
            .is_empty());
    }

    #[tokio::test]
    async fn detail_unknown_fingerprint_is_404() {
        let (_dir, app, _seed) = seeded_app();

        let response = get(&app, "/api/v1/issues/does-not-exist").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_api_headers(&response);
        let value = json_body(response).await;
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["message"], "issue not found");
    }
}
