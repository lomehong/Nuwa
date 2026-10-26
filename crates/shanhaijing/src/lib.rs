//! 山海经 — the problem-issue store.
//!
//! Every system's collector (青鸟) reports signals here. The store fingerprints
//! them (see [`fingerprint`]), deduplicates, applies cooldowns, orders pickups
//! by priority (severity × occurrences), and enforces the issue state machine:
//!
//! ```text
//! new → triggered → fixing → pr_open → merged | rejected
//!                                 fixing ────────────→ escalated
//!                                 rejected ───────────→ escalated
//! merged/rejected + same fingerprint again → new (regression, reopened)
//! ```
//!
//! Storage is SQLite (one file per deployment), matching docs/07 A-1.

pub mod fingerprint;
mod migration;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// 每仓库保留的修复记忆条数。
pub const REPAIR_MEMORY_KEEP: usize = 50;

/// A raw collector report. One signal per incident batch; dedup happens here,
/// never at the collector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub repo: String,
    pub language: String,
    pub title: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    /// Collector-parsed error headline (first log line). Optional: derived
    /// from the stack when absent.
    #[serde(default)]
    pub message: Option<String>,
    pub stack_trace: String,
    /// Log lines around the incident, when the collector sends them. The
    /// repair prompt shows this window to the agent — without it the agent
    /// sees the stack and nothing else about the failure.
    #[serde(default)]
    pub log_window: Vec<String>,
}

fn default_severity() -> String {
    "medium".to_string()
}

/// Issue lifecycle states (docs/07 A-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    New,
    Triggered,
    Fixing,
    PrOpen,
    Merged,
    Rejected,
    Escalated,
    /// Owner mute / attempt cap. Reports never auto-reopen it.
    Ignored,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::New => "new",
            State::Triggered => "triggered",
            State::Fixing => "fixing",
            State::PrOpen => "pr_open",
            State::Merged => "merged",
            State::Rejected => "rejected",
            State::Escalated => "escalated",
            State::Ignored => "ignored",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "new" => State::New,
            "triggered" => State::Triggered,
            "fixing" => State::Fixing,
            "pr_open" => State::PrOpen,
            "merged" => State::Merged,
            "rejected" => State::Rejected,
            "escalated" => State::Escalated,
            "ignored" => State::Ignored,
            _ => return None,
        })
    }
}

/// Allowed transitions. `(Merged|Rejected, New)` is the regression-reopen
/// path: a fixed bug reporting again is a new cycle, same fingerprint.
/// `(Escalated, New)` is the human path — only via the explicit `nuwa reopen`
/// command after an owner reviews the escalation; reports never auto-reopen
/// an escalated issue.
pub fn can_transition(from: State, to: State) -> bool {
    use State::*;
    matches!(
        (from, to),
        (New, Triggered)
            | (Triggered, Fixing)
            | (Triggered, Escalated)
            | (Fixing, PrOpen)
            | (Fixing, Escalated)
            | (PrOpen, Merged)
            | (PrOpen, Rejected)
            | (Rejected, Escalated)
            | (New, Ignored)
            | (Triggered, Ignored)
            | (Escalated, Ignored)
            | (Rejected, Ignored)
            | (Merged, New)
            | (Rejected, New)
            | (Escalated, New)
            | (Ignored, New)
    )
}

#[derive(Debug, thiserror::Error)]
pub enum ShanError {
    #[error("db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("invalid transition {} -> {}", from.as_str(), to.as_str())]
    InvalidTransition { from: State, to: State },
    #[error("unknown issue {0}")]
    UnknownIssue(String),
    #[error("unknown state string {0}")]
    UnknownState(String),
    #[error("unknown runner {0}")]
    UnknownRunner(String),
    #[error("unknown run {0}")]
    UnknownRun(String),
    #[error("repo {0} already exists")]
    RepoExists(String),
    #[error("issue {0} is not stuck")]
    NotStuck(String),
}

pub type ShanResult<T> = Result<T, ShanError>;

/// What reporting a signal decided.
#[derive(Debug, Clone, Serialize)]
pub enum ReportOutcome {
    /// First sighting — eligible for pickup.
    NewIssue { fingerprint: String },
    /// Same fingerprint, already tracked, currently pickable.
    Deduped {
        fingerprint: String,
        occurrences: u64,
    },
    /// Same fingerprint inside its cooldown — counted, not re-triggered.
    CooldownActive {
        fingerprint: String,
        occurrences: u64,
        retry_after_ms: i64,
    },
    /// Terminal-state issue reported again: regression, reopened as `new`.
    Regression {
        fingerprint: String,
        occurrences: u64,
    },
    /// Owner-muted or attempt-capped; reports are counted but never reopen it.
    Muted {
        fingerprint: String,
        occurrences: u64,
    },
}

/// A pickable issue handed to the runner pool.
#[derive(Debug, Clone, Serialize)]
pub struct IssueSnapshot {
    pub fingerprint: String,
    pub title: String,
    pub language: String,
    pub repo: String,
    pub severity: String,
    pub occurrences: u64,
    pub attempts: u64,
    pub stack_sample: String,
    /// Log lines around the incident, as the collector reported them.
    pub log_window: Vec<String>,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Full issue row — the `nuwa status` / ops view.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct IssueRow {
    pub fingerprint: String,
    pub title: String,
    pub language: String,
    pub repo: String,
    pub severity: String,
    pub state: String,
    pub occurrences: u64,
    pub attempts: u64,
    pub pr_url: Option<String>,
    #[serde(rename = "first_seen_ms")]
    pub first_seen: i64,
    #[serde(rename = "last_seen_ms")]
    pub last_seen: i64,
    #[serde(rename = "state_changed_at_ms")]
    pub state_changed_at: i64,
    pub owner_runner_id: Option<String>,
    /// Verdict of the newest run for this fingerprint, if any.
    pub ci_verdict: Option<String>,
}

/// One lifecycle event — console activity feed row.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct EventRow {
    pub fingerprint: String,
    pub ts: i64,
    pub kind: String,
    pub detail: String,
    pub repo: String,
    pub actor: Option<String>,
}

/// 一条已验证的修复记忆。来源（提交/PR/时间）必须随记忆一起保存：
/// 没有来源的"经验"无法被复核，也就不能进入下一次修复的上下文。
#[derive(Debug, Clone)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepairMemoryEntry {
    pub repo: String,
    pub fingerprint: String,
    pub commit_sha: String,
    pub pr_url: Option<String>,
    pub root_cause: String,
    pub change_stat: String,
    pub files: i64,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairMemoryRow {
    pub fingerprint: String,
    pub commit_sha: String,
    pub pr_url: Option<String>,
    pub root_cause: String,
    pub change_stat: String,
    pub files: i64,
    pub recorded_at: i64,
}

/// 控制台记忆列表行：内容 + 来源 + 所属问题单 + 是否停用。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepairMemoryView {
    pub fingerprint: String,
    /// 问题单标题；问题单已被清理时为空。
    pub title: String,
    pub state: String,
    pub commit_sha: String,
    pub pr_url: Option<String>,
    pub root_cause: String,
    pub change_stat: String,
    pub files: i64,
    pub recorded_at: i64,
    pub disabled: bool,
}

#[derive(Debug, Clone)]
pub struct BenchmarkStart {
    pub id: String,
    pub started_at: i64,
    pub provider: String,
    pub model: Option<String>,
    /// `faux`（确定性）或 `live`（真实 provider）。
    pub mode: String,
    pub scenarios: usize,
}

#[derive(Debug, Clone)]
pub struct BenchmarkFinish {
    pub finished_at: i64,
    pub status: String,
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

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkRunRow {
    pub id: String,
    pub started_at: i64,
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

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkResultInput {
    pub run_id: String,
    pub scenario: String,
    pub language: String,
    pub expected_fixed: bool,
    pub repaired: bool,
    pub claimed_fixed: bool,
    pub delivery_ok: bool,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    pub duration_ms: i64,
    pub root_cause: Option<String>,
    pub final_report: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BenchmarkResultRow {
    pub run_id: String,
    pub scenario: String,
    pub language: String,
    pub expected_fixed: bool,
    pub repaired: bool,
    pub claimed_fixed: bool,
    pub delivery_ok: bool,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    pub duration_ms: i64,
    pub root_cause: Option<String>,
    pub final_report: Option<String>,
    pub error: Option<String>,
}

/// Filters for the console event feed. An empty `kinds` list means "every
/// kind"; `from_ms`/`to_ms` are inclusive bounds on `events.ts`.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub kinds: Vec<String>,
    pub repo: Option<String>,
    pub fingerprint: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

/// One UTC day of the console trends chart. Aggregates are window-scoped by
/// the optional repo filter; days without data are zero-filled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TrendBucket {
    pub day: String,
    pub reports: u64,
    pub new_issues: u64,
    pub prs_opened: u64,
    pub merged: u64,
    pub rejected: u64,
    pub tokens: u64,
    pub ci_green: u64,
    pub ci_red: u64,
}

#[derive(Debug, Clone)]
pub struct RunnerUpsert {
    pub id: String,
    pub repo: String,
    pub host: String,
    pub pid: Option<i64>,
    pub started_at: i64,
    pub heartbeat_at: i64,
    pub status: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub daily_pr_limit: Option<i64>,
    pub repo_path: Option<String>,
    pub service_unit: Option<String>,
    pub config_json: String,
}

/// Result of trying to become the single live process for a runner id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerLeaseOutcome {
    /// This process now owns the id and the checkout.
    Acquired,
    /// Another live process owns this runner id.
    HeldByRunner,
    /// A different runner id is using the same checkout path. Two processes
    /// sharing one working tree would reset each other mid-repair, so this is
    /// a configuration error rather than something to wait for.
    CheckoutInUse(String),
}

#[derive(Debug, Clone)]
pub struct RunnerHeartbeat {
    pub heartbeat_at: i64,
    pub status: String,
    pub current_fingerprint: Option<String>,
    pub phase: Option<String>,
    pub config_json: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RunnerRow {
    pub id: String,
    pub repo: String,
    pub host: String,
    pub pid: Option<i64>,
    pub started_at: i64,
    pub heartbeat_at: i64,
    /// Last real progress (phase change/turn), distinct from liveness.
    pub progress_at: i64,
    pub status: String,
    pub current_fingerprint: Option<String>,
    pub phase: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub daily_pr_limit: Option<i64>,
    pub repo_path: Option<String>,
    pub service_unit: Option<String>,
    pub config_json: String,
    pub last_error: Option<String>,
}

/// One registered repository — the console registry's display identity.
/// Repository content (`failing_check`, `forbidden_paths`, `notes`) stays in
/// the target repo's `nuwa.repo.json` and is deliberately not stored here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RepoRow {
    pub repo: String,
    pub name: String,
    pub url: String,
    pub description: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct RunStart {
    pub id: String,
    pub fingerprint: String,
    pub runner_id: String,
    pub repo: String,
    pub started_at: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RunUpdate {
    pub phase: Option<String>,
    pub turns: Option<i64>,
    pub tokens_uncached: Option<i64>,
    pub tokens_cached: Option<i64>,
    pub compactions: Option<i64>,
    pub context_window: Option<i64>,
    pub messages: Option<i64>,
    pub harness_run_id: Option<String>,
    pub root_cause: Option<String>,
    pub verification_ok: Option<bool>,
    pub verification_detail: Option<String>,
    pub change_stat: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    pub pr_opened_at: Option<i64>,
    pub ci_verdict: Option<String>,
    pub session_dir: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunFinish {
    pub finished_at: i64,
    pub outcome: String,
    pub ci_verdict: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RunRow {
    pub id: String,
    pub harness_run_id: Option<String>,
    pub fingerprint: String,
    pub runner_id: String,
    pub repo: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub phase: String,
    pub outcome: Option<String>,
    pub ci_verdict: Option<String>,
    pub turns: i64,
    pub tokens_uncached: i64,
    pub tokens_cached: i64,
    /// 自动压缩次数（harness CompactionProgress，manual=false）。
    pub compactions: i64,
    /// 模型上下文窗口（token）；0 = 未知。
    pub context_window: i64,
    /// run 结束时会话里的消息数。
    pub messages: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub root_cause: Option<String>,
    pub change_stat: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub pr_url: Option<String>,
    pub pr_opened_at: Option<i64>,
    pub verification_ok: Option<bool>,
    pub verification_detail: Option<String>,
    pub session_dir: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDisposition {
    Retry,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureOutcome {
    pub disposition: FailureDisposition,
    pub state: State,
    pub attempts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Intent,
    Pushed,
    PrRequested,
    PrCreated,
    Ambiguous,
    Failed,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Pushed => "pushed",
            Self::PrRequested => "pr_requested",
            Self::PrCreated => "pr_created",
            Self::Ambiguous => "ambiguous",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "intent" => Self::Intent,
            "pushed" => Self::Pushed,
            "pr_requested" => Self::PrRequested,
            "pr_created" => Self::PrCreated,
            "ambiguous" => Self::Ambiguous,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DeliveryStart {
    pub id: String,
    pub run_id: Option<String>,
    pub fingerprint: String,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub head_sha: String,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct DeliveryRow {
    pub id: String,
    pub run_id: Option<String>,
    pub fingerprint: String,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub head_sha: String,
    pub state: DeliveryState,
    pub pr_url: Option<String>,
    pub pr_number: Option<u64>,
    pub detail: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiWatchState {
    Pending,
    Green,
    Red,
    Merged,
    Closed,
    Expired,
}

impl CiWatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Green => "green",
            Self::Red => "red",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "green" => Self::Green,
            "red" => Self::Red,
            "merged" => Self::Merged,
            "closed" => Self::Closed,
            "expired" => Self::Expired,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Red | Self::Merged | Self::Closed | Self::Expired
        )
    }
}

#[derive(Debug, Clone)]
pub struct CiWatchInput {
    pub run_id: Option<String>,
    pub fingerprint: String,
    pub repo: String,
    pub pr_url: String,
    pub slug: String,
    pub pr_number: u64,
    pub head_sha: String,
    pub root_cause: Option<String>,
    pub change_stat: Option<String>,
    pub now: i64,
    pub next_check_at: i64,
    pub max_attempts: u64,
}

#[derive(Debug, Clone)]
pub struct CiWatchRow {
    pub id: i64,
    pub run_id: Option<String>,
    pub fingerprint: String,
    pub repo: String,
    pub pr_url: String,
    pub slug: String,
    pub pr_number: u64,
    pub head_sha: String,
    pub state: CiWatchState,
    pub last_verdict: Option<String>,
    pub last_checked_at: Option<i64>,
    pub next_check_at: i64,
    pub check_attempts: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub terminal_at: Option<i64>,
    pub last_error: Option<String>,
    pub root_cause: Option<String>,
    pub change_stat: Option<String>,
    pub memory_written: bool,
    pub max_attempts: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS issues (
    fingerprint   TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    language      TEXT NOT NULL,
    severity      TEXT NOT NULL DEFAULT 'medium',
    state         TEXT NOT NULL DEFAULT 'new',
    occurrences   INTEGER NOT NULL DEFAULT 1,
    first_seen    INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    cooldown_until INTEGER NOT NULL DEFAULT 0,
    repo          TEXT NOT NULL,
    stack_sample  TEXT NOT NULL,
    pr_url        TEXT,
    pr_opened_at  INTEGER
);
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT NOT NULL,
    ts          INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    detail      TEXT
);
CREATE INDEX IF NOT EXISTS idx_events_fp ON events(fingerprint);
CREATE TABLE IF NOT EXISTS heartbeat (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    ts INTEGER NOT NULL
);
";

/// Claim cooldown: while a run is in flight, further reports of the same
/// fingerprint only count occurrences (ADR-002 — no re-triggering).
pub const DEFAULT_COOLDOWN_MS: i64 = 6 * 60 * 60 * 1000;

/// UTC day width for the trends buckets (UTC has no DST).
const DAY_MS: i64 = 86_400_000;

/// Console event feed page cap — a hostile page size must not scan the whole
/// journal.
const EVENT_LIST_CAP: u32 = 5_000;

/// Priority ordering shared by the claim and observer queries: severity is
/// lexicographically first, so "most severe" beats "noisiest"; occurrences
/// and age only break ties inside a severity band.
const READY_ORDER: &str = "CASE severity
        WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END DESC,
        occurrences DESC,
        last_seen ASC";

/// Severity rank mirroring the `READY_ORDER` CASE band. Unknown spellings rank
/// 0, so they never outweigh a recognized severity.
fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// The more severe of two severity labels; on a tie the already-stored value
/// wins, so a repeat report never churns the stored severity sideways.
fn higher_severity(current: &str, incoming: &str) -> String {
    if severity_rank(incoming) > severity_rank(current) {
        incoming.to_string()
    } else {
        current.to_string()
    }
}

/// Full [`IssueRow`] projection; keep aligned with [`Store::issue_from_row`].
const ISSUE_VIEW_SELECT: &str = "SELECT i.fingerprint, i.title, i.language, i.repo, i.severity,
        i.state, i.occurrences, i.pr_url, i.first_seen, i.last_seen,
        CASE WHEN i.state_changed_at = 0 THEN i.first_seen ELSE i.state_changed_at END AS state_changed_at,
        i.owner_runner_id,
        (SELECT r.ci_verdict FROM runs r
         WHERE r.fingerprint = i.fingerprint
         ORDER BY r.started_at DESC, r.id DESC LIMIT 1) AS ci_verdict,
        i.attempts
 FROM issues i";

/// Event projection shared by the feed/detail/timeline queries; legacy rows
/// with `events.repo` NULL fall back to the issue repo. Keep aligned with
/// [`Store::event_from_row`].
const EVENT_SELECT: &str =
    "SELECT e.fingerprint, e.ts, COALESCE(e.kind, ''), COALESCE(e.detail, ''),
        COALESCE(e.repo, i.repo, ''), e.actor
 FROM events e LEFT JOIN issues i ON i.fingerprint = e.fingerprint";

/// Full [`RunRow`] projection; keep aligned with [`Store::run_from_row`].
const RUN_COLUMNS: &str =
    "id, harness_run_id, fingerprint, runner_id, repo, started_at, finished_at,
        phase, outcome, ci_verdict, turns, tokens_uncached, tokens_cached, provider,
        model, root_cause, change_stat, branch, commit_sha, pr_url, pr_opened_at,
        verification_ok, verification_detail, session_dir, error, compactions,
        context_window, messages";

/// Runner telemetry upsert shared by the plain path and the lease acquisition,
/// so both keep the same column list. Lease columns are never written here.
fn upsert_runner_on(conn: &Connection, r: &RunnerUpsert) -> ShanResult<()> {
    conn.execute(
        "INSERT INTO runners(id, repo, host, pid, started_at, heartbeat_at, status,
                             current_fingerprint, phase, provider, model, daily_pr_limit,
                             repo_path, service_unit, config_json, last_error, progress_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,NULL,?8,?9,?10,?11,?12,?13,NULL,?5)
         ON CONFLICT(id) DO UPDATE SET
            repo=excluded.repo, host=excluded.host, pid=excluded.pid,
            started_at=excluded.started_at, heartbeat_at=excluded.heartbeat_at,
            status=excluded.status, current_fingerprint=NULL, phase=NULL,
            provider=excluded.provider, model=excluded.model,
            daily_pr_limit=excluded.daily_pr_limit, repo_path=excluded.repo_path,
            service_unit=excluded.service_unit, config_json=excluded.config_json,
            last_error=NULL, progress_at=excluded.progress_at",
        rusqlite::params![
            r.id,
            r.repo,
            r.host,
            r.pid,
            r.started_at,
            r.heartbeat_at,
            r.status,
            r.provider,
            r.model,
            r.daily_pr_limit,
            r.repo_path,
            r.service_unit,
            r.config_json,
        ],
    )?;
    Ok(())
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> ShanResult<Self> {
        let mut conn = Connection::open(path.as_ref())?;
        migration::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic in another thread must not wedge the store forever.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fingerprint a signal, dedup, and decide whether it may trigger a run.
    /// Every report also bumps the per-day aggregate in `signal_daily`; only
    /// the first sighting writes a `reported` event (dedup must not spam the
    /// activity feed).
    pub fn report_signal(&self, signal: &Signal, now_ms: i64) -> ShanResult<ReportOutcome> {
        let fp = fingerprint::compute(
            &signal.repo,
            &signal.language,
            signal.message.as_deref(),
            &signal.stack_trace,
        );
        let mut conn = self.lock();
        // Serialize before reading the state: a concurrent owner mute must not
        // be reopened using a stale terminal-state observation.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, u64, i64, String)> = tx
            .query_row(
                "SELECT state, occurrences, cooldown_until, severity FROM issues WHERE fingerprint = ?1",
                [&fp],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;

        let Some((state, occurrences, cooldown_until, current_severity)) = existing else {
            tx.execute(
                "INSERT INTO issues(fingerprint, title, language, severity, state, occurrences, first_seen, last_seen, cooldown_until, repo, stack_sample, log_sample)
                 VALUES (?1, ?2, ?3, ?4, 'new', 1, ?5, ?5, 0, ?6, ?7, ?8)",
                rusqlite::params![
                    fp,
                    signal.title,
                    signal.language,
                    signal.severity,
                    now_ms,
                    signal.repo,
                    signal.stack_trace,
                    encode_log_window(&signal.log_window),
                ],
            )?;
            bump_signal_daily(&tx, &fp, &signal.repo, now_ms)?;
            record_event_on(
                &tx,
                &fp,
                now_ms,
                Some(&signal.repo),
                "system",
                "reported",
                "first sighting",
            )?;
            tx.commit()?;
            return Ok(ReportOutcome::NewIssue { fingerprint: fp });
        };

        // A repeat report must never downgrade a confirmed severity: a `low`
        // (or unspecified → `medium`) re-report of a `critical` bug would
        // otherwise sink it in the claim order. Keep the higher of the two;
        // downgrading is an explicit owner action, not a side effect of noise.
        let severity = higher_severity(&current_severity, &signal.severity);
        tx.execute(
            "UPDATE issues SET occurrences = occurrences + 1, last_seen = ?2, stack_sample = ?3,
                severity = ?4, log_sample = COALESCE(?5, log_sample)
             WHERE fingerprint = ?1",
            rusqlite::params![
                fp,
                now_ms,
                signal.stack_trace,
                severity,
                encode_log_window(&signal.log_window)
            ],
        )?;
        bump_signal_daily(&tx, &fp, &signal.repo, now_ms)?;
        let occurrences = occurrences + 1;

        let state = State::parse(&state).ok_or(ShanError::UnknownState(state))?;
        let outcome = if state == State::Ignored {
            ReportOutcome::Muted {
                fingerprint: fp,
                occurrences,
            }
        } else if matches!(state, State::Merged | State::Rejected) {
            // Automatic regression is narrower than the owner's generic
            // transition API, which also permits Ignored/Escalated -> New.
            tx.execute(
                "UPDATE issues SET state='new', state_changed_at=?2,
                    cooldown_until=0, owner_runner_id=NULL
                 WHERE fingerprint=?1 AND state IN ('merged', 'rejected')",
                rusqlite::params![fp, now_ms],
            )?;
            record_event_on(
                &tx,
                &fp,
                now_ms,
                Some(&signal.repo),
                "system",
                "state_changed",
                "regression: terminal issue reported again",
            )?;
            ReportOutcome::Regression {
                fingerprint: fp,
                occurrences,
            }
        } else if now_ms < cooldown_until {
            ReportOutcome::CooldownActive {
                fingerprint: fp,
                occurrences,
                retry_after_ms: cooldown_until - now_ms,
            }
        } else {
            ReportOutcome::Deduped {
                fingerprint: fp,
                occurrences,
            }
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// Highest-priority pickable issue: state `new`, cooldown passed,
    /// ordered by `severity_weight × occurrences` then oldest sighting.
    pub fn ready_issue(&self, now_ms: i64) -> ShanResult<Option<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT fingerprint, title, language, repo, severity, occurrences, attempts, stack_sample, log_sample, first_seen, last_seen
             FROM issues
             WHERE state = 'new' AND cooldown_until <= ?1
             ORDER BY {READY_ORDER}
             LIMIT 1"
        ))?;
        let mut rows = stmt.query([now_ms])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::snapshot_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Pickable issues in claim order, untouched — the console queue view.
    pub fn ready_issues(&self, now_ms: i64, limit: u32) -> ShanResult<Vec<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT fingerprint, title, language, repo, severity, occurrences, attempts, stack_sample, log_sample, first_seen, last_seen
             FROM issues
             WHERE state = 'new' AND cooldown_until <= ?1
             ORDER BY {READY_ORDER}
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![now_ms, limit], Self::snapshot_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Size of the pickable queue at `now_ms` (same predicate as claim).
    pub fn count_ready(&self, now_ms: i64) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE state = 'new' AND cooldown_until <= ?1",
            [now_ms],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Size of this repo's pickable queue (same predicate as the repo-scoped
    /// claim). The runner uses it to decide whether it is worth reaching for
    /// the network: an idle runner polls every 30–60s and a fetch per poll
    /// was a pointless round trip to GitHub.
    pub fn count_ready_for_repo(&self, now_ms: i64, repo: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issues
             WHERE state = 'new' AND cooldown_until <= ?1 AND repo = ?2",
            rusqlite::params![now_ms, repo],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueSnapshot> {
        let log_sample: Option<String> = row.get(8)?;
        Ok(IssueSnapshot {
            fingerprint: row.get(0)?,
            title: row.get(1)?,
            language: row.get(2)?,
            repo: row.get(3)?,
            severity: row.get(4)?,
            occurrences: row.get(5)?,
            attempts: row.get(6)?,
            stack_sample: row.get(7)?,
            log_window: decode_log_window(log_sample.as_deref()),
            first_seen: row.get(9)?,
            last_seen: row.get(10)?,
        })
    }

    /// Atomically claim the highest-priority pickable issue: flips it to
    /// `triggered` and starts its cooldown in the same statement that returns
    /// it, so N concurrent runners can never pick up the same issue
    /// (ready_issue is only an observation; this is the checkout).
    pub fn claim_ready_issue(
        &self,
        now_ms: i64,
        cooldown_until_ms: i64,
    ) -> ShanResult<Option<IssueSnapshot>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claimed: Option<IssueSnapshot> = tx
            .query_row(
                &format!(
                    "UPDATE issues SET state = 'triggered', cooldown_until = ?2, state_changed_at = ?1
                     WHERE fingerprint = (
                         SELECT fingerprint FROM issues
                         WHERE state = 'new' AND cooldown_until <= ?1
                         ORDER BY {READY_ORDER}
                         LIMIT 1
                     )
                     RETURNING fingerprint, title, language, repo, severity, occurrences, attempts, stack_sample, log_sample, first_seen, last_seen"
                ),
                rusqlite::params![now_ms, cooldown_until_ms],
                Self::snapshot_from_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(snapshot) = &claimed {
            record_event_on(
                &tx,
                &snapshot.fingerprint,
                now_ms,
                Some(&snapshot.repo),
                "runner",
                "state_changed",
                "claimed by runner",
            )?;
        }
        tx.commit()?;
        Ok(claimed)
    }

    /// Repo-scoped atomic claim: a runner must never pick up another repo's
    /// issue just because it shares a database. The unfiltered claim remains
    /// for tests and tooling.
    ///
    /// The claim stamps `owner_runner_id` in the same atomic statement. Without
    /// this there is a window between the claim (state→triggered) and
    /// `start_run` (which also sets the owner) during which the row has a NULL
    /// owner; a second runner's startup `revive_orphaned_fixing` would treat
    /// that live claim as an orphan and requeue it, letting two runners work
    /// the same issue. Owning it at claim time closes the window.
    pub fn claim_ready_issue_for_repo(
        &self,
        now_ms: i64,
        cooldown_until_ms: i64,
        repo: &str,
        runner_id: &str,
    ) -> ShanResult<Option<IssueSnapshot>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claimed: Option<IssueSnapshot> = tx
            .query_row(
                &format!(
                    "UPDATE issues SET state = 'triggered', cooldown_until = ?2, state_changed_at = ?1, owner_runner_id = ?4
                     WHERE fingerprint = (
                         SELECT fingerprint FROM issues
                         WHERE state = 'new' AND cooldown_until <= ?1 AND repo = ?3
                         ORDER BY {READY_ORDER}
                         LIMIT 1
                     )
                     RETURNING fingerprint, title, language, repo, severity, occurrences, attempts, stack_sample, log_sample, first_seen, last_seen"
                ),
                rusqlite::params![now_ms, cooldown_until_ms, repo, runner_id],
                Self::snapshot_from_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(snapshot) = &claimed {
            record_event_on(
                &tx,
                &snapshot.fingerprint,
                now_ms,
                Some(&snapshot.repo),
                "runner",
                "state_changed",
                "claimed by runner",
            )?;
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn stuck(&self, fingerprint: &str, now_ms: i64, stuck_after_ms: i64) -> ShanResult<bool> {
        let conn = self.lock();
        let (state, state_changed_at, owner): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT state, state_changed_at, owner_runner_id FROM issues WHERE fingerprint=?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ShanError::UnknownIssue(fingerprint.to_string())
                }
                other => ShanError::Db(other),
            })?;
        if state != "triggered" && state != "fixing" {
            return Ok(false);
        }

        let owner_state: Option<(String, Option<String>, i64, i64)> = match owner.as_deref() {
            Some(id) => conn
                .query_row(
                    "SELECT status, current_fingerprint, heartbeat_at, progress_at
                     FROM runners WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?,
            None => None,
        };

        let (owner_heartbeat, healthy_owner) = match &owner_state {
            Some((status, current, heartbeat, progress)) => {
                // Progress is the real liveness signal only while this runner
                // is actively working this issue. Idle/other-owner heartbeats
                // are still the meaningful timestamp for an abandoned issue.
                let working = status == "working" && current.as_deref() == Some(fingerprint);
                let last = if working && *progress > 0 {
                    *progress
                } else {
                    *heartbeat
                };
                (Some(last), working && now_ms - last <= stuck_after_ms)
            }
            None => (None, false),
        };
        if healthy_owner {
            return Ok(false);
        }

        let last_progress = state_changed_at.max(owner_heartbeat.unwrap_or(0));
        Ok(now_ms - last_progress > stuck_after_ms)
    }

    /// Requeue a `triggered`/`fixing` issue as `new` so the pool can claim it
    /// again. Without `force`, the issue must first be [`Store::stuck`].
    pub fn requeue_stuck(
        &self,
        fingerprint: &str,
        force: bool,
        now_ms: i64,
        stuck_after_ms: i64,
    ) -> ShanResult<()> {
        if !force && !self.stuck(fingerprint, now_ms, stuck_after_ms)? {
            return Err(ShanError::NotStuck(fingerprint.to_string()));
        }
        let conn = self.lock();
        let (state, repo): (String, String) = conn
            .query_row(
                "SELECT state, repo FROM issues WHERE fingerprint=?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ShanError::UnknownIssue(fingerprint.to_string())
                }
                other => ShanError::Db(other),
            })?;
        let from = State::parse(&state).ok_or_else(|| ShanError::UnknownState(state))?;
        if !matches!(from, State::Triggered | State::Fixing) {
            return Err(ShanError::InvalidTransition {
                from,
                to: State::New,
            });
        }
        conn.execute(
            "UPDATE issues SET state='new', cooldown_until=0, owner_runner_id=NULL,
                state_changed_at=?2 WHERE fingerprint=?1",
            rusqlite::params![fingerprint, now_ms],
        )?;
        drop(conn);
        self.record_event(
            fingerprint,
            now_ms,
            Some(&repo),
            "owner",
            "requeued",
            if force {
                "forced by owner"
            } else {
                "stuck requeued by owner"
            },
        )
    }

    /// Revive every issue stuck in a claimed-but-unfinished state (`triggered`
    /// or `fixing`). Called once at runner startup: a crash/restart orphans the
    /// previous process's in-flight repairs, and the claim path only picks up
    /// `new` — without this a crash mid-repair would strand the issue forever.
    /// Returns how many issues were revived.
    pub fn revive_orphaned_fixing(&self, now_ms: i64, runner_id: &str) -> ShanResult<usize> {
        let revived: Vec<(String, String)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT fingerprint, repo FROM issues
                 WHERE state IN ('triggered','fixing')
                   AND (owner_runner_id = ?1 OR owner_runner_id IS NULL)",
            )?;
            let rows = stmt.query_map([runner_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut count = 0;
        for (fp, repo) in revived {
            {
                let conn = self.lock();
                conn.execute(
                    "UPDATE issues SET state='new', cooldown_until=0, owner_runner_id=NULL,
                        state_changed_at=?2 WHERE fingerprint=?1 AND state IN ('triggered','fixing')
                        AND (owner_runner_id = ?3 OR owner_runner_id IS NULL)",
                    rusqlite::params![fp, now_ms, runner_id],
                )?;
            }
            self.record_event(
                &fp,
                now_ms,
                Some(&repo),
                runner_id,
                "requeued",
                "orphaned repair revived at runner startup",
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// Reopen an `escalated`/`rejected`/`ignored` issue as `new` (owner
    /// decision). The owner's reason is journaled and an explicit reopen
    /// resets the attempt counter — the cap only protects automatic loops.
    pub fn reopen_issue(
        &self,
        fingerprint: &str,
        now_ms: i64,
        reason: Option<&str>,
    ) -> ShanResult<()> {
        let conn = self.lock();
        let (state, repo): (String, String) = conn
            .query_row(
                "SELECT state, repo FROM issues WHERE fingerprint=?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ShanError::UnknownIssue(fingerprint.to_string())
                }
                other => ShanError::Db(other),
            })?;
        let from = State::parse(&state).ok_or_else(|| ShanError::UnknownState(state))?;
        if !matches!(from, State::Escalated | State::Rejected | State::Ignored) {
            return Err(ShanError::InvalidTransition {
                from,
                to: State::New,
            });
        }
        conn.execute(
            "UPDATE issues SET state='new', cooldown_until=0, owner_runner_id=NULL,
                attempts=0, state_changed_at=?2 WHERE fingerprint=?1",
            rusqlite::params![fingerprint, now_ms],
        )?;
        drop(conn);
        let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
        let detail = match reason {
            Some(reason) => format!(
                "reopened by owner: {}",
                reason.chars().take(2_000).collect::<String>()
            ),
            None => "reopened by owner".to_string(),
        };
        self.record_event(
            fingerprint,
            now_ms,
            Some(&repo),
            "owner",
            "state_changed",
            &detail,
        )
    }

    /// Internal retry: put a `triggered`/`fixing` issue back to `new` with a
    /// short cooldown. Used for transient provider/environment failures; the
    /// attempt counter is intentionally not touched.
    pub fn release_issue(
        &self,
        fingerprint: &str,
        reason: &str,
        cooldown_until_ms: i64,
        now_ms: i64,
    ) -> ShanResult<()> {
        let conn = self.lock();
        let (state, repo): (String, String) = conn
            .query_row(
                "SELECT state, repo FROM issues WHERE fingerprint=?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ShanError::UnknownIssue(fingerprint.to_string())
                }
                other => ShanError::Db(other),
            })?;
        let from = State::parse(&state).ok_or_else(|| ShanError::UnknownState(state))?;
        if !matches!(from, State::Triggered | State::Fixing) {
            return Err(ShanError::InvalidTransition {
                from,
                to: State::New,
            });
        }
        conn.execute(
            "UPDATE issues SET state='new', cooldown_until=?2, owner_runner_id=NULL,
                state_changed_at=?3 WHERE fingerprint=?1",
            rusqlite::params![fingerprint, cooldown_until_ms, now_ms],
        )?;
        drop(conn);
        self.record_event(
            fingerprint,
            now_ms,
            Some(&repo),
            "runner",
            "requeued",
            reason,
        )
    }

    /// Owner mute or attempt-cap: `ignored` is terminal and reports never
    /// auto-reopen it. The reason is journaled and visible to future prompts.
    pub fn ignore_issue(
        &self,
        fingerprint: &str,
        reason: &str,
        actor: &str,
        now_ms: i64,
    ) -> ShanResult<()> {
        let reason = reason.trim();
        let detail = if reason.is_empty() {
            "ignored".to_string()
        } else {
            format!(
                "ignored: {}",
                reason.chars().take(2_000).collect::<String>()
            )
        };
        self.set_state_as(fingerprint, State::Ignored, now_ms, Some(&detail), actor)?;
        let conn = self.lock();
        conn.execute(
            "UPDATE issues SET owner_runner_id=NULL WHERE fingerprint=?1",
            [fingerprint],
        )?;
        Ok(())
    }

    /// Reject the identified open PR once, counting the failed attempt and both
    /// audit events atomically. A duplicate, stale state or mismatched PR URL
    /// (including no stored URL) is a no-op.
    pub fn reject_pr(
        &self,
        fingerprint: &str,
        expected_pr_url: &str,
        now_ms: i64,
        detail: &str,
        actor: &str,
    ) -> ShanResult<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let repo: Option<String> = tx
            .query_row(
                "UPDATE issues SET state='rejected', attempts=attempts+1, state_changed_at=?2
                 WHERE fingerprint=?1 AND state='pr_open' AND pr_url=?3 RETURNING repo",
                rusqlite::params![fingerprint, now_ms, expected_pr_url],
                |row| row.get(0),
            )
            .optional()?;
        let Some(repo) = repo else {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM issues WHERE fingerprint=?1)",
                [fingerprint],
                |row| row.get(0),
            )?;
            return if exists {
                Ok(false)
            } else {
                Err(ShanError::UnknownIssue(fingerprint.to_string()))
            };
        };
        for kind in ["state_changed", "rejected"] {
            record_event_on(&tx, fingerprint, now_ms, Some(&repo), actor, kind, detail)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Atomically count one issue-level failure and converge immediately:
    /// below the cap the issue returns to `new` with a cooldown; at the cap it
    /// becomes `ignored`. Provider/environment failures never call this.
    #[allow(clippy::too_many_arguments)]
    pub fn record_issue_failure(
        &self,
        fingerprint: &str,
        now_ms: i64,
        max_attempts: u64,
        cooldown_until_ms: i64,
        detail: &str,
        actor: &str,
    ) -> ShanResult<FailureOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (state, repo, attempts): (String, String, u64) = tx.query_row(
            "SELECT state, repo, attempts FROM issues WHERE fingerprint=?1",
            [fingerprint],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let from = State::parse(&state).ok_or_else(|| ShanError::UnknownState(state.clone()))?;
        if !matches!(from, State::Triggered | State::Fixing) {
            return Err(ShanError::InvalidTransition {
                from,
                to: State::New,
            });
        }
        let attempts = attempts + 1;
        let (to, kind, cooldown) = if attempts >= max_attempts {
            (State::Ignored, "ignored", 0)
        } else {
            (State::New, "state_changed", cooldown_until_ms)
        };
        let changed = tx.execute(
            "UPDATE issues SET state=?2, attempts=?3, cooldown_until=?4,
                owner_runner_id=NULL, state_changed_at=?5
             WHERE fingerprint=?1 AND state=?6",
            rusqlite::params![fingerprint, to.as_str(), attempts, cooldown, now_ms, state],
        )?;
        if changed == 0 {
            return Err(ShanError::InvalidTransition { from, to });
        }
        record_event_on(&tx, fingerprint, now_ms, Some(&repo), actor, kind, detail)?;
        tx.commit()?;
        Ok(FailureOutcome {
            disposition: if to == State::Ignored {
                FailureDisposition::Ignored
            } else {
                FailureDisposition::Retry
            },
            state: to,
            attempts,
        })
    }

    /// CI red belongs to one delivered PR. Close that delivery's issue (and
    /// count the attempt) atomically, then either schedule a retry or ignore at
    /// the cap. A stale PR URL is a no-op, never a reason to reject a new cycle.
    #[allow(clippy::too_many_arguments)]
    pub fn record_ci_rejection(
        &self,
        fingerprint: &str,
        expected_pr_url: &str,
        now_ms: i64,
        max_attempts: u64,
        cooldown_until_ms: i64,
        detail: &str,
        actor: &str,
    ) -> ShanResult<FailureOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (repo, attempts): (String, u64) = tx.query_row(
            "SELECT repo, attempts FROM issues
             WHERE fingerprint=?1 AND state='pr_open' AND pr_url=?2",
            rusqlite::params![fingerprint, expected_pr_url],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        record_event_on(
            &tx,
            fingerprint,
            now_ms,
            Some(&repo),
            actor,
            "rejected",
            detail,
        )?;
        let attempts = attempts + 1;
        let (to, kind, cooldown) = if attempts >= max_attempts {
            (State::Ignored, "ignored", 0)
        } else {
            (State::New, "state_changed", cooldown_until_ms)
        };
        tx.execute(
            "UPDATE issues SET state=?2, attempts=?3, cooldown_until=?4,
                owner_runner_id=NULL, state_changed_at=?5, pr_url=NULL, pr_opened_at=NULL
             WHERE fingerprint=?1 AND state='pr_open' AND pr_url=?6",
            rusqlite::params![
                fingerprint,
                to.as_str(),
                attempts,
                cooldown,
                now_ms,
                expected_pr_url
            ],
        )?;
        record_event_on(&tx, fingerprint, now_ms, Some(&repo), actor, kind, detail)?;
        tx.commit()?;
        Ok(FailureOutcome {
            disposition: if to == State::Ignored {
                FailureDisposition::Ignored
            } else {
                FailureDisposition::Retry
            },
            state: to,
            attempts,
        })
    }

    /// Record a delivery intent before any network side effect. Repeated calls
    /// for the same (repo, branch, head_sha) return the existing record, so a
    /// crash cannot create a second delivery identity.
    pub fn begin_delivery(&self, start: &DeliveryStart) -> ShanResult<DeliveryRow> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO deliveries
                (id, run_id, fingerprint, repo, branch, base, head_sha, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'intent', ?8, ?8)
             ON CONFLICT(repo, branch, head_sha) DO UPDATE SET
                updated_at=excluded.updated_at,
                run_id=COALESCE(deliveries.run_id, excluded.run_id)",
            rusqlite::params![
                start.id,
                start.run_id,
                start.fingerprint,
                start.repo,
                start.branch,
                start.base,
                start.head_sha,
                start.now,
            ],
        )?;
        let row = conn.query_row(
            "SELECT id, run_id, fingerprint, repo, branch, base, head_sha, state,
                    pr_url, pr_number, detail, created_at, updated_at
             FROM deliveries WHERE repo=?1 AND branch=?2 AND head_sha=?3",
            rusqlite::params![start.repo, start.branch, start.head_sha],
            delivery_from_row,
        )?;
        Ok(row)
    }

    pub fn update_delivery_head(&self, id: &str, head_sha: &str, now_ms: i64) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE deliveries SET head_sha=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![id, head_sha, now_ms],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownIssue(id.to_string()));
        }
        Ok(())
    }

    pub fn mark_delivery(
        &self,
        id: &str,
        state: DeliveryState,
        pr_url: Option<&str>,
        pr_number: Option<u64>,
        detail: Option<&str>,
        now_ms: i64,
    ) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE deliveries SET state=?2,
                pr_url=COALESCE(?3, pr_url), pr_number=COALESCE(?4, pr_number),
                detail=COALESCE(?5, detail), updated_at=?6
             WHERE id=?1",
            rusqlite::params![id, state.as_str(), pr_url, pr_number, detail, now_ms],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownIssue(id.to_string()));
        }
        Ok(())
    }

    /// Unfinished deliveries for one repo, oldest first. The runner uses this
    /// at startup to adopt a PR that GitHub created before the process died.
    pub fn pending_deliveries(&self, repo: &str) -> ShanResult<Vec<DeliveryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, run_id, fingerprint, repo, branch, base, head_sha, state,
                    pr_url, pr_number, detail, created_at, updated_at
             FROM deliveries
             WHERE repo=?1 AND state IN ('intent','pushed','pr_requested','ambiguous')
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([repo], delivery_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Recovery path: a PR was found for an unfinished delivery. Move the
    /// issue to `pr_open` without requiring the normal fixing→pr_open sequence,
    /// because the process may have died before the fixing transition landed.
    pub fn adopt_delivered_pr(
        &self,
        fingerprint: &str,
        pr_url: &str,
        now_ms: i64,
    ) -> ShanResult<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some((state, repo)): Option<(String, String)> = tx
            .query_row(
                "SELECT state, repo FROM issues WHERE fingerprint=?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        else {
            return Err(ShanError::UnknownIssue(fingerprint.to_string()));
        };
        let state = State::parse(&state).ok_or(ShanError::UnknownState(state))?;
        if matches!(
            state,
            State::Merged | State::Rejected | State::Escalated | State::Ignored
        ) {
            return Ok(false);
        }
        tx.execute(
            "UPDATE issues SET state='pr_open', pr_url=?2, pr_opened_at=?3,
                state_changed_at=?3, owner_runner_id=NULL
             WHERE fingerprint=?1",
            rusqlite::params![fingerprint, pr_url, now_ms],
        )?;
        record_event_on(
            &tx,
            fingerprint,
            now_ms,
            Some(&repo),
            "runner",
            "state_changed",
            "delivery recovered: PR adopted from GitHub",
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn upsert_ci_watch(&self, input: &CiWatchInput) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO ci_watches
                (run_id, fingerprint, repo, pr_url, slug, pr_number, head_sha,
                 state, next_check_at, created_at, updated_at, root_cause, change_stat,
                 max_attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?9, ?10, ?11, ?12)
             ON CONFLICT(repo, pr_url, head_sha) DO UPDATE SET
                updated_at=excluded.updated_at,
                next_check_at=MIN(ci_watches.next_check_at, excluded.next_check_at),
                run_id=COALESCE(ci_watches.run_id, excluded.run_id),
                root_cause=COALESCE(ci_watches.root_cause, excluded.root_cause),
                change_stat=COALESCE(ci_watches.change_stat, excluded.change_stat)",
            rusqlite::params![
                input.run_id,
                input.fingerprint,
                input.repo,
                input.pr_url,
                input.slug,
                input.pr_number,
                input.head_sha,
                input.next_check_at,
                input.now,
                input.root_cause,
                input.change_stat,
                input.max_attempts,
            ],
        )?;
        Ok(())
    }

    /// Atomically claim due watches by pushing their next check out. A second
    /// serve process cannot poll the same watch concurrently.
    pub fn claim_due_ci_watches(
        &self,
        now_ms: i64,
        limit: u32,
        lease_ms: i64,
    ) -> ShanResult<Vec<CiWatchRow>> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM ci_watches
                 WHERE state IN ('pending','green') AND next_check_at <= ?1
                 ORDER BY next_check_at ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![now_ms, limit], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            tx.execute(
                "UPDATE ci_watches SET next_check_at=?2, updated_at=?1 WHERE id=?3",
                rusqlite::params![now_ms, now_ms + lease_ms, id],
            )?;
            let row = tx.query_row(
                "SELECT id, run_id, fingerprint, repo, pr_url, slug, pr_number, head_sha,
                        state, last_verdict, last_checked_at, next_check_at, check_attempts,
                        created_at, updated_at, terminal_at, last_error, root_cause,
                        change_stat, memory_written, max_attempts
                 FROM ci_watches WHERE id=?1",
                [id],
                ci_watch_from_row,
            )?;
            out.push(row);
        }
        tx.commit()?;
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_ci_watch(
        &self,
        id: i64,
        state: CiWatchState,
        last_verdict: Option<&str>,
        next_check_at: i64,
        last_error: Option<&str>,
        memory_written: bool,
        terminal_at: Option<i64>,
        now_ms: i64,
    ) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE ci_watches SET state=?2, last_verdict=COALESCE(?3, last_verdict),
                last_checked_at=?8, next_check_at=?4, last_error=?5,
                memory_written=?6, terminal_at=COALESCE(?7, terminal_at), updated_at=?8,
                check_attempts=0
             WHERE id=?1",
            rusqlite::params![
                id,
                state.as_str(),
                last_verdict,
                next_check_at,
                last_error,
                memory_written,
                terminal_at,
                now_ms
            ],
        )?;
        Ok(())
    }

    pub fn record_ci_watch_error(
        &self,
        id: i64,
        state: CiWatchState,
        next_check_at: i64,
        last_error: &str,
        now_ms: i64,
    ) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE ci_watches SET state=?2, next_check_at=?3, last_error=?4,
                last_checked_at=?5, updated_at=?5, check_attempts=check_attempts+1
             WHERE id=?1",
            rusqlite::params![id, state.as_str(), next_check_at, last_error, now_ms],
        )?;
        Ok(())
    }

    pub fn ci_watch_for(&self, fingerprint: &str) -> ShanResult<Option<CiWatchRow>> {
        let conn = self.lock();
        conn.query_row(
            "SELECT id, run_id, fingerprint, repo, pr_url, slug, pr_number, head_sha,
                    state, last_verdict, last_checked_at, next_check_at, check_attempts,
                    created_at, updated_at, terminal_at, last_error, root_cause,
                    change_stat, memory_written, max_attempts
             FROM ci_watches WHERE fingerprint=?1
             ORDER BY created_at DESC, id DESC LIMIT 1",
            [fingerprint],
            ci_watch_from_row,
        )
        .optional()
        .map_err(ShanError::from)
    }

    /// Increment the issue-level attempt counter and return the new value.
    /// Provider outages and environment faults intentionally do not call this.
    pub fn increment_attempts(&self, fingerprint: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE issues SET attempts = attempts + 1 WHERE fingerprint = ?1",
            [fingerprint],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownIssue(fingerprint.to_string()));
        }
        let attempts = conn.query_row(
            "SELECT attempts FROM issues WHERE fingerprint = ?1",
            [fingerprint],
            |row| row.get(0),
        )?;
        Ok(attempts)
    }

    pub fn attempts_for(&self, fingerprint: &str) -> ShanResult<u64> {
        let conn = self.lock();
        conn.query_row(
            "SELECT attempts FROM issues WHERE fingerprint = ?1",
            [fingerprint],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ShanError::UnknownIssue(fingerprint.to_string())
            }
            other => ShanError::Db(other),
        })
    }

    /// Validate + apply a state transition, journaling the event as `system`.
    pub fn set_state(
        &self,
        fingerprint: &str,
        to: State,
        now_ms: i64,
        detail: Option<&str>,
    ) -> ShanResult<()> {
        self.set_state_as(fingerprint, to, now_ms, detail, "system")
    }

    /// [`Store::set_state`], attributing the journaled event to `actor`.
    pub fn set_state_as(
        &self,
        fingerprint: &str,
        to: State,
        now_ms: i64,
        detail: Option<&str>,
        actor: &str,
    ) -> ShanResult<()> {
        let conn = self.lock();
        let (from, repo): (String, String) = conn
            .query_row(
                "SELECT state, repo FROM issues WHERE fingerprint = ?1",
                [fingerprint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ShanError::UnknownIssue(fingerprint.to_string())
                }
                other => ShanError::Db(other),
            })?;
        let from = State::parse(&from).ok_or_else(|| ShanError::UnknownState(from))?;
        if !can_transition(from, to) {
            return Err(ShanError::InvalidTransition { from, to });
        }
        // Compare-and-swap: the UPDATE only lands if the state is still the one
        // we validated. Two processes (a runner and the owner, or two runners)
        // can both SELECT the same `from` before either writes; without the
        // `AND state = ?4` guard the transition check is merely advisory and
        // both writes succeed, pushing the issue along an edge that
        // `can_transition` would have rejected had either seen the other's
        // change. SQLite serializes the two writes, so the loser matches 0 rows.
        let changed = conn.execute(
            "UPDATE issues SET state = ?2, state_changed_at = ?3 WHERE fingerprint = ?1 AND state = ?4",
            rusqlite::params![fingerprint, to.as_str(), now_ms, from.as_str()],
        )?;
        if changed == 0 {
            // The state moved under us between the SELECT and the write. Report
            // the actual current state so the caller sees the real conflict.
            let current: String = conn
                .query_row(
                    "SELECT state FROM issues WHERE fingerprint = ?1",
                    [fingerprint],
                    |row| row.get(0),
                )
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        ShanError::UnknownIssue(fingerprint.to_string())
                    }
                    other => ShanError::Db(other),
                })?;
            let current = State::parse(&current).ok_or_else(|| ShanError::UnknownState(current))?;
            return Err(ShanError::InvalidTransition { from: current, to });
        }
        if to == State::New {
            conn.execute(
                "UPDATE issues SET owner_runner_id = NULL WHERE fingerprint = ?1",
                [fingerprint],
            )?;
        }
        drop(conn);
        self.record_event(
            fingerprint,
            now_ms,
            Some(&repo),
            actor,
            "state_changed",
            detail.unwrap_or(""),
        )
    }

    /// Put an issue into cooldown (used when a run starts: same fingerprint
    /// must not double-trigger while a fix is in flight).
    pub fn set_cooldown(&self, fingerprint: &str, until_ms: i64) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE issues SET cooldown_until = ?2 WHERE fingerprint = ?1",
            rusqlite::params![fingerprint, until_ms],
        )?;
        drop(conn);
        if changed == 0 {
            return Err(ShanError::UnknownIssue(fingerprint.to_string()));
        }
        Ok(())
    }

    /// Record the delivered PR on the issue (delivery step calls this right
    /// after the API returns the URL).
    pub fn set_pr(&self, fingerprint: &str, pr_url: &str, now_ms: i64) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE issues SET pr_url = ?2, pr_opened_at = ?3 WHERE fingerprint = ?1",
            rusqlite::params![fingerprint, pr_url, now_ms],
        )?;
        drop(conn);
        if changed == 0 {
            return Err(ShanError::UnknownIssue(fingerprint.to_string()));
        }
        let repo: String = {
            let conn = self.lock();
            conn.query_row(
                "SELECT repo FROM issues WHERE fingerprint = ?1",
                [fingerprint],
                |row| row.get(0),
            )?
        };
        self.record_event(
            fingerprint,
            now_ms,
            Some(&repo),
            "runner",
            "pr_opened",
            pr_url,
        )
    }

    /// PRs opened for `repo` since `since_ms` — the counter behind the
    /// per-repo daily PR limit (ADR-003 频控). Counts distinct PR URLs across
    /// the run ledger and legacy issue rows so retries of the same PR never
    /// consume quota twice.
    /// Tokens spent by runs for one repo (or all repos) since `since_ms`.
    /// Failed attempts count: this is a spend cap, not a PR quota.
    pub fn tokens_used_since(&self, repo: Option<&str>, since_ms: i64) -> ShanResult<u64> {
        let conn = self.lock();
        let total: i64 = match repo {
            Some(repo) => conn.query_row(
                "SELECT COALESCE(SUM(tokens_uncached + tokens_cached), 0)
                 FROM runs WHERE repo = ?1 AND started_at >= ?2",
                rusqlite::params![repo, since_ms],
                |row| row.get(0),
            )?,
            None => conn.query_row(
                "SELECT COALESCE(SUM(tokens_uncached + tokens_cached), 0)
                 FROM runs WHERE started_at >= ?1",
                [since_ms],
                |row| row.get(0),
            )?,
        };
        Ok(total.max(0) as u64)
    }

    /// Number of merged issues for a repo. Used by `draft_first_n` to keep the
    /// first PRs in a new repository as drafts without changing ADR-003's
    /// default for established repos.
    pub fn merged_pr_count(&self, repo: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE repo = ?1 AND state = 'merged'",
            [repo],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    pub fn prs_opened_since(&self, repo: &str, since_ms: i64) -> ShanResult<u64> {
        let conn = self.lock();
        let count: u64 = conn.query_row(
            "SELECT COUNT(DISTINCT pr_url) FROM (
                SELECT pr_url FROM runs
                WHERE repo = ?1 AND pr_opened_at IS NOT NULL AND pr_opened_at >= ?2
                UNION ALL
                SELECT pr_url FROM issues
                WHERE repo = ?1 AND pr_opened_at IS NOT NULL AND pr_opened_at >= ?2
            )",
            rusqlite::params![repo, since_ms],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Delete telemetry past its retention window and return the total number
    /// of removed rows. Normal events roll off after `event_days`, audit
    /// events (`config_changed` / `config_migrated` / `restart_requested`)
    /// after `audit_days` (callers pass `event_days.max(365)` so audit history
    /// is never shorter than normal telemetry), runs after `run_days`, and
    /// runner rows whose last heartbeat is older than 30 days. Callers
    /// schedule this (M1b/M2).
    pub fn prune(
        &self,
        now_ms: i64,
        event_days: u32,
        audit_days: u32,
        run_days: u32,
    ) -> ShanResult<u64> {
        let event_cutoff = now_ms - i64::from(event_days) * 86_400_000;
        let audit_cutoff = now_ms - i64::from(audit_days) * 86_400_000;
        let run_cutoff = now_ms - i64::from(run_days) * 86_400_000;
        let runner_cutoff = now_ms - 30 * 86_400_000;
        let conn = self.lock();
        let mut deleted = 0u64;
        deleted += conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND kind NOT IN
                ('config_changed','config_migrated','restart_requested')",
            [event_cutoff],
        )? as u64;
        deleted += conn.execute(
            "DELETE FROM events WHERE ts < ?1 AND kind IN
                ('config_changed','config_migrated','restart_requested')",
            [audit_cutoff],
        )? as u64;
        deleted += conn.execute("DELETE FROM runs WHERE started_at < ?1", [run_cutoff])? as u64;
        deleted += conn.execute(
            "DELETE FROM runners WHERE heartbeat_at < ?1",
            [runner_cutoff],
        )? as u64;
        Ok(deleted)
    }

    /// Reports for `fingerprint` on UTC `day` (`YYYY-MM-DD`), summed across
    /// repos — the per-issue daily report count shown by the console.
    pub fn signal_reports_on(&self, fingerprint: &str, day: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM signal_daily WHERE fingerprint=?1 AND day=?2",
            rusqlite::params![fingerprint, day],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Total reports across all fingerprints and repos on UTC `day`.
    pub fn reports_on(&self, day: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM signal_daily WHERE day = ?1",
            [day],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Total reports on UTC `day`, optionally scoped to one repo — the
    /// console overview's repo-aware `reports_today`.
    pub fn reports_on_repo(&self, day: &str, repo: Option<&str>) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM signal_daily
             WHERE day = ?1 AND (?2 IS NULL OR repo = ?2)",
            rusqlite::params![day, repo],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Daily aggregates for the console trends chart. Returns exactly `days`
    /// UTC day buckets ending on the day containing `now_ms`, oldest first;
    /// days without data are zero-filled. `repo` filters every source.
    pub fn trends(
        &self,
        days: u32,
        repo: Option<&str>,
        now_ms: i64,
    ) -> ShanResult<Vec<TrendBucket>> {
        if days == 0 {
            return Ok(Vec::new());
        }
        let span = i64::from(days);
        let today_start = now_ms - now_ms.rem_euclid(DAY_MS);
        let window_start = today_start - (span - 1) * DAY_MS;
        let window_end = today_start + DAY_MS;

        let mut buckets: BTreeMap<String, TrendBucket> = BTreeMap::new();
        for offset in (0..span).rev() {
            let day = utc_day(today_start - offset * DAY_MS);
            buckets.insert(
                day.clone(),
                TrendBucket {
                    day,
                    ..TrendBucket::default()
                },
            );
        }
        let first_day = buckets.keys().next().cloned().unwrap_or_default();
        let last_day = buckets.keys().next_back().cloned().unwrap_or_default();

        let conn = self.lock();

        // Reports are already aggregated per day and repo by report_signal.
        {
            let mut stmt = conn.prepare(
                "SELECT day, COALESCE(SUM(count), 0) FROM signal_daily
                 WHERE day >= :from_day AND day <= :to_day
                   AND (:repo IS NULL OR repo = :repo)
                 GROUP BY day",
            )?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":from_day": &first_day,
                    ":to_day": &last_day,
                    ":repo": repo,
                },
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )?;
            for row in rows {
                let (day, count) = row?;
                if let Some(bucket) = buckets.get_mut(&day) {
                    bucket.reports = count.max(0) as u64;
                }
            }
        }

        // New issues: bucket first_seen in Rust (R29).
        {
            let mut stmt = conn.prepare(
                "SELECT first_seen FROM issues
                 WHERE first_seen >= :from AND first_seen < :to
                   AND (:repo IS NULL OR repo = :repo)",
            )?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":from": window_start,
                    ":to": window_end,
                    ":repo": repo,
                },
                |row| row.get::<_, i64>(0),
            )?;
            for ts in rows {
                if let Some(bucket) = buckets.get_mut(&utc_day(ts?)) {
                    bucket.new_issues += 1;
                }
            }
        }

        // PRs opened: the run ledger plus the legacy issue row for PRs opened
        // before runs recorded them (pre-migration data). A PR is counted once
        // per day/repo even when both sources carry it; rows without a URL
        // fall back to their own identity so they are never silently dropped.
        {
            let mut stmt = conn.prepare(
                "SELECT pr_opened_at, repo, COALESCE(pr_url, 'run:' || id)
                 FROM runs
                 WHERE pr_opened_at IS NOT NULL
                   AND pr_opened_at >= :from AND pr_opened_at < :to
                   AND (:repo IS NULL OR repo = :repo)
                 UNION ALL
                 SELECT pr_opened_at, repo, COALESCE(pr_url, 'issue:' || fingerprint)
                 FROM issues
                 WHERE pr_opened_at IS NOT NULL
                   AND pr_opened_at >= :from AND pr_opened_at < :to
                   AND (:repo IS NULL OR repo = :repo)",
            )?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":from": window_start,
                    ":to": window_end,
                    ":repo": repo,
                },
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            let mut prs: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
            for row in rows {
                let (ts, repo, identity) = row?;
                prs.entry(utc_day(ts)).or_default().insert((repo, identity));
            }
            for (day, identities) in prs {
                if let Some(bucket) = buckets.get_mut(&day) {
                    bucket.prs_opened = identities.len() as u64;
                }
            }
        }

        // Tokens: bucket by run start day.
        {
            let mut stmt = conn.prepare(
                "SELECT started_at,
                        COALESCE(tokens_uncached, 0) + COALESCE(tokens_cached, 0)
                 FROM runs
                 WHERE started_at >= :from AND started_at < :to
                   AND (:repo IS NULL OR repo = :repo)",
            )?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":from": window_start,
                    ":to": window_end,
                    ":repo": repo,
                },
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?;
            for row in rows {
                let (ts, tokens) = row?;
                if let Some(bucket) = buckets.get_mut(&utc_day(ts)) {
                    bucket.tokens += tokens.max(0) as u64;
                }
            }
        }

        // Lifecycle outcomes: legacy rows resolve their repo through the
        // issue join, same COALESCE semantics as list_events.
        {
            let mut stmt = conn.prepare(
                "SELECT e.kind, e.ts FROM events e
                 LEFT JOIN issues i ON i.fingerprint = e.fingerprint
                 WHERE e.kind IN ('merged', 'rejected', 'ci_green', 'ci_red')
                   AND e.ts >= :from AND e.ts < :to
                   AND (:repo IS NULL OR COALESCE(e.repo, i.repo, '') = :repo)",
            )?;
            let rows = stmt.query_map(
                rusqlite::named_params! {
                    ":from": window_start,
                    ":to": window_end,
                    ":repo": repo,
                },
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )?;
            for row in rows {
                let (kind, ts) = row?;
                let Some(bucket) = buckets.get_mut(&utc_day(ts)) else {
                    continue;
                };
                match kind.as_str() {
                    "merged" => bucket.merged += 1,
                    "rejected" => bucket.rejected += 1,
                    "ci_green" => bucket.ci_green += 1,
                    "ci_red" => bucket.ci_red += 1,
                    _ => {}
                }
            }
        }

        Ok(buckets.into_values().collect())
    }

    /// Events of the given kinds at or after `since_ms` — CI counters.
    /// `kinds` are bound parameters; the placeholder list is built from
    /// their count only, so no value ever reaches the SQL text.
    pub fn count_events_since(&self, kinds: &[&str], since_ms: i64) -> ShanResult<u64> {
        if kinds.is_empty() {
            return Ok(0);
        }
        let placeholders: Vec<String> = (2..=kinds.len() + 1)
            .map(|index| format!("?{index}"))
            .collect();
        let sql = format!(
            "SELECT COUNT(*) FROM events WHERE ts >= ?1 AND kind IN ({})",
            placeholders.join(", ")
        );
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(kinds.len() + 1);
        params.push(rusqlite::types::Value::Integer(since_ms));
        params.extend(
            kinds
                .iter()
                .map(|kind| rusqlite::types::Value::Text((*kind).to_string())),
        );
        let conn = self.lock();
        let count: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Events of the given kinds at or after `since_ms`, optionally scoped to
    /// one repo — the console overview's repo-aware 24h CI counters. Legacy
    /// rows with a NULL `events.repo` fall back to the issue's repo.
    pub fn count_events_since_repo(
        &self,
        kinds: &[&str],
        since_ms: i64,
        repo: Option<&str>,
    ) -> ShanResult<u64> {
        if kinds.is_empty() {
            return Ok(0);
        }
        // Only the placeholder list is interpolated; kind values stay bound.
        let placeholders: Vec<String> = (2..=kinds.len() + 1)
            .map(|index| format!("?{index}"))
            .collect();
        let mut sql = format!(
            "SELECT COUNT(*) FROM events e
             LEFT JOIN issues i ON i.fingerprint = e.fingerprint
             WHERE e.ts >= ?1 AND e.kind IN ({})",
            placeholders.join(", ")
        );
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(kinds.len() + 2);
        params.push(rusqlite::types::Value::Integer(since_ms));
        params.extend(
            kinds
                .iter()
                .map(|kind| rusqlite::types::Value::Text((*kind).to_string())),
        );
        if let Some(repo) = repo {
            sql.push_str(" AND COALESCE(e.repo, i.repo, '') = ?");
            params.push(rusqlite::types::Value::Text(repo.to_string()));
        }
        let conn = self.lock();
        let count: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn get(&self, fingerprint: &str) -> ShanResult<Option<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, title, language, repo, severity, occurrences, attempts, stack_sample, log_sample, first_seen, last_seen
             FROM issues WHERE fingerprint = ?1",
        )?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::snapshot_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// All issues, newest-sighting first — the `nuwa status` view.
    pub fn list_issues(&self) -> ShanResult<Vec<IssueRow>> {
        self.issue_rows(
            &format!("{ISSUE_VIEW_SELECT} ORDER BY i.last_seen DESC"),
            [],
        )
    }

    /// Issues sharing `title`, newest first, excluding `exclude_fp` itself —
    /// the detail page's "similar problems" strip.
    pub fn similar_issues(
        &self,
        title: &str,
        exclude_fp: &str,
        limit: u32,
    ) -> ShanResult<Vec<IssueRow>> {
        self.issue_rows(
            &format!(
                "{ISSUE_VIEW_SELECT}
                 WHERE i.title = ?1 AND i.fingerprint <> ?2
                 ORDER BY i.last_seen DESC LIMIT ?3"
            ),
            rusqlite::params![title, exclude_fp, limit],
        )
    }

    /// Total issues sharing `title`, excluding `exclude_fp` — the detail
    /// page's uncapped "similar problems" count (the strip itself is limited).
    pub fn similar_count(&self, title: &str, exclude_fp: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE title = ?1 AND fingerprint <> ?2",
            rusqlite::params![title, exclude_fp],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    fn issue_rows(&self, sql: &str, params: impl rusqlite::Params) -> ShanResult<Vec<IssueRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, Self::issue_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn issue_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueRow> {
        Ok(IssueRow {
            fingerprint: row.get(0)?,
            title: row.get(1)?,
            language: row.get(2)?,
            repo: row.get(3)?,
            severity: row.get(4)?,
            state: row.get(5)?,
            occurrences: row.get(6)?,
            pr_url: row.get(7)?,
            first_seen: row.get(8)?,
            last_seen: row.get(9)?,
            state_changed_at: row.get(10)?,
            owner_runner_id: row.get(11)?,
            ci_verdict: row.get(12)?,
            attempts: row.get(13)?,
        })
    }

    /// Recent lifecycle events, newest first — the console activity feed.
    pub fn recent_events(&self, limit: u32) -> ShanResult<Vec<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("{EVENT_SELECT} ORDER BY e.id DESC LIMIT ?1"))?;
        let rows = stmt.query_map([limit], Self::event_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Per-issue lifecycle events, newest first — the detail page timeline.
    pub fn events_for(&self, fingerprint: &str, limit: u32) -> ShanResult<Vec<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{EVENT_SELECT} WHERE e.fingerprint = ?1 ORDER BY e.id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![fingerprint, limit], Self::event_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Newest event for one issue — detail header / attention reason.
    pub fn latest_event(&self, fingerprint: &str) -> ShanResult<Option<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{EVENT_SELECT} WHERE e.fingerprint = ?1 ORDER BY e.ts DESC, e.id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::event_from_row(row)?)),
            None => Ok(None),
        }
    }

    fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
        Ok(EventRow {
            fingerprint: row.get(0)?,
            ts: row.get(1)?,
            kind: row.get(2)?,
            detail: row.get(3)?,
            repo: row.get(4)?,
            actor: row.get(5)?,
        })
    }

    /// 记录一条已验证的修复。同一 (repo, fingerprint) 只保留最新一次，
    /// 每仓库最多 [`REPAIR_MEMORY_KEEP`] 条——记忆是参考，不是账本。
    pub fn record_repair_memory(&self, entry: &RepairMemoryEntry) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO repair_memory
                (repo, fingerprint, commit_sha, pr_url, root_cause, change_stat, files, recorded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(repo, fingerprint) DO UPDATE SET
                commit_sha=excluded.commit_sha,
                pr_url=excluded.pr_url,
                root_cause=excluded.root_cause,
                change_stat=excluded.change_stat,
                files=excluded.files,
                recorded_at=excluded.recorded_at",
            rusqlite::params![
                entry.repo,
                entry.fingerprint,
                entry.commit_sha,
                entry.pr_url,
                entry.root_cause,
                entry.change_stat,
                entry.files,
                entry.recorded_at,
            ],
        )?;
        conn.execute(
            "DELETE FROM repair_memory WHERE repo = ?1 AND fingerprint NOT IN (
                SELECT fingerprint FROM repair_memory WHERE repo = ?1
                ORDER BY recorded_at DESC LIMIT ?2
             )",
            rusqlite::params![entry.repo, REPAIR_MEMORY_KEEP as i64],
        )?;
        Ok(())
    }

    /// 同仓库其他问题单的已验证修复，最新在前。
    pub fn recent_repair_memory(
        &self,
        repo: &str,
        exclude_fingerprint: &str,
        limit: u32,
    ) -> ShanResult<Vec<RepairMemoryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, commit_sha, pr_url, root_cause, change_stat, files, recorded_at
             FROM repair_memory
             WHERE repo = ?1 AND fingerprint != ?2 AND disabled = 0
             ORDER BY recorded_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo, exclude_fingerprint, limit], |row| {
            Ok(RepairMemoryRow {
                fingerprint: row.get(0)?,
                commit_sha: row.get(1)?,
                pr_url: row.get(2)?,
                root_cause: row.get(3)?,
                change_stat: row.get(4)?,
                files: row.get(5)?,
                recorded_at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// 控制台记忆列表：来源、问题单标题/状态、停用标记，最新在前。
    pub fn list_repair_memory(
        &self,
        repo: &str,
        limit: u32,
        offset: u32,
    ) -> ShanResult<(Vec<RepairMemoryView>, u64)> {
        let conn = self.lock();
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM repair_memory WHERE repo = ?1",
            [repo],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(
            "SELECT m.fingerprint, COALESCE(i.title, ''), COALESCE(i.state, ''),
                    m.commit_sha, m.pr_url, m.root_cause, m.change_stat, m.files,
                    m.recorded_at, m.disabled
             FROM repair_memory m
             LEFT JOIN issues i ON i.fingerprint = m.fingerprint
             WHERE m.repo = ?1
             ORDER BY m.recorded_at DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![repo, limit, offset], |row| {
            Ok(RepairMemoryView {
                fingerprint: row.get(0)?,
                title: row.get(1)?,
                state: row.get(2)?,
                commit_sha: row.get(3)?,
                pr_url: row.get(4)?,
                root_cause: row.get(5)?,
                change_stat: row.get(6)?,
                files: row.get(7)?,
                recorded_at: row.get(8)?,
                disabled: row.get::<_, i64>(9)? != 0,
            })
        })?;
        Ok((rows.collect::<Result<_, _>>()?, total as u64))
    }

    /// 停用/启用一条记忆。返回 false 表示该记忆不存在。
    pub fn set_repair_memory_disabled(
        &self,
        repo: &str,
        fingerprint: &str,
        disabled: bool,
    ) -> ShanResult<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE repair_memory SET disabled = ?3 WHERE repo = ?1 AND fingerprint = ?2",
            rusqlite::params![repo, fingerprint, disabled as i64],
        )?;
        Ok(changed > 0)
    }

    /// 删除一条记忆。返回 false 表示该记忆不存在。
    pub fn delete_repair_memory(&self, repo: &str, fingerprint: &str) -> ShanResult<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "DELETE FROM repair_memory WHERE repo = ?1 AND fingerprint = ?2",
            rusqlite::params![repo, fingerprint],
        )?;
        Ok(changed > 0)
    }

    // ---- repair benchmark ----

    pub fn start_benchmark_run(&self, start: &BenchmarkStart) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO benchmark_runs
                (id, started_at, provider, model, mode, status, scenarios)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
            rusqlite::params![
                start.id,
                start.started_at,
                start.provider,
                start.model,
                start.mode,
                start.scenarios as i64,
            ],
        )?;
        Ok(())
    }

    pub fn insert_benchmark_result(&self, result: &BenchmarkResultInput) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO benchmark_results
                (run_id, scenario, language, expected_fixed, repaired, claimed_fixed,
                 delivery_ok, turns, tokens_uncached, tokens_cached, duration_ms,
                 root_cause, final_report, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(run_id, scenario) DO UPDATE SET
                language=excluded.language,
                expected_fixed=excluded.expected_fixed,
                repaired=excluded.repaired,
                claimed_fixed=excluded.claimed_fixed,
                delivery_ok=excluded.delivery_ok,
                turns=excluded.turns,
                tokens_uncached=excluded.tokens_uncached,
                tokens_cached=excluded.tokens_cached,
                duration_ms=excluded.duration_ms,
                root_cause=excluded.root_cause,
                final_report=excluded.final_report,
                error=excluded.error",
            rusqlite::params![
                result.run_id,
                result.scenario,
                result.language,
                result.expected_fixed as i64,
                result.repaired as i64,
                result.claimed_fixed as i64,
                result.delivery_ok as i64,
                result.turns,
                result.tokens_uncached,
                result.tokens_cached,
                result.duration_ms,
                result.root_cause,
                result.final_report,
                result.error,
            ],
        )?;
        Ok(())
    }

    pub fn finish_benchmark_run(&self, id: &str, finish: &BenchmarkFinish) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE benchmark_runs SET
                finished_at = ?2,
                status = ?3,
                repaired = ?4,
                claimed = ?5,
                false_positive = ?6,
                delivery_ok = ?7,
                turns = ?8,
                tokens_uncached = ?9,
                tokens_cached = ?10,
                duration_ms = ?11,
                error = ?12
             WHERE id = ?1",
            rusqlite::params![
                id,
                finish.finished_at,
                finish.status,
                finish.repaired,
                finish.claimed,
                finish.false_positive,
                finish.delivery_ok,
                finish.turns,
                finish.tokens_uncached,
                finish.tokens_cached,
                finish.duration_ms,
                finish.error,
            ],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRun(id.to_string()));
        }
        Ok(())
    }

    pub fn list_benchmark_runs(&self, limit: u32) -> ShanResult<Vec<BenchmarkRunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, started_at, finished_at, provider, model, mode, status,
                    scenarios, repaired, claimed, false_positive, delivery_ok,
                    turns, tokens_uncached, tokens_cached, duration_ms, error
             FROM benchmark_runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], Self::benchmark_run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_benchmark_run(&self, id: &str) -> ShanResult<Option<BenchmarkRunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, started_at, finished_at, provider, model, mode, status,
                    scenarios, repaired, claimed, false_positive, delivery_ok,
                    turns, tokens_uncached, tokens_cached, duration_ms, error
             FROM benchmark_runs WHERE id = ?1",
        )?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::benchmark_run_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn benchmark_results(&self, run_id: &str) -> ShanResult<Vec<BenchmarkResultRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT run_id, scenario, language, expected_fixed, repaired, claimed_fixed,
                    delivery_ok, turns, tokens_uncached, tokens_cached, duration_ms,
                    root_cause, final_report, error
             FROM benchmark_results WHERE run_id = ?1 ORDER BY scenario",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok(BenchmarkResultRow {
                run_id: row.get(0)?,
                scenario: row.get(1)?,
                language: row.get(2)?,
                expected_fixed: row.get::<_, i64>(3)? != 0,
                repaired: row.get::<_, i64>(4)? != 0,
                claimed_fixed: row.get::<_, i64>(5)? != 0,
                delivery_ok: row.get::<_, i64>(6)? != 0,
                turns: row.get(7)?,
                tokens_uncached: row.get(8)?,
                tokens_cached: row.get(9)?,
                duration_ms: row.get(10)?,
                root_cause: row.get(11)?,
                final_report: row.get(12)?,
                error: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn benchmark_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BenchmarkRunRow> {
        Ok(BenchmarkRunRow {
            id: row.get(0)?,
            started_at: row.get(1)?,
            finished_at: row.get(2)?,
            provider: row.get(3)?,
            model: row.get(4)?,
            mode: row.get(5)?,
            status: row.get(6)?,
            scenarios: row.get(7)?,
            repaired: row.get(8)?,
            claimed: row.get(9)?,
            false_positive: row.get(10)?,
            delivery_ok: row.get(11)?,
            turns: row.get(12)?,
            tokens_uncached: row.get(13)?,
            tokens_cached: row.get(14)?,
            duration_ms: row.get(15)?,
            error: row.get(16)?,
        })
    }

    /// Filtered lifecycle events, newest first (`ts DESC, id DESC`) — the
    /// console event feed. `limit` is capped at 5000.
    pub fn list_events(&self, query: &EventQuery, limit: u32) -> ShanResult<Vec<EventRow>> {
        let mut conditions: Vec<String> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if !query.kinds.is_empty() {
            // Only the placeholder list is interpolated; kind values stay bound.
            let placeholders = vec!["?"; query.kinds.len()].join(", ");
            conditions.push(format!("e.kind IN ({placeholders})"));
            params.extend(
                query
                    .kinds
                    .iter()
                    .cloned()
                    .map(rusqlite::types::Value::Text),
            );
        }
        if let Some(repo) = &query.repo {
            conditions.push("COALESCE(e.repo, i.repo, '') = ?".to_string());
            params.push(rusqlite::types::Value::Text(repo.clone()));
        }
        if let Some(fingerprint) = &query.fingerprint {
            conditions.push("e.fingerprint = ?".to_string());
            params.push(rusqlite::types::Value::Text(fingerprint.clone()));
        }
        if let Some(from_ms) = query.from_ms {
            conditions.push("e.ts >= ?".to_string());
            params.push(rusqlite::types::Value::Integer(from_ms));
        }
        if let Some(to_ms) = query.to_ms {
            conditions.push("e.ts <= ?".to_string());
            params.push(rusqlite::types::Value::Integer(to_ms));
        }
        let mut sql = String::from(EVENT_SELECT);
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY e.ts DESC, e.id DESC LIMIT ?");
        params.push(rusqlite::types::Value::Integer(i64::from(
            limit.min(EVENT_LIST_CAP),
        )));

        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), Self::event_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runner liveness heartbeat — work loop upserts each iteration; the
    /// console shows "N 秒前" so a dead loop is visible within one interval.
    pub fn heartbeat(&self, now_ms: i64) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO heartbeat(id, ts) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET ts = excluded.ts",
            [now_ms],
        )?;
        Ok(())
    }

    pub fn last_heartbeat(&self) -> ShanResult<Option<i64>> {
        let conn = self.lock();
        let ts: Option<i64> = conn
            .query_row("SELECT ts FROM heartbeat WHERE id = 1", [], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(ts)
    }

    pub fn upsert_runner(&self, r: &RunnerUpsert) -> ShanResult<()> {
        let conn = self.lock();
        upsert_runner_on(&conn, r)
    }

    /// Take the single-process lease for `r.id` and register the runner in one
    /// transaction. False means a live process still holds the lease; a bare
    /// [`upsert_runner`] would clobber that process's identity, so contenders
    /// must not write anything before winning.
    pub fn try_acquire_runner_lease(
        &self,
        r: &RunnerUpsert,
        instance_id: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> ShanResult<RunnerLeaseOutcome> {
        let mut conn = self.lock();
        // IMMEDIATE takes the write lock before the lease check: two processes
        // starting together must serialize here, not both read "free" and race
        // the upgrade.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let free: Option<bool> = tx
            .query_row(
                "SELECT (lease_until <= ?2 OR instance_id = ?3) FROM runners WHERE id = ?1",
                rusqlite::params![r.id, now_ms, instance_id],
                |row| row.get(0),
            )
            .optional()?;
        if free == Some(false) {
            return Ok(RunnerLeaseOutcome::HeldByRunner);
        }
        // One working tree per live runner: two ids sharing a checkout would
        // both reset/fetch it and corrupt each other's repair.
        let checkout_holder: Option<String> = tx
            .query_row(
                "SELECT id FROM runners
                 WHERE id != ?1 AND repo_path = ?2 AND lease_until > ?3
                 LIMIT 1",
                rusqlite::params![r.id, r.repo_path, now_ms],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(holder) = checkout_holder {
            return Ok(RunnerLeaseOutcome::CheckoutInUse(holder));
        }
        upsert_runner_on(&tx, r)?;
        tx.execute(
            "UPDATE runners SET instance_id = ?2, lease_until = ?3 WHERE id = ?1",
            rusqlite::params![r.id, instance_id, now_ms.saturating_add(ttl_ms)],
        )?;
        tx.commit()?;
        Ok(RunnerLeaseOutcome::Acquired)
    }

    /// Extend the lease while this process is alive. False means another
    /// process took over (or the runner row is gone): stop working now — a
    /// second writer would race the same issue and branch.
    pub fn renew_runner_lease(
        &self,
        runner_id: &str,
        instance_id: &str,
        now_ms: i64,
        ttl_ms: i64,
    ) -> ShanResult<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runners SET lease_until = ?3 WHERE id = ?1 AND instance_id = ?2",
            rusqlite::params![runner_id, instance_id, now_ms.saturating_add(ttl_ms)],
        )?;
        Ok(changed == 1)
    }

    /// Drop the lease on a clean shutdown so a restart need not wait for the TTL.
    pub fn release_runner_lease(&self, runner_id: &str, instance_id: &str) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE runners SET lease_until = 0, instance_id = NULL
             WHERE id = ?1 AND instance_id = ?2",
            rusqlite::params![runner_id, instance_id],
        )?;
        Ok(())
    }

    pub fn heartbeat_runner(&self, id: &str, hb: &RunnerHeartbeat) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runners SET heartbeat_at=?2, status=?3, current_fingerprint=?4, phase=?5,
                config_json=COALESCE(?6, config_json), last_error=?7
             WHERE id=?1",
            rusqlite::params![
                id,
                hb.heartbeat_at,
                hb.status,
                hb.current_fingerprint,
                hb.phase,
                hb.config_json,
                hb.last_error,
            ],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRunner(id.to_string()));
        }
        Ok(())
    }

    /// Persist the last real progress timestamp separately from liveness.
    pub fn set_runner_progress(&self, id: &str, progress_at: i64) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runners SET progress_at=?2 WHERE id=?1",
            rusqlite::params![id, progress_at],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRunner(id.to_string()));
        }
        Ok(())
    }

    pub fn list_runners(&self) -> ShanResult<Vec<RunnerRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, repo, host, pid, started_at, heartbeat_at, status,
                    current_fingerprint, phase, provider, model, daily_pr_limit,
                    repo_path, service_unit, config_json, last_error, progress_at
             FROM runners ORDER BY heartbeat_at DESC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RunnerRow {
                id: row.get(0)?,
                repo: row.get(1)?,
                host: row.get(2)?,
                pid: row.get(3)?,
                started_at: row.get(4)?,
                heartbeat_at: row.get(5)?,
                progress_at: row.get(16)?,
                status: row.get(6)?,
                current_fingerprint: row.get(7)?,
                phase: row.get(8)?,
                provider: row.get(9)?,
                model: row.get(10)?,
                daily_pr_limit: row.get(11)?,
                repo_path: row.get(12)?,
                service_unit: row.get(13)?,
                config_json: row.get(14)?,
                last_error: row.get(15)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Registered repositories, ordered by display name (case-insensitive)
    /// then identity — the console registry list.
    pub fn list_repos(&self) -> ShanResult<Vec<RepoRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT repo, name, url, description, created_at, updated_at
             FROM repos ORDER BY name COLLATE NOCASE, repo",
        )?;
        let rows = stmt.query_map([], Self::repo_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_repo(&self, repo: &str) -> ShanResult<Option<RepoRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT repo, name, url, description, created_at, updated_at
             FROM repos WHERE repo = ?1",
        )?;
        let mut rows = stmt.query([repo])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::repo_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Register one repository. A duplicate identity is
    /// [`ShanError::RepoExists`], not a silent overwrite: PUT is the explicit
    /// edit path.
    pub fn insert_repo(
        &self,
        repo: &str,
        name: &str,
        url: &str,
        description: &str,
        now_ms: i64,
    ) -> ShanResult<()> {
        let conn = self.lock();
        match conn.execute(
            "INSERT INTO repos(repo, name, url, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            rusqlite::params![repo, name, url, description, now_ms],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(ShanError::RepoExists(repo.to_string()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Edit display fields; the identity itself is immutable. `false` means
    /// no such repository.
    pub fn update_repo(
        &self,
        repo: &str,
        name: &str,
        url: &str,
        description: &str,
        now_ms: i64,
    ) -> ShanResult<bool> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE repos SET name = ?2, url = ?3, description = ?4, updated_at = ?5
             WHERE repo = ?1",
            rusqlite::params![repo, name, url, description, now_ms],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_repo(&self, repo: &str) -> ShanResult<bool> {
        let conn = self.lock();
        let changed = conn.execute("DELETE FROM repos WHERE repo = ?1", [repo])?;
        Ok(changed > 0)
    }

    fn repo_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepoRow> {
        Ok(RepoRow {
            repo: row.get(0)?,
            name: row.get(1)?,
            url: row.get(2)?,
            description: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    pub fn start_run(&self, r: &RunStart) -> ShanResult<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO runs(id, fingerprint, runner_id, repo, started_at, phase, provider, model)
             VALUES (?1, ?2, ?3, ?4, ?5, 'claim', ?6, ?7)",
            rusqlite::params![
                r.id,
                r.fingerprint,
                r.runner_id,
                r.repo,
                r.started_at,
                r.provider,
                r.model
            ],
        )?;
        let changed = tx.execute(
            "UPDATE issues SET owner_runner_id = ?2 WHERE fingerprint = ?1",
            rusqlite::params![r.fingerprint, r.runner_id],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownIssue(r.fingerprint.clone()));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn update_run(&self, id: &str, u: &RunUpdate) -> ShanResult<()> {
        let conn = self.lock();
        // Finished runs are immutable: a late progress tick changes 0 rows and
        // surfaces as UnknownRun. M1b's progress callback must ignore that
        // error once the run has finished.
        let changed = conn.execute(
            "UPDATE runs SET
                phase = COALESCE(?2, phase),
                turns = MAX(COALESCE(?3, turns), turns),
                tokens_uncached = MAX(COALESCE(?4, tokens_uncached), tokens_uncached),
                tokens_cached = MAX(COALESCE(?5, tokens_cached), tokens_cached),
                harness_run_id = COALESCE(?6, harness_run_id),
                root_cause = COALESCE(?7, root_cause),
                verification_ok = COALESCE(?8, verification_ok),
                verification_detail = COALESCE(?9, verification_detail),
                change_stat = COALESCE(?10, change_stat),
                branch = COALESCE(?11, branch),
                commit_sha = COALESCE(?12, commit_sha),
                pr_url = COALESCE(?13, pr_url),
                pr_opened_at = COALESCE(?14, pr_opened_at),
                ci_verdict = COALESCE(?15, ci_verdict),
                session_dir = COALESCE(?16, session_dir),
                error = COALESCE(?17, error),
                compactions = COALESCE(?18, compactions),
                context_window = COALESCE(?19, context_window),
                messages = COALESCE(?20, messages)
             WHERE id = ?1 AND finished_at IS NULL",
            rusqlite::params![
                id,
                u.phase,
                u.turns,
                u.tokens_uncached,
                u.tokens_cached,
                u.harness_run_id,
                u.root_cause,
                u.verification_ok,
                u.verification_detail,
                u.change_stat,
                u.branch,
                u.commit_sha,
                u.pr_url,
                u.pr_opened_at,
                u.ci_verdict,
                u.session_dir,
                u.error,
                u.compactions,
                u.context_window,
                u.messages,
            ],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRun(id.to_string()));
        }
        Ok(())
    }

    pub fn finish_run(&self, id: &str, f: &RunFinish) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET finished_at=?2, phase='done', outcome=?3,
                ci_verdict=COALESCE(?4, ci_verdict), error=COALESCE(?5, error)
             WHERE id=?1",
            rusqlite::params![id, f.finished_at, f.outcome, f.ci_verdict, f.error],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRun(id.to_string()));
        }
        Ok(())
    }

    /// A late CI observation belongs to a run that already finished; the
    /// normal `update_run` guard rejects finished rows on purpose.
    pub fn set_run_ci_verdict(&self, id: &str, verdict: &str) -> ShanResult<()> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET ci_verdict=?2 WHERE id=?1",
            rusqlite::params![id, verdict],
        )?;
        if changed == 0 {
            return Err(ShanError::UnknownRun(id.to_string()));
        }
        Ok(())
    }

    pub fn mark_interrupted(&self, runner_id: &str, now_ms: i64) -> ShanResult<u64> {
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE runs SET finished_at=?2, phase='done', outcome='interrupted',
                error=COALESCE(error, 'runner restarted before completion')
             WHERE runner_id=?1 AND finished_at IS NULL",
            rusqlite::params![runner_id, now_ms],
        )?;
        Ok(changed as u64)
    }

    fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
        Ok(RunRow {
            id: row.get(0)?,
            harness_run_id: row.get(1)?,
            fingerprint: row.get(2)?,
            runner_id: row.get(3)?,
            repo: row.get(4)?,
            started_at: row.get(5)?,
            finished_at: row.get(6)?,
            phase: row.get(7)?,
            outcome: row.get(8)?,
            ci_verdict: row.get(9)?,
            turns: row.get(10)?,
            tokens_uncached: row.get(11)?,
            tokens_cached: row.get(12)?,
            provider: row.get(13)?,
            model: row.get(14)?,
            root_cause: row.get(15)?,
            change_stat: row.get(16)?,
            branch: row.get(17)?,
            commit_sha: row.get(18)?,
            pr_url: row.get(19)?,
            pr_opened_at: row.get(20)?,
            verification_ok: row.get(21)?,
            verification_detail: row.get(22)?,
            session_dir: row.get(23)?,
            error: row.get(24)?,
            compactions: row.get(25)?,
            context_window: row.get(26)?,
            messages: row.get(27)?,
        })
    }

    pub fn latest_run(&self, fingerprint: &str) -> ShanResult<Option<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE fingerprint=?1 ORDER BY started_at DESC, id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::run_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Newest runs for one fingerprint, used to build the next prompt's
    /// `PRIOR ATTEMPTS` section.
    pub fn recent_runs(&self, fingerprint: &str, limit: u32) -> ShanResult<Vec<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE fingerprint=?1 ORDER BY started_at DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![fingerprint, limit], Self::run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Newest owner-authored event (reopen/ignore reason) for one issue.
    pub fn latest_owner_event(&self, fingerprint: &str) -> ShanResult<Option<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{EVENT_SELECT} WHERE e.fingerprint = ?1 AND e.actor = 'owner'
             ORDER BY e.id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::event_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// One run by id, scoped to its issue. Unlike `runs_for(.., N)` this is
    /// not limited to a recent-run window, so an old run stays addressable.
    pub fn run_for(&self, fingerprint: &str, id: &str) -> ShanResult<Option<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE id=?1 AND fingerprint=?2"
        ))?;
        let mut rows = stmt.query(rusqlite::params![id, fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::run_from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Newest run of an issue that wrote a session directory — the
    /// transcript's default run. A run still in flight has no `session_dir`
    /// yet, so the newest run with one is the last readable transcript.
    pub fn latest_run_with_session(&self, fingerprint: &str) -> ShanResult<Option<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE fingerprint=?1 AND session_dir IS NOT NULL AND TRIM(session_dir) <> ''
             ORDER BY started_at DESC, id DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::run_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn runs_for(&self, fingerprint: &str, limit: u32) -> ShanResult<Vec<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE fingerprint=?1 ORDER BY started_at DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(rusqlite::params![fingerprint, limit], Self::run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs started since `since_ms`, newest first. The project rollup reads
    /// them to compute per-repo delivery and CI counters.
    pub fn runs_since(&self, since_ms: i64) -> ShanResult<Vec<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE started_at >= ?1 ORDER BY started_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([since_ms], Self::run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Signal counts per repo for the given UTC day and later. The project
    /// rollup sums the last two days to get a 24h report count.
    pub fn signal_counts_since_day(&self, day: &str) -> ShanResult<BTreeMap<String, u64>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT repo, SUM(count) FROM signal_daily WHERE day >= ?1 GROUP BY repo")?;
        let rows = stmt.query_map([day], |row| {
            let repo: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((repo, count.max(0) as u64))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs still in flight, newest first — the overview's active-run strip.
    pub fn active_runs(&self) -> ShanResult<Vec<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE finished_at IS NULL
             ORDER BY started_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], Self::run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Append a lifecycle event (console activity feed + per-issue timeline).
    pub fn record_event(
        &self,
        fingerprint: &str,
        now_ms: i64,
        repo: Option<&str>,
        actor: &str,
        kind: &str,
        detail: &str,
    ) -> ShanResult<()> {
        record_event_on(&self.lock(), fingerprint, now_ms, repo, actor, kind, detail)
    }
}

fn delivery_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRow> {
    let state: String = row.get(7)?;
    Ok(DeliveryRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        fingerprint: row.get(2)?,
        repo: row.get(3)?,
        branch: row.get(4)?,
        base: row.get(5)?,
        head_sha: row.get(6)?,
        state: DeliveryState::parse(&state).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(7, state, rusqlite::types::Type::Text)
        })?,
        pr_url: row.get(8)?,
        pr_number: row.get(9)?,
        detail: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn ci_watch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CiWatchRow> {
    let state: String = row.get(8)?;
    Ok(CiWatchRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        fingerprint: row.get(2)?,
        repo: row.get(3)?,
        pr_url: row.get(4)?,
        slug: row.get(5)?,
        pr_number: row.get(6)?,
        head_sha: row.get(7)?,
        state: CiWatchState::parse(&state).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(8, state, rusqlite::types::Type::Text)
        })?,
        last_verdict: row.get(9)?,
        last_checked_at: row.get(10)?,
        next_check_at: row.get(11)?,
        check_attempts: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        terminal_at: row.get(15)?,
        last_error: row.get(16)?,
        root_cause: row.get(17)?,
        change_stat: row.get(18)?,
        memory_written: row.get(19)?,
        max_attempts: row.get(20)?,
    })
}

/// Append using the caller's connection so state changes and their audit can
/// share a transaction without reacquiring the store mutex.
fn record_event_on(
    conn: &Connection,
    fingerprint: &str,
    now_ms: i64,
    repo: Option<&str>,
    actor: &str,
    kind: &str,
    detail: &str,
) -> ShanResult<()> {
    conn.execute(
        "INSERT INTO events(fingerprint, ts, repo, actor, kind, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![fingerprint, now_ms, repo, actor, kind, detail],
    )?;
    Ok(())
}

/// UTC calendar day (`YYYY-MM-DD`) containing `now_ms` — the `signal_daily`
/// bucket key. Out-of-range timestamps fall back to the epoch day.
pub fn utc_day(now_ms: i64) -> String {
    use time::OffsetDateTime;
    let seconds = now_ms.div_euclid(1_000);
    let dt = OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day()
    )
}

/// How many log lines of the incident window are kept.
const LOG_WINDOW_MAX_LINES: usize = 40;
/// How many characters one log line may contribute.
const LOG_WINDOW_MAX_LINE_CHARS: usize = 2_000;

/// Encode the collector's log window for storage. JSON keeps multi-line
/// entries intact; the window is bounded so a runaway collector cannot grow
/// the issue row without limit.
fn encode_log_window(window: &[String]) -> Option<String> {
    if window.is_empty() {
        return None;
    }
    let bounded: Vec<String> = window
        .iter()
        .take(LOG_WINDOW_MAX_LINES)
        .map(|line| line.chars().take(LOG_WINDOW_MAX_LINE_CHARS).collect())
        .collect();
    serde_json::to_string(&bounded).ok()
}

/// Decode a stored log window. A row written before the column existed (or
/// by a future version with a different shape) decodes to an empty window
/// instead of failing the whole read.
fn decode_log_window(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn bump_signal_daily(
    conn: &rusqlite::Connection,
    fingerprint: &str,
    repo: &str,
    now_ms: i64,
) -> ShanResult<()> {
    conn.execute(
        "INSERT INTO signal_daily(fingerprint, repo, day, count) VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(fingerprint, repo, day) DO UPDATE SET count = count + 1",
        rusqlite::params![fingerprint, repo, utc_day(now_ms)],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "panic: runtime error: index out of range [3] with length 3\n\
        goroutine 1 [running]:\n\
        example.com/spike-target.Average(...)\n\
        \t/app/calc.go:17 +0x8c";

    fn signal() -> Signal {
        Signal {
            repo: "example.com/spike-target".to_string(),
            language: "go".to_string(),
            title: "panic in Average".to_string(),
            severity: "high".to_string(),
            message: None,
            log_window: Vec::new(),
            stack_trace: TRACE.to_string(),
        }
    }

    /// Same repo/language, distinct fingerprint (stack symbol) and title.
    fn issue_signal(title: &str, symbol: &str) -> Signal {
        let mut sig = signal();
        sig.title = title.to_string();
        sig.stack_trace = TRACE.replace("Average", symbol);
        sig
    }

    #[derive(Debug, PartialEq)]
    struct WriteEffects {
        issues: String,
        events: String,
        daily: u64,
        extra: Vec<(i64, Option<String>, String)>,
    }

    // Snapshot every issue field touched by report/claim plus both journals.
    fn write_effects(store: &Store) -> WriteEffects {
        let issues = serde_json::to_string(&store.list_issues().expect("issues")).expect("json");
        let fp = fingerprint::compute(&signal().repo, "go", None, TRACE);
        let events =
            serde_json::to_string(&store.events_for(&fp, 100).expect("events")).expect("json");
        let daily = store.reports_on(&utc_day(1)).expect("daily");
        let conn = store.lock();
        let mut stmt = conn
            .prepare(
                "SELECT cooldown_until, log_sample, stack_sample FROM issues ORDER BY fingerprint",
            )
            .expect("prepare");
        let extra = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        WriteEffects {
            issues,
            events,
            daily,
            extra,
        }
    }

    fn abort_event(store: &Store, kind: &str) {
        store
            .lock()
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER fail_audit BEFORE INSERT ON events
             WHEN NEW.kind = '{kind}' BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;"
            ))
            .expect("trigger");
    }

    const TEST_PR_URL: &str = "https://example.com/pull/1";

    fn advance_to_pr(store: &Store, fp: &str) {
        for state in [State::Triggered, State::Fixing, State::PrOpen] {
            store.set_state(fp, state, 2, None).expect("advance to PR");
        }
        store.set_pr(fp, TEST_PR_URL, 2).expect("PR identity");
    }

    #[test]
    fn unscoped_claim_rolls_back_when_audit_fails() {
        assert_claim_rolls_back(false);
    }

    #[test]
    fn scoped_claim_rolls_back_when_audit_fails() {
        assert_claim_rolls_back(true);
    }

    fn assert_claim_rolls_back(scoped: bool) {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let before = write_effects(&store);
        abort_event(&store, "state_changed");
        let result = if scoped {
            store.claim_ready_issue_for_repo(2, 1_000, &signal().repo, "runner-a")
        } else {
            store.claim_ready_issue(2, 1_000)
        };
        assert!(result.is_err(), "audit failure must reach caller");
        assert_eq!(write_effects(&store), before, "scoped={scoped}");
        store
            .lock()
            .execute_batch("DROP TRIGGER fail_audit")
            .expect("drop trigger");
        assert!(store.claim_ready_issue(3, 1_000).expect("retry").is_some());
    }

    #[test]
    fn reject_pr_counts_once_and_audits_the_actor_and_repo() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&store, &fp);
        store
            .set_pr(&fp, "https://example.com/pull/1", 3)
            .expect("PR");
        assert!(store
            .reject_pr(&fp, TEST_PR_URL, 4, "CI red", "runner-a")
            .expect("reject"));
        let issue = &store.list_issues().expect("issues")[0];
        assert_eq!(
            (issue.state.as_str(), issue.attempts, issue.state_changed_at),
            ("rejected", 1, 4)
        );
        let events = store.events_for(&fp, 100).expect("events");
        let rejection: Vec<_> = events.iter().filter(|event| event.ts == 4).collect();
        assert_eq!(rejection.len(), 2);
        assert!(rejection.iter().any(|event| event.kind == "state_changed"));
        assert!(rejection.iter().any(|event| event.kind == "rejected"));
        for event in rejection {
            assert_eq!(event.actor.as_deref(), Some("runner-a"));
            assert_eq!(event.repo, signal().repo);
            assert_eq!(event.detail, "CI red");
        }
        let stored_repos: u64 = store
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE ts=4 AND repo=?1 AND actor='runner-a'",
                [&signal().repo],
                |row| row.get(0),
            )
            .expect("stored audit repo, not read fallback");
        assert_eq!(stored_repos, 2);
        let before = write_effects(&store);
        assert!(!store
            .reject_pr(&fp, TEST_PR_URL, 5, "duplicate", "runner-b")
            .expect("duplicate"));
        assert_eq!(write_effects(&store), before);
    }

    #[test]
    fn reject_pr_is_a_noop_outside_pr_open_and_unknown_is_an_error() {
        let store = Store::open(":memory:").expect("open");
        assert!(
            matches!(store.reject_pr("missing", TEST_PR_URL, 1, "CI red", "runner"),
            Err(ShanError::UnknownIssue(fp)) if fp == "missing")
        );
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store.set_pr(&fp, TEST_PR_URL, 2).expect("PR identity");
        for state in [
            "new",
            "triggered",
            "fixing",
            "merged",
            "rejected",
            "escalated",
            "ignored",
        ] {
            store
                .lock()
                .execute(
                    "UPDATE issues SET state=?1 WHERE fingerprint=?2",
                    rusqlite::params![state, fp],
                )
                .expect("fixture state");
            let before = write_effects(&store);
            assert!(!store
                .reject_pr(&fp, TEST_PR_URL, 4, "not an open PR", "runner")
                .expect("noop"));
            assert_eq!(write_effects(&store), before, "{state}");
        }
    }

    #[test]
    fn reject_pr_rolls_back_if_either_audit_fails() {
        for kind in ["state_changed", "rejected"] {
            let store = Store::open(":memory:").expect("open");
            store.report_signal(&signal(), 1).expect("report");
            let fp = store
                .ready_issue(1)
                .expect("ready")
                .expect("issue")
                .fingerprint;
            advance_to_pr(&store, &fp);
            let before = write_effects(&store);
            abort_event(&store, kind);
            assert!(store
                .reject_pr(&fp, TEST_PR_URL, 4, "CI red", "runner")
                .is_err());
            assert_eq!(write_effects(&store), before, "{kind}");
            store
                .lock()
                .execute_batch("DROP TRIGGER fail_audit")
                .expect("drop trigger");
            assert!(store
                .reject_pr(&fp, TEST_PR_URL, 5, "retry", "runner")
                .expect("retry"));
            assert_eq!(store.attempts_for(&fp).expect("attempts"), 1);
        }
    }

    #[test]
    fn issue_failure_retries_then_converges_to_ignored() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_state(&fp, State::Triggered, 2, None)
            .expect("triggered");
        store
            .set_state(&fp, State::Fixing, 2, None)
            .expect("fixing");
        let first = store
            .record_issue_failure(&fp, 10, 2, 500, "repair_failed: boom", "runner")
            .expect("first");
        assert_eq!(first.disposition, FailureDisposition::Retry);
        assert_eq!(first.state, State::New);
        assert_eq!(first.attempts, 1);
        let row = store.list_issues().expect("issues").remove(0);
        assert_eq!((row.state.as_str(), row.attempts), ("new", 1));
        assert!(store
            .events_for(&fp, 10)
            .expect("events")
            .iter()
            .any(|event| event.kind == "state_changed" && event.detail.contains("repair_failed")));
        store
            .set_state(&fp, State::Triggered, 20, None)
            .expect("triggered");
        store
            .set_state(&fp, State::Fixing, 20, None)
            .expect("fixing");
        let second = store
            .record_issue_failure(&fp, 30, 2, 500, "repair_failed: boom", "runner")
            .expect("second");
        assert_eq!(second.disposition, FailureDisposition::Ignored);
        assert_eq!(second.state, State::Ignored);
        assert_eq!(second.attempts, 2);
        let row = store.list_issues().expect("issues").remove(0);
        assert_eq!((row.state.as_str(), row.attempts), ("ignored", 2));
    }

    #[test]
    fn ci_rejection_binds_pr_and_retries_or_ignores_at_cap() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&store, &fp);
        store.set_pr(&fp, TEST_PR_URL, 2).expect("pr");
        let first = store
            .record_ci_rejection(&fp, TEST_PR_URL, 10, 2, 500, "ci_red", "runner")
            .expect("first");
        assert_eq!(first.disposition, FailureDisposition::Retry);
        assert_eq!(first.state, State::New);
        assert_eq!(first.attempts, 1);
        let row = store.list_issues().expect("issues").remove(0);
        assert!(
            row.pr_url.is_none(),
            "a retry must not keep the old PR URL: the next claim sees an orphan branch"
        );
        assert!(store
            .events_for(&fp, 100)
            .expect("events")
            .iter()
            .any(|event| event.kind == "rejected" && event.detail == "ci_red"));

        advance_to_pr(&store, &fp);
        store.set_pr(&fp, TEST_PR_URL, 20).expect("pr");
        let second = store
            .record_ci_rejection(&fp, TEST_PR_URL, 30, 2, 500, "ci_red", "runner")
            .expect("second");
        assert_eq!(second.disposition, FailureDisposition::Ignored);
        assert_eq!(second.attempts, 2);
        assert_eq!(store.list_issues().expect("issues")[0].state, "ignored");
    }

    #[test]
    fn delivery_intent_is_idempotent_and_pending_until_pr_created() {
        let store = Store::open(":memory:").expect("open");
        let start = DeliveryStart {
            id: "delivery-1".into(),
            run_id: Some("run-1".into()),
            fingerprint: "fp-delivery".into(),
            repo: "example.com/app".into(),
            branch: "agent/fix/fp".into(),
            base: "main".into(),
            head_sha: "abc123".into(),
            now: 10,
        };
        let first = store.begin_delivery(&start).expect("first");
        let second = store
            .begin_delivery(&DeliveryStart {
                id: "delivery-2".into(),
                now: 11,
                ..start
            })
            .expect("second");
        assert_eq!(first.id, second.id, "same branch/head keeps the identity");
        assert_eq!(
            store
                .pending_deliveries("example.com/app")
                .expect("pending")
                .len(),
            1
        );
        store
            .mark_delivery(
                &first.id,
                DeliveryState::PrCreated,
                Some("https://github.com/o/r/pull/1"),
                Some(1),
                None,
                20,
            )
            .expect("created");
        assert!(store
            .pending_deliveries("example.com/app")
            .expect("pending")
            .is_empty());
    }

    #[test]
    fn recovered_delivery_adopts_pr_without_replaying_fixing_transition() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_state(&fp, State::Triggered, 2, None)
            .expect("triggered");
        store
            .set_state(&fp, State::Fixing, 2, None)
            .expect("fixing");
        assert!(store
            .adopt_delivered_pr(&fp, "https://github.com/o/r/pull/5", 10)
            .expect("adopt"));
        let row = store.list_issues().expect("issues").remove(0);
        assert_eq!(row.state, "pr_open");
        assert_eq!(row.pr_url.as_deref(), Some("https://github.com/o/r/pull/5"));
        assert!(store
            .events_for(&fp, 20)
            .expect("events")
            .iter()
            .any(|event| event.detail.contains("PR adopted from GitHub")));
        store
            .set_state(&fp, State::Merged, 30, None)
            .expect("merged");
        assert!(!store
            .adopt_delivered_pr(&fp, "https://github.com/o/r/pull/6", 40)
            .expect("terminal no-op"));
    }

    #[test]
    fn due_ci_watches_are_claimed_once_and_keep_their_identity() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run-ci".into(),
                fingerprint: fp.clone(),
                runner_id: "runner".into(),
                repo: "example.com/app".into(),
                started_at: 1,
                provider: None,
                model: None,
            })
            .expect("run");
        store
            .upsert_ci_watch(&CiWatchInput {
                run_id: Some("run-ci".into()),
                fingerprint: fp.clone(),
                repo: "example.com/app".into(),
                pr_url: "https://github.com/o/r/pull/9".into(),
                slug: "o/r".into(),
                pr_number: 9,
                head_sha: "deadbeef".into(),
                root_cause: None,
                change_stat: None,
                now: 10,
                next_check_at: 10,
                max_attempts: 3,
            })
            .expect("watch");
        let claimed = store.claim_due_ci_watches(10, 10, 60).expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].state, CiWatchState::Pending);
        assert!(store
            .claim_due_ci_watches(11, 10, 60)
            .expect("second claim")
            .is_empty());
        store
            .update_ci_watch(
                claimed[0].id,
                CiWatchState::Green,
                Some("green"),
                100,
                None,
                true,
                None,
                20,
            )
            .expect("green");
        let row = store.ci_watch_for(&fp).expect("read").expect("watch");
        assert_eq!(row.state, CiWatchState::Green);
        assert!(row.memory_written);
    }

    #[test]
    fn reject_pr_counts_a_new_cycle_after_regression() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&store, &fp);
        assert!(store
            .reject_pr(&fp, TEST_PR_URL, 3, "first", "runner")
            .expect("first"));
        assert!(matches!(
            store.report_signal(&signal(), 4).expect("regression"),
            ReportOutcome::Regression { occurrences: 2, .. }
        ));
        assert_eq!(store.attempts_for(&fp).expect("attempts"), 1);
        advance_to_pr(&store, &fp);
        assert!(store
            .reject_pr(&fp, TEST_PR_URL, 5, "second", "runner")
            .expect("second"));
        assert_eq!(store.attempts_for(&fp).expect("attempts"), 2);
        assert_eq!(
            store
                .events_for(&fp, 100)
                .expect("events")
                .iter()
                .filter(|event| event.kind == "rejected")
                .count(),
            2
        );
    }

    #[test]
    fn review_late_old_pr_rejection_cannot_reject_a_new_cycle() {
        let store = Store::open(":memory:").expect("test fixture");
        store.report_signal(&signal(), 1).expect("test fixture");
        let fp = store
            .ready_issue(1)
            .expect("test fixture")
            .expect("test fixture")
            .fingerprint;
        let old_url = "https://github.com/example/app/pull/1";
        let new_url = "https://github.com/example/app/pull/2";
        advance_to_pr(&store, &fp);
        store.set_pr(&fp, old_url, 2).expect("test fixture");
        assert!(store
            .reject_pr(&fp, old_url, 3, "first PR closed", "runner")
            .expect("test fixture"));
        store.report_signal(&signal(), 4).expect("test fixture");
        advance_to_pr(&store, &fp);
        store.set_pr(&fp, new_url, 5).expect("test fixture");
        let before = write_effects(&store);
        assert!(
            !store
                .reject_pr(&fp, old_url, 6, "old PR closed", "late-reconcile")
                .expect("test fixture"),
            "a verdict queried for the old PR must not reject the new PR"
        );
        assert_eq!(write_effects(&store), before);
        let row = &store.list_issues().expect("test fixture")[0];
        assert_eq!(row.state, "pr_open");
        assert_eq!(row.pr_url.as_deref(), Some(new_url));
        assert_eq!(row.attempts, 1);
        assert!(store
            .reject_pr(&fp, new_url, 7, "new PR closed", "reconcile")
            .expect("test fixture"));
        let after = write_effects(&store);
        assert!(!store
            .reject_pr(&fp, new_url, 8, "new PR still closed", "repeat-reconcile")
            .expect("test fixture"));
        assert_eq!(write_effects(&store), after);
        assert_eq!(store.attempts_for(&fp).expect("test fixture"), 2);
    }

    #[test]
    fn concurrent_reject_pr_counts_at_most_once_across_connections() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.db");
        let first = Store::open(&path).expect("first");
        let second = Store::open(&path).expect("second");
        first.report_signal(&signal(), 1).expect("report");
        let fp = first
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&first, &fp);
        let barrier = std::sync::Barrier::new(2);
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                first.reject_pr(&fp, TEST_PR_URL, 4, "CI red", "runner-a")
            });
            let b = scope.spawn(|| {
                barrier.wait();
                second.reject_pr(&fp, TEST_PR_URL, 4, "CI red", "runner-b")
            });
            (
                a.join().expect("join a").expect("reject a"),
                b.join().expect("join b").expect("reject b"),
            )
        });
        assert_ne!(a, b, "exactly one caller must win");
        assert_eq!(first.attempts_for(&fp).expect("attempts"), 1);
        let events = first.events_for(&fp, 100).expect("events");
        let rejection: Vec<_> = events.iter().filter(|event| event.ts == 4).collect();
        assert_eq!(rejection.len(), 2);
        for event in rejection {
            assert_eq!(
                event.actor.as_deref(),
                Some(if a { "runner-a" } else { "runner-b" })
            );
        }
    }

    #[test]
    fn first_report_rolls_back_when_audit_fails() {
        let store = Store::open(":memory:").expect("open");
        let before = write_effects(&store);
        abort_event(&store, "reported");
        assert!(store.report_signal(&signal(), 1).is_err());
        assert_eq!(write_effects(&store), before);
    }

    #[test]
    fn regression_report_rolls_back_when_audit_fails() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&store, &fp);
        store
            .set_state(&fp, State::Rejected, 3, None)
            .expect("reject");
        store.set_cooldown(&fp, 1_000).expect("cooldown");
        let before = write_effects(&store);
        abort_event(&store, "state_changed");
        let mut sig = signal();
        sig.severity = "critical".into();
        sig.log_window = vec!["new log".into()];
        assert!(store.report_signal(&sig, 4).is_err());
        assert_eq!(write_effects(&store), before);
    }

    #[test]
    fn repeat_report_rolls_back_when_daily_count_fails() {
        let store = Store::open(":memory:").expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let before = write_effects(&store);
        store
            .lock()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_daily BEFORE INSERT ON signal_daily
             BEGIN SELECT RAISE(ABORT, 'injected daily failure'); END;",
            )
            .expect("trigger");
        assert!(store.report_signal(&signal(), 2).is_err());
        assert_eq!(write_effects(&store), before);
    }

    type WriterGate = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
    thread_local! {
        static WRITER_GATE: std::cell::RefCell<Option<WriterGate>> = const { std::cell::RefCell::new(None) };
    }

    // SQLite invokes this at the blocked write (or BEGIN IMMEDIATE). The test
    // releases the writer only after committing ignore, without timing sleeps.
    fn wait_for_test_writer(_: i32) -> bool {
        WRITER_GATE.with(|gate| {
            gate.borrow_mut().take().is_some_and(|(blocked, resume)| {
                blocked.send(()).is_ok()
                    && resume
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .is_ok()
            })
        })
    }

    #[test]
    fn concurrent_ignore_wins_before_report_can_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.db");
        let owner = Store::open(&path).expect("owner");
        let reporter = Store::open(&path).expect("reporter");
        owner.report_signal(&signal(), 1).expect("report");
        let fp = owner
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        advance_to_pr(&owner, &fp);
        owner
            .set_state(&fp, State::Rejected, 3, None)
            .expect("reject");
        owner
            .lock()
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold writer");
        let (blocked_tx, blocked_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let report = std::thread::spawn(move || {
            WRITER_GATE.with(|gate| *gate.borrow_mut() = Some((blocked_tx, resume_rx)));
            reporter
                .lock()
                .busy_handler(Some(wait_for_test_writer))
                .expect("busy handler");
            reporter.report_signal(&signal(), 5)
        });
        blocked_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("report blocked");
        owner
            .ignore_issue(&fp, "owner mute", "owner", 4)
            .expect("ignore");
        owner.lock().execute_batch("COMMIT").expect("commit ignore");
        resume_tx.send(()).expect("resume report");
        let outcome = report.join().expect("join").expect("report");
        assert!(
            matches!(outcome, ReportOutcome::Muted { occurrences: 2, .. }),
            "{outcome:?}"
        );
        assert_eq!(owner.list_issues().expect("issues")[0].state, "ignored");
        assert_eq!(owner.reports_on(&utc_day(5)).expect("daily"), 2);
        assert!(!owner
            .events_for(&fp, 100)
            .expect("events")
            .iter()
            .any(|event| event.detail.starts_with("regression:")));
    }

    #[test]
    fn delayed_progress_cannot_reduce_run_usage() {
        let store = Store::open(":memory:").expect("store");
        store.report_signal(&signal(), 1).expect("signal");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "usage-order".into(),
                fingerprint: fp.clone(),
                runner_id: "runner".into(),
                repo: signal().repo,
                started_at: 2,
                provider: None,
                model: None,
            })
            .expect("run");
        for (turns, uncached, cached) in [(5, 1000, 100), (2, 400, 20)] {
            store
                .update_run(
                    "usage-order",
                    &RunUpdate {
                        turns: Some(turns),
                        tokens_uncached: Some(uncached),
                        tokens_cached: Some(cached),
                        ..Default::default()
                    },
                )
                .expect("progress");
        }
        let run = store
            .run_for(&fp, "usage-order")
            .expect("read")
            .expect("run");
        assert_eq!(
            (run.turns, run.tokens_uncached, run.tokens_cached),
            (5, 1000, 100)
        );
    }

    #[test]
    fn cross_repo_same_stack_are_separate_issues() {
        // Two repositories sharing a fully-qualified symbol and message (forks,
        // vendored copies) are two bugs: each must get its own row and be
        // claimable by its own runner. Before repo entered the fingerprint the
        // second report deduped onto the first, so repo B could never be picked.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let a = signal();
        let mut b = signal();
        b.repo = "example.com/other".to_string();
        store.report_signal(&a, 1).expect("a");
        let outcome_b = store.report_signal(&b, 2).expect("b");
        assert!(
            matches!(outcome_b, ReportOutcome::NewIssue { .. }),
            "a different repo's identical stack must be a new issue, got {outcome_b:?}"
        );
        let fp_a = fingerprint::compute(&a.repo, "go", None, &a.stack_trace);
        let fp_b = fingerprint::compute(&b.repo, "go", None, &b.stack_trace);
        assert_ne!(fp_a, fp_b);
        // Each repo's runner can claim its own.
        assert!(store
            .claim_ready_issue_for_repo(3, 1_000, "example.com/spike-target", "r-a")
            .expect("claim a")
            .is_some());
        assert!(store
            .claim_ready_issue_for_repo(3, 1_000, "example.com/other", "r-b")
            .expect("claim b")
            .is_some());
    }

    /// 采集器的日志窗口必须随问题单落库并随快照交给 runner——否则修复
    /// prompt 里的 LOG WINDOW 永远是 "(none)"。
    #[test]
    fn log_window_round_trips_through_the_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let mut sig = signal();
        sig.log_window = vec!["line one".into(), "line two\nwith newline".into()];
        let fp = match store.report_signal(&sig, 1).expect("report") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected new, got {other:?}"),
        };
        let snapshot = store.ready_issue(1).expect("ready").expect("issue");
        assert_eq!(snapshot.fingerprint, fp);
        assert_eq!(
            snapshot.log_window,
            vec!["line one".to_string(), "line two\nwith newline".to_string()]
        );
        // The sighting window comes from the row, not from the prompt.
        assert_eq!(snapshot.first_seen, 1);
        assert_eq!(snapshot.last_seen, 1);
    }

    #[test]
    fn log_window_is_bounded_and_refreshed_only_when_provided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let mut sig = signal();
        sig.log_window = (0..60).map(|i| format!("line {i}")).collect();
        store.report_signal(&sig, 1).expect("report");
        let snapshot = store.ready_issue(1).expect("ready").expect("issue");
        assert_eq!(snapshot.log_window.len(), 40, "window is capped");

        // A later report without a window keeps the stored one.
        let mut quiet = signal();
        quiet.log_window = Vec::new();
        store.report_signal(&quiet, 2).expect("report again");
        let snapshot = store.ready_issue(2).expect("ready").expect("issue");
        assert_eq!(snapshot.log_window.len(), 40);
        assert_eq!(snapshot.last_seen, 2);

        // A later report with a window replaces it.
        let mut fresh = signal();
        fresh.log_window = vec!["fresh".into()];
        store.report_signal(&fresh, 3).expect("third");
        let snapshot = store.ready_issue(3).expect("ready").expect("issue");
        assert_eq!(snapshot.log_window, vec!["fresh".to_string()]);
    }

    #[test]
    fn repeat_report_does_not_downgrade_severity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let mut critical = signal();
        critical.severity = "critical".to_string();
        store.report_signal(&critical, 1).expect("first");
        // A noisy low-severity re-report of the same bug must not sink it.
        let mut low = signal();
        low.severity = "low".to_string();
        store.report_signal(&low, 2).expect("repeat");
        let fp = fingerprint::compute(&critical.repo, "go", None, &critical.stack_trace);
        let row = store
            .list_issues()
            .expect("list")
            .into_iter()
            .find(|r| r.fingerprint == fp)
            .expect("row");
        assert_eq!(
            row.severity, "critical",
            "severity must keep the confirmed max"
        );
        assert_eq!(row.occurrences, 2);
    }

    #[test]
    fn claim_stamps_owner_so_another_runner_cannot_revive_it() {
        // The claim registers the owner atomically; a second runner's startup
        // orphan-revive must not treat that live claim as an orphan and requeue
        // it (which would let two runners work the same issue).
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let claimed = store
            .claim_ready_issue_for_repo(2, 1_000_000, "example.com/spike-target", "runner-a")
            .expect("claim")
            .expect("claimed");
        let row = store
            .list_issues()
            .expect("list")
            .into_iter()
            .find(|r| r.fingerprint == claimed.fingerprint)
            .expect("row");
        assert_eq!(row.owner_runner_id.as_deref(), Some("runner-a"));
        assert_eq!(row.state, "triggered");
        // A different runner must not revive it; the owner's own restart would.
        assert_eq!(
            store
                .revive_orphaned_fixing(3, "runner-b")
                .expect("revive b"),
            0,
            "another runner must not steal a live claim"
        );
        assert_eq!(
            store
                .revive_orphaned_fixing(3, "runner-a")
                .expect("revive a"),
            1,
            "the owner's restart revives its own orphaned claim"
        );
    }

    #[test]
    fn concurrent_same_transition_admits_exactly_one_writer() {
        // Two processes racing the same triggered→fixing edge: the CAS guard
        // (UPDATE … AND state = <validated from>) means exactly one writer
        // lands; the loser sees the state already moved and is rejected rather
        // than both "succeeding" past the transition check.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("race.db");
        {
            let store = Store::open(&path).expect("open");
            store.report_signal(&signal(), 1).expect("report");
            store
                .claim_ready_issue_for_repo(2, 1_000_000, "example.com/spike-target", "r")
                .expect("claim")
                .expect("claimed");
        }
        let fp = fingerprint::compute("example.com/spike-target", "go", None, TRACE);
        let results: Vec<bool> = (0..2)
            .map(|i| {
                let path = path.clone();
                let fp = fp.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&path).expect("open");
                    store
                        .set_state_as(&fp, State::Fixing, 3, None, &format!("r{i}"))
                        .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();
        assert_eq!(
            results.iter().filter(|ok| **ok).count(),
            1,
            "exactly one writer may apply triggered→fixing, got {results:?}"
        );
    }

    /// Start a run and fill the evidence columns the trends buckets read.
    fn seed_run(
        store: &Store,
        id: &str,
        fingerprint: &str,
        repo: &str,
        started_at: i64,
        pr_opened_at: Option<i64>,
        tokens: (i64, i64),
    ) {
        store
            .start_run(&RunStart {
                id: id.to_string(),
                fingerprint: fingerprint.to_string(),
                runner_id: "host:runner".to_string(),
                repo: repo.to_string(),
                started_at,
                provider: None,
                model: None,
            })
            .expect("seed run");
        store
            .update_run(
                id,
                &RunUpdate {
                    tokens_uncached: Some(tokens.0),
                    tokens_cached: Some(tokens.1),
                    pr_opened_at,
                    ..RunUpdate::default()
                },
            )
            .expect("seed run update");
    }

    #[test]
    fn new_then_dedup_then_cooldown_then_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_000_000;

        match store.report_signal(&signal(), t0).expect("report") {
            ReportOutcome::NewIssue { fingerprint } => {
                assert!(!fingerprint.is_empty());
            }
            other => panic!("expected NewIssue, got {other:?}"),
        }
        // Same fingerprint → Deduped, occurrences bump.
        match store.report_signal(&signal(), t0 + 1).expect("report") {
            ReportOutcome::Deduped { occurrences, .. } => assert_eq!(occurrences, 2),
            other => panic!("expected Deduped, got {other:?}"),
        }
        // Ready immediately (cooldown 0) while state = new.
        assert!(store.ready_issue(t0 + 2).expect("ready").is_some());

        // Runner picks up: triggered + cooldown 1h — further reports dedup
        // into CooldownActive, not a new trigger.
        let fp = store
            .ready_issue(t0 + 2)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_state(&fp, State::Triggered, t0 + 3, None)
            .expect("state");
        store.set_cooldown(&fp, t0 + 3_600_000).expect("cooldown");
        match store.report_signal(&signal(), t0 + 10).expect("report") {
            ReportOutcome::CooldownActive {
                occurrences,
                retry_after_ms,
                ..
            } => {
                assert_eq!(occurrences, 3);
                assert_eq!(retry_after_ms, 3_600_000 - 10);
            }
            other => panic!("expected CooldownActive, got {other:?}"),
        }
        // Not pickable while triggered.
        assert!(store.ready_issue(t0 + 11).expect("ready").is_none());
    }

    #[test]
    fn regression_reopens_terminal_issue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 5_000_000;
        store.report_signal(&signal(), t0).expect("report");
        let fp = store
            .ready_issue(t0)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store.set_state(&fp, State::Triggered, t0, None).expect("t");
        store.set_state(&fp, State::Fixing, t0, None).expect("f");
        store.set_state(&fp, State::PrOpen, t0, None).expect("p");
        store.set_state(&fp, State::Merged, t0, None).expect("m");

        match store.report_signal(&signal(), t0 + 1).expect("report") {
            ReportOutcome::Regression { occurrences, .. } => assert_eq!(occurrences, 2),
            other => panic!("expected Regression, got {other:?}"),
        }
        let issue = store.get(&fp).expect("get").expect("issue");
        assert_eq!(issue.occurrences, 2);
        // Reopened: pickable again.
        assert!(store.ready_issue(t0 + 2).expect("ready").is_some());
    }

    #[test]
    fn regression_reopen_clears_cooldown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 7_000_000;
        store.report_signal(&signal(), t0).expect("report");
        let fp = store
            .ready_issue(t0)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store.set_state(&fp, State::Triggered, t0, None).expect("t");
        store.set_state(&fp, State::Fixing, t0, None).expect("f");
        store.set_state(&fp, State::PrOpen, t0, None).expect("p");
        store.set_state(&fp, State::Merged, t0, None).expect("m");
        // The run that produced the merge left a claim cooldown behind.
        store
            .set_cooldown(&fp, t0 + DEFAULT_COOLDOWN_MS)
            .expect("cooldown");

        match store.report_signal(&signal(), t0 + 1_000).expect("report") {
            ReportOutcome::Regression { occurrences, .. } => assert_eq!(occurrences, 2),
            other => panic!("expected Regression, got {other:?}"),
        }
        // A merged bug reporting again is pickable at once — it must not
        // wait out the previous run's stale cooldown.
        assert!(store.ready_issue(t0 + 1_000).expect("ready").is_some());
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        assert!(!can_transition(State::New, State::PrOpen));
        assert!(!can_transition(State::New, State::Merged));
        assert!(can_transition(State::PrOpen, State::Rejected));

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        let err = store
            .set_state(&fp, State::PrOpen, 2, None)
            .expect_err("new -> pr_open must fail");
        assert!(matches!(err, ShanError::InvalidTransition { .. }));
    }

    /// Wire contract: a 青鸟 (qingniao) envelope JSON must parse as a Signal —
    /// unknown envelope fields (schema/service/host/ts) are ignored, missing
    /// optional fields default. This pins the A-5 collector contract.
    #[test]
    fn qingniao_envelope_parses_as_signal() {
        let envelope = r#"{"schema":"qingniao/v1","repo":"example.com/spike-target","language":"go","service":"reporter","host":"host-1","title":"panic: boom","severity":"high","message":"boom","stack_trace":"panic: boom\ngoroutine 1 [running]:\n","ts_unix_ms":123}"#;
        let signal: Signal = serde_json::from_str(envelope).expect("envelope must parse as Signal");
        assert_eq!(signal.repo, "example.com/spike-target");
        assert_eq!(signal.severity, "high");

        // Optional fields fall back to defaults.
        let minimal: Signal = serde_json::from_str(
            r#"{"repo":"r","language":"go","title":"t","stack_trace":"panic: x"}"#,
        )
        .expect("minimal envelope must parse");
        assert_eq!(minimal.severity, "medium");

        // And the parsed signal fingerprints end-to-end.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        match store.report_signal(&signal, 1).expect("report") {
            ReportOutcome::NewIssue { fingerprint } => assert!(!fingerprint.is_empty()),
            other => panic!("expected NewIssue, got {other:?}"),
        }
    }

    #[test]
    fn claim_is_atomic_exclusive_and_ordered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let mut critical = signal();
        critical.severity = "critical".to_string();
        critical.stack_trace = TRACE.replace("Average", "Submit");
        let mut low = signal();
        low.severity = "low".to_string();
        low.stack_trace = TRACE.replace("Average", "Load");

        store.report_signal(&low, 1).expect("low");
        store.report_signal(&critical, 2).expect("critical");

        // First claim takes the critical issue (priority order), flipping it
        // to triggered inside the claim itself.
        let first = store
            .claim_ready_issue(10, 1_000_000)
            .expect("claim 1")
            .expect("some issue");
        assert_eq!(first.severity, "critical");

        // The claimed issue is not pickable by a second runner.
        let second = store
            .claim_ready_issue(11, 1_000_000)
            .expect("claim 2")
            .expect("remaining issue");
        assert_eq!(second.severity, "low");

        // Queue drained.
        assert!(store
            .claim_ready_issue(12, 1_000_000)
            .expect("claim 3")
            .is_none());

        // No observer view either: claimed issues left the ready set.
        assert!(store.ready_issue(13).expect("ready").is_none());
    }

    #[test]
    fn daily_pr_count_is_per_repo_and_time_windowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let mut b = signal();
        b.stack_trace = TRACE.replace("Average", "Submit");
        b.repo = "other/repo".to_string();
        store.report_signal(&signal(), 1).expect("a");
        store.report_signal(&b, 2).expect("b");
        let fp_a = fingerprint::compute(&signal().repo, "go", None, &signal().stack_trace);
        let mut c = signal();
        c.stack_trace = TRACE.replace("Average", "Load");
        let fp_c = fingerprint::compute(&c.repo, "go", None, &c.stack_trace);
        store.report_signal(&c, 3).expect("c");

        store.set_pr(&fp_a, "https://x/pr/1", 10).expect("pr1");
        assert_eq!(
            store
                .prs_opened_since("example.com/spike-target", 0)
                .expect("count"),
            1
        );
        assert_eq!(store.prs_opened_since("other/repo", 0).expect("count"), 0);

        store.set_pr(&fp_c, "https://x/pr/2", 20).expect("pr2");
        assert_eq!(
            store
                .prs_opened_since("example.com/spike-target", 0)
                .expect("count"),
            2
        );
        assert_eq!(
            store
                .prs_opened_since("example.com/spike-target", 15)
                .expect("count"),
            1,
            "window starts at since_ms"
        );
    }

    #[test]
    fn pr_quota_counts_distinct_urls_from_runs_and_issues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 1_700_000_000_000;
        store.report_signal(&signal(), now).expect("report");
        let fp = store
            .ready_issue(now)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_pr(&fp, "https://github.com/a/b/pull/1", now)
            .expect("legacy pr");
        store
            .start_run(&RunStart {
                id: "run1".into(),
                fingerprint: fp.clone(),
                runner_id: "r".into(),
                repo: signal().repo,
                started_at: now,
                provider: None,
                model: None,
            })
            .expect("run");
        store
            .update_run(
                "run1",
                &RunUpdate {
                    pr_url: Some("https://github.com/a/b/pull/1".into()),
                    pr_opened_at: Some(now + 1),
                    ..RunUpdate::default()
                },
            )
            .expect("update");
        assert_eq!(
            store
                .prs_opened_since(&signal().repo, now - 1)
                .expect("count"),
            1
        );

        store
            .update_run(
                "run1",
                &RunUpdate {
                    pr_url: Some("https://github.com/a/b/pull/2".into()),
                    pr_opened_at: Some(now + 2),
                    ..RunUpdate::default()
                },
            )
            .expect("update2");
        assert_eq!(
            store
                .prs_opened_since(&signal().repo, now - 1)
                .expect("count"),
            2
        );
    }

    #[test]
    fn prune_removes_old_rows_and_keeps_audit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day = 86_400_000;
        let now = 400 * day;
        store
            .record_event(
                "fp",
                now - 100 * day,
                Some("r"),
                "system",
                "reported",
                "old",
            )
            .expect("e");
        store
            .record_event(
                "fp",
                now - 100 * day,
                Some("r"),
                "owner",
                "config_changed",
                "k",
            )
            .expect("audit");
        store
            .record_event(
                "fp",
                now - 400 * day,
                Some("r"),
                "owner",
                "config_changed",
                "old-audit",
            )
            .expect("audit old");
        store
            .upsert_runner(&RunnerUpsert {
                id: "r".into(),
                repo: "r".into(),
                host: "h".into(),
                pid: None,
                started_at: 0,
                heartbeat_at: now - 40 * day,
                status: "idle".into(),
                provider: None,
                model: None,
                daily_pr_limit: None,
                repo_path: None,
                service_unit: None,
                config_json: "{}".into(),
            })
            .expect("runner");
        let deleted = store.prune(now, 90, 365, 365).expect("prune");
        assert!(deleted >= 3);
        let events = store.recent_events(100).expect("events");
        assert!(events
            .iter()
            .any(|e| e.kind == "config_changed" && e.detail == "k"));
        assert!(!events.iter().any(|e| e.detail == "old"));
        assert!(!events.iter().any(|e| e.detail == "old-audit"));
        assert!(store.list_runners().expect("runners").is_empty());
    }

    #[test]
    fn prune_honours_the_audit_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day = 86_400_000;
        let now = 500 * day;
        store
            .record_event(
                "fp",
                now - 410 * day,
                Some("r"),
                "system",
                "reported",
                "old",
            )
            .expect("e");
        store
            .record_event(
                "fp",
                now - 380 * day,
                Some("r"),
                "owner",
                "config_changed",
                "audit",
            )
            .expect("audit");
        // audit_days=400 must outlive both the normal-event window and the
        // historical 365-day audit default.
        let deleted = store.prune(now, 400, 400, 365).expect("prune");
        assert_eq!(deleted, 1);
        let events = store.recent_events(100).expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].detail, "audit");
    }

    #[test]
    fn priority_orders_by_severity_times_occurrences() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let low = Signal {
            title: "low".into(),
            severity: "low".into(),
            ..signal()
        };
        let mut critical = signal();
        critical.title = "critical".into();
        critical.severity = "critical".into();
        critical.stack_trace = TRACE.replace("Average", "Submit");

        store.report_signal(&low, 1).expect("low");
        store.report_signal(&low, 2).expect("low"); // 1 × 2 = 2
        store.report_signal(&critical, 1).expect("critical"); // 10 × 1 = 10

        let top = store.ready_issue(3).expect("ready").expect("issue");
        assert_eq!(top.severity, "critical");
    }

    #[test]
    fn events_carry_repo_and_actor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .record_event(
                "fp1",
                1,
                Some("example.com/r"),
                "owner",
                "requeued",
                "manual",
            )
            .expect("event");
        let events = store.recent_events(10).expect("events");
        let event = events.first().expect("event");
        assert_eq!(event.repo, "example.com/r");
        assert_eq!(event.actor.as_deref(), Some("owner"));
        assert_eq!(event.kind, "requeued");
    }

    #[test]
    fn set_state_as_persists_actor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_state_as(&fp, State::Triggered, 2, Some("claimed"), "runner")
            .expect("state");
        let events = store.events_for(&fp, 10).expect("events");
        assert!(events
            .iter()
            .any(|e| e.kind == "state_changed" && e.actor.as_deref() == Some("runner")));
    }

    #[test]
    fn run_lifecycle_and_latest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;

        store
            .start_run(&RunStart {
                id: "run1".into(),
                fingerprint: fp.clone(),
                runner_id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                started_at: 100,
                provider: Some("zai".into()),
                model: Some("glm-5.3-flash".into()),
            })
            .expect("start");

        store
            .update_run(
                "run1",
                &RunUpdate {
                    phase: Some("repair".into()),
                    turns: Some(3),
                    tokens_uncached: Some(1200),
                    tokens_cached: Some(300),
                    session_dir: Some("sessions/xxxx".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("update");
        store
            .finish_run(
                "run1",
                &RunFinish {
                    finished_at: 200,
                    outcome: "success".into(),
                    ci_verdict: Some("success".into()),
                    error: None,
                },
            )
            .expect("finish");

        let run = store.latest_run(&fp).expect("latest").expect("run");
        assert_eq!(run.id, "run1");
        assert_eq!(run.phase, "done");
        assert_eq!(run.outcome.as_deref(), Some("success"));
        assert_eq!(run.turns, 3);
        assert_eq!(run.tokens_uncached, 1200);
        assert_eq!(run.finished_at, Some(200));
    }

    #[test]
    fn latest_run_and_runs_for_break_started_at_ties_by_newest_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;

        // Insert out of id order so rowid alone cannot produce the result.
        for id in ["run-b", "run-a", "run-c"] {
            store
                .start_run(&RunStart {
                    id: id.into(),
                    fingerprint: fp.clone(),
                    runner_id: "host:repo".into(),
                    repo: "example.com/spike-target".into(),
                    started_at: 100,
                    provider: None,
                    model: None,
                })
                .expect("start");
        }

        let latest = store.latest_run(&fp).expect("latest").expect("run");
        assert_eq!(latest.id, "run-c", "ties resolve to the newest id");
        store
            .update_run(
                "run-c",
                &RunUpdate {
                    ci_verdict: Some("success".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("update tied run");
        let ids: Vec<String> = store
            .runs_for(&fp, 10)
            .expect("runs")
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids, ["run-c", "run-b", "run-a"]);
        // The issue's ci_verdict follows the same deterministic newest run.
        let issue = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|issue| issue.fingerprint == fp)
            .expect("issue row");
        assert_eq!(issue.ci_verdict.as_deref(), Some("success"));
    }

    #[test]
    fn run_for_is_scoped_to_the_issue_and_ignores_the_recent_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let fp = match store
            .report_signal(&issue_signal("old run", "Average"), 1)
            .expect("report")
        {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected NewIssue, got {other:?}"),
        };
        let other = match store
            .report_signal(&issue_signal("other run", "Median"), 1)
            .expect("report")
        {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected NewIssue, got {other:?}"),
        };
        assert_ne!(fp, other);

        seed_run(
            &store,
            "run-old",
            &fp,
            "example.com/spike-target",
            100,
            None,
            (0, 0),
        );
        seed_run(
            &store,
            "run-new",
            &fp,
            "example.com/spike-target",
            200,
            None,
            (0, 0),
        );
        seed_run(
            &store,
            "run-other",
            &other,
            "example.com/spike-target",
            300,
            None,
            (0, 0),
        );

        // An old run id is found directly, without scanning recent runs.
        let old = store
            .run_for(&fp, "run-old")
            .expect("run_for")
            .expect("old run");
        assert_eq!(old.id, "run-old");
        assert_eq!(old.fingerprint, fp);
        // A run of another fingerprint is never visible through this issue.
        assert!(store
            .run_for(&other, "run-old")
            .expect("cross fp")
            .is_none());
        assert!(store.run_for(&fp, "missing").expect("missing").is_none());

        // Default transcript run: the newest run that wrote a session dir,
        // even while a newer run is still in flight without one.
        store
            .update_run(
                "run-old",
                &RunUpdate {
                    session_dir: Some("sessions/old".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("session dir");
        let latest = store.latest_run(&fp).expect("latest").expect("run");
        assert_eq!(latest.id, "run-new");
        let session = store
            .latest_run_with_session(&fp)
            .expect("latest with session")
            .expect("run");
        assert_eq!(session.id, "run-old");
        assert!(store
            .latest_run_with_session(&other)
            .expect("other")
            .is_none());
    }

    #[test]
    fn stuck_detection_uses_owner_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let old = 1_000_000;
        store.report_signal(&signal(), old).expect("report");
        let fp = store
            .claim_ready_issue(old, old + 10_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run1".into(),
                fingerprint: fp.clone(),
                runner_id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                started_at: old,
                provider: None,
                model: None,
            })
            .expect("run");

        store
            .upsert_runner(&RunnerUpsert {
                id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                host: "host".into(),
                pid: Some(1),
                started_at: old,
                heartbeat_at: old,
                status: "working".into(),
                provider: None,
                model: None,
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: "{}".into(),
            })
            .expect("runner");

        // 心跳新鲜且正在修这条单 → 不卡。
        assert!(!store.stuck(&fp, old + 1_000, 300_000).expect("stuck"));
        store
            .heartbeat_runner(
                "host:repo",
                &RunnerHeartbeat {
                    heartbeat_at: old + 1_000,
                    status: "working".into(),
                    current_fingerprint: Some(fp.clone()),
                    phase: Some("repair".into()),
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("heartbeat");
        assert!(!store.stuck(&fp, old + 2_000, 300_000).expect("stuck"));

        // 心跳过期 → 卡。
        assert!(store.stuck(&fp, old + 500_000, 300_000).expect("stuck"));

        // runner 已 idle：从最后一次心跳起超过阈值才判卡。
        store
            .heartbeat_runner(
                "host:repo",
                &RunnerHeartbeat {
                    heartbeat_at: old + 500_000,
                    status: "idle".into(),
                    current_fingerprint: None,
                    phase: None,
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("heartbeat");
        assert!(!store.stuck(&fp, old + 500_001, 300_000).expect("not yet"));
        assert!(store.stuck(&fp, old + 800_001, 300_000).expect("stuck"));
    }

    /// A fresh claim has no owner heartbeat yet; the claim-time
    /// `state_changed_at` stamp is its only progress marker, so it must not
    /// count as stuck merely for lacking a runner.
    #[test]
    fn freshly_claimed_issue_is_not_stuck() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 1_000_000;
        store.report_signal(&signal(), now).expect("report");
        let fp = store
            .claim_ready_issue(now, now + 10_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        assert!(!store.stuck(&fp, now + 100_000, 300_000).expect("stuck"));
    }

    /// 崩溃重启自愈：fixing 且 owner_runner_id 非空的单在 runner 启动时批量
    /// 回 new（清冷却），claim 才能重领；revive 后再次 claim 不再复活别的单。
    #[test]
    fn revive_orphaned_fixing_requeues_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let now = 1_000_000;
        // 两张单都会被第一轮 claim 变成 fixing（各自 cooldown 不同 fp）。
        store.report_signal(&signal(), now).expect("report");
        let fp1 = store
            .claim_ready_issue(now, now + 10_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        store
            .report_signal(&issue_signal("second panic", "Second"), now + 1000)
            .expect("report");
        let fp2 = store
            .claim_ready_issue(now + 1000, now + 11_000)
            .expect("claim2")
            .expect("issue2")
            .fingerprint;
        assert_ne!(fp1, fp2);

        // 重启自愈：全部 fixing 回 new。
        let revived = store
            .revive_orphaned_fixing(now + 60_000, "host:repo")
            .expect("revive");
        assert_eq!(revived, 2);

        // 复活后的单可被重新 claim（cooldown 已清）。
        let again = store
            .claim_ready_issue(now + 61_000, now + 71_000)
            .expect("reclaim")
            .expect("reclaimed");
        assert_eq!(again.fingerprint, fp1);

        // 已是 new 的单不再被 revive（幂等）。
        let revived2 = store
            .revive_orphaned_fixing(now + 62_000, "host:repo")
            .expect("revive2");
        assert_eq!(revived2, 1);
    }

    #[test]
    fn requeue_blocks_healthy_and_forces_when_requested() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let old = 1_000_000;
        store.report_signal(&signal(), old).expect("report");
        let fp = store
            .claim_ready_issue(old, old + 10_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        store
            .set_state_as(&fp, State::Fixing, old + 10, Some("repair"), "runner")
            .expect("fixing");
        // owner 尚未建立：超过阈值后才允许非 force requeue。
        assert!(store.requeue_stuck(&fp, false, old + 20, 300_000).is_err());
        assert!(store
            .requeue_stuck(&fp, false, old + 400_000, 300_000)
            .is_ok());
        assert!(store.ready_issue(old + 400_001).expect("ready").is_some());

        // 重新领取并建立一个健康 owner。
        let fp = store
            .claim_ready_issue(old + 400_002, old + 500_000)
            .expect("claim2")
            .expect("issue")
            .fingerprint;
        store
            .set_state_as(&fp, State::Fixing, old + 400_010, Some("repair"), "runner")
            .expect("fixing2");
        store
            .start_run(&RunStart {
                id: "run2".into(),
                fingerprint: fp.clone(),
                runner_id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                started_at: old + 400_011,
                provider: None,
                model: None,
            })
            .expect("run");
        store
            .upsert_runner(&RunnerUpsert {
                id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                host: "host".into(),
                pid: Some(1),
                started_at: old,
                heartbeat_at: old + 400_020,
                status: "working".into(),
                provider: None,
                model: None,
                daily_pr_limit: Some(5),
                repo_path: None,
                service_unit: None,
                config_json: "{}".into(),
            })
            .expect("runner");
        store
            .heartbeat_runner(
                "host:repo",
                &RunnerHeartbeat {
                    heartbeat_at: old + 400_020,
                    status: "working".into(),
                    current_fingerprint: Some(fp.clone()),
                    phase: Some("repair".into()),
                    config_json: None,
                    last_error: None,
                },
            )
            .expect("heartbeat");
        assert!(store
            .requeue_stuck(&fp, false, old + 400_021, 300_000)
            .is_err());
        store
            .requeue_stuck(&fp, true, old + 400_022, 300_000)
            .expect("force");
        assert!(store.ready_issue(old + 400_023).expect("ready").is_some());
        let events = store.events_for(&fp, 20).expect("events");
        assert!(events.iter().any(|e| e.kind == "requeued"));
    }

    #[test]
    fn reopen_issue_allows_escalated_and_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t = 1_000_000;
        store.report_signal(&signal(), t).expect("report");
        let fp = store
            .ready_issue(t)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store.set_state(&fp, State::Triggered, t, None).expect("t");
        store.set_state(&fp, State::Fixing, t, None).expect("f");
        store.set_state(&fp, State::Escalated, t, None).expect("e");
        store.reopen_issue(&fp, t + 1, None).expect("reopen");
        assert!(store.ready_issue(t + 2).expect("ready").is_some());
        assert!(
            store.reopen_issue(&fp, t + 3, None).is_err(),
            "new is not reopenable"
        );
    }

    #[test]
    fn mark_interrupted_closes_open_runs_for_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run2".into(),
                fingerprint: fp.clone(),
                runner_id: "host:repo".into(),
                repo: "example.com/spike-target".into(),
                started_at: 100,
                provider: None,
                model: None,
            })
            .expect("start");
        assert_eq!(store.mark_interrupted("host:repo", 300).expect("mark"), 1);
        let runs = store.runs_for(&fp, 10).expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].outcome.as_deref(), Some("interrupted"));
    }

    #[test]
    fn utc_day_formats_epoch_days() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(86_400_000), "1970-01-02");
    }

    #[test]
    fn dedup_counts_signal_daily_without_report_event_spam() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_700_000_000_000;
        store.report_signal(&signal(), t0).expect("first");
        store.report_signal(&signal(), t0 + 1_000).expect("dedup");
        store.report_signal(&signal(), t0 + 2_000).expect("dedup");

        let fp = store
            .ready_issue(t0)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        let day = utc_day(t0);
        assert_eq!(store.signal_reports_on(&fp, &day).expect("count"), 3);

        let reported = store
            .events_for(&fp, 100)
            .expect("events")
            .into_iter()
            .filter(|e| e.kind == "reported")
            .count();
        assert_eq!(reported, 1, "only the first sighting writes a report event");
    }

    #[test]
    fn start_run_requires_an_existing_issue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let err = store
            .start_run(&RunStart {
                id: "missing-run".into(),
                fingerprint: "does-not-exist".into(),
                runner_id: "r".into(),
                repo: "repo".into(),
                started_at: 1,
                provider: None,
                model: None,
            })
            .expect_err("missing issue must fail");
        assert!(matches!(err, ShanError::UnknownIssue(_)));
        assert!(store
            .runs_for("does-not-exist", 10)
            .expect("runs")
            .is_empty());
    }

    #[test]
    fn mark_interrupted_preserves_existing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run-err".into(),
                fingerprint: fp.clone(),
                runner_id: "r".into(),
                repo: "example.com/spike-target".into(),
                started_at: 1,
                provider: None,
                model: None,
            })
            .expect("start");
        store
            .update_run(
                "run-err",
                &RunUpdate {
                    error: Some("provider exploded".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("update");
        assert_eq!(store.mark_interrupted("r", 2).expect("mark"), 1);
        let run = store.latest_run(&fp).expect("latest").expect("run");
        assert_eq!(run.outcome.as_deref(), Some("interrupted"));
        assert_eq!(run.error.as_deref(), Some("provider exploded"));
    }

    #[test]
    fn update_run_rejects_finished_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run-done".into(),
                fingerprint: fp,
                runner_id: "r".into(),
                repo: "example.com/spike-target".into(),
                started_at: 1,
                provider: None,
                model: None,
            })
            .expect("start");
        store
            .finish_run(
                "run-done",
                &RunFinish {
                    finished_at: 2,
                    outcome: "success".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("finish");
        assert!(store
            .update_run(
                "run-done",
                &RunUpdate {
                    phase: Some("ci".into()),
                    ..RunUpdate::default()
                },
            )
            .is_err());
    }

    #[test]
    fn re_upserting_a_runner_resets_live_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let base = RunnerUpsert {
            id: "host:repo".into(),
            repo: "r".into(),
            host: "host".into(),
            pid: Some(1),
            started_at: 1,
            heartbeat_at: 1,
            status: "starting".into(),
            provider: None,
            model: None,
            daily_pr_limit: Some(5),
            repo_path: None,
            service_unit: None,
            config_json: "{}".into(),
        };
        store.upsert_runner(&base).expect("first");
        store
            .heartbeat_runner(
                "host:repo",
                &RunnerHeartbeat {
                    heartbeat_at: 2,
                    status: "working".into(),
                    current_fingerprint: Some("fp".into()),
                    phase: Some("repair".into()),
                    config_json: Some("{\"v\":1}".into()),
                    last_error: Some("boom".into()),
                },
            )
            .expect("heartbeat");
        store
            .upsert_runner(&RunnerUpsert {
                config_json: "{\"v\":2}".into(),
                ..base
            })
            .expect("second");
        let row = store.list_runners().expect("list").pop().expect("runner");
        assert_eq!(row.status, "starting");
        assert_eq!(row.current_fingerprint, None);
        assert_eq!(row.phase, None);
        assert_eq!(row.last_error, None);
        assert_eq!(row.config_json, "{\"v\":2}");
    }

    #[test]
    fn set_state_as_clears_owner_and_stamps_time_on_new() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run-owner".into(),
                fingerprint: fp.clone(),
                runner_id: "runner-1".into(),
                repo: "example.com/spike-target".into(),
                started_at: 1,
                provider: None,
                model: None,
            })
            .expect("start");
        store
            .set_state_as(&fp, State::Triggered, 2, None, "runner")
            .expect("triggered");
        store
            .set_state_as(&fp, State::Fixing, 3, None, "runner")
            .expect("fixing");
        store
            .set_state_as(&fp, State::Escalated, 4, None, "runner")
            .expect("escalated");
        store
            .set_state_as(&fp, State::New, 5, Some("reopen"), "owner")
            .expect("new");
        let conn = store.lock();
        let (owner, changed): (Option<String>, i64) = conn
            .query_row(
                "SELECT owner_runner_id, state_changed_at FROM issues WHERE fingerprint=?1",
                [&fp],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("issue row");
        assert_eq!(owner, None);
        assert_eq!(changed, 5);
    }

    #[test]
    fn list_issues_exposes_lifecycle_owner_and_ci_verdict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_700_000_000_000;
        store.report_signal(&signal(), t0).expect("report");
        let fp = store
            .claim_ready_issue(t0 + 10, t0 + 1_000_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        store
            .start_run(&RunStart {
                id: "run-1".into(),
                fingerprint: fp.clone(),
                runner_id: "runner-1".into(),
                repo: "example.com/spike-target".into(),
                started_at: t0 + 20,
                provider: None,
                model: None,
            })
            .expect("start");
        store
            .update_run(
                "run-1",
                &RunUpdate {
                    ci_verdict: Some("success".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("update");

        let row = store.list_issues().expect("issues").pop().expect("issue");
        assert_eq!(row.fingerprint, fp);
        assert_eq!(row.state, "triggered");
        assert_eq!(row.first_seen, t0);
        assert_eq!(row.last_seen, t0);
        assert_eq!(row.state_changed_at, t0 + 10);
        assert_eq!(row.owner_runner_id.as_deref(), Some("runner-1"));
        assert_eq!(row.ci_verdict.as_deref(), Some("success"));
    }

    #[test]
    fn issue_ci_verdict_comes_from_latest_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_700_000_000_000;
        store.report_signal(&signal(), t0).expect("report");
        let fp = store
            .claim_ready_issue(t0 + 10, t0 + 1_000_000)
            .expect("claim")
            .expect("issue")
            .fingerprint;
        for (id, started_at) in [("run-a", t0 + 20), ("run-b", t0 + 40)] {
            store
                .start_run(&RunStart {
                    id: id.into(),
                    fingerprint: fp.clone(),
                    runner_id: format!("{id}-runner"),
                    repo: "example.com/spike-target".into(),
                    started_at,
                    provider: None,
                    model: None,
                })
                .expect("start");
        }
        store
            .update_run(
                "run-a",
                &RunUpdate {
                    ci_verdict: Some("success".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("ci a");
        store
            .finish_run(
                "run-a",
                &RunFinish {
                    finished_at: t0 + 30,
                    outcome: "success".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("finish a");
        store
            .update_run(
                "run-b",
                &RunUpdate {
                    ci_verdict: Some("failure".into()),
                    ..RunUpdate::default()
                },
            )
            .expect("ci b");

        let row = store.list_issues().expect("issues").pop().expect("issue");
        assert_eq!(
            row.ci_verdict.as_deref(),
            Some("failure"),
            "newest run wins"
        );
        assert_eq!(row.owner_runner_id.as_deref(), Some("run-b-runner"));
    }

    #[test]
    fn issue_row_serializes_ms_suffixed_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .report_signal(&signal(), 1_700_000_000_000)
            .expect("report");
        let row = store.list_issues().expect("issues").pop().expect("issue");
        let value = serde_json::to_value(&row).expect("json");
        for key in ["first_seen_ms", "last_seen_ms", "state_changed_at_ms"] {
            assert!(value.get(key).is_some(), "missing {key} in {value}");
        }
        for key in ["first_seen", "last_seen", "state_changed_at"] {
            assert!(value.get(key).is_none(), "{key} must stay renamed");
        }
        assert!(value.get("owner_runner_id").is_some());
        assert!(value.get("ci_verdict").is_some());
    }

    #[test]
    fn active_runs_excludes_finished_and_orders_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        for (id, started_at) in [("run-old", 100), ("run-new", 200)] {
            store
                .start_run(&RunStart {
                    id: id.into(),
                    fingerprint: fp.clone(),
                    runner_id: "runner-1".into(),
                    repo: "example.com/spike-target".into(),
                    started_at,
                    provider: None,
                    model: None,
                })
                .expect("start");
        }
        let ids: Vec<String> = store
            .active_runs()
            .expect("active")
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids, ["run-new", "run-old"]);

        store
            .finish_run(
                "run-new",
                &RunFinish {
                    finished_at: 300,
                    outcome: "success".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("finish");
        let ids: Vec<String> = store
            .active_runs()
            .expect("active")
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids, ["run-old"], "finished runs leave the active view");

        store
            .finish_run(
                "run-old",
                &RunFinish {
                    finished_at: 400,
                    outcome: "success".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("finish");
        assert!(store.active_runs().expect("active").is_empty());
    }

    #[test]
    fn ready_issues_mirrors_claim_priority_without_claiming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let low = Signal {
            severity: "low".into(),
            ..issue_signal("low", "Load")
        };
        let critical = Signal {
            severity: "critical".into(),
            ..issue_signal("critical", "Submit")
        };
        let fp_low = fingerprint::compute(&low.repo, "go", None, &low.stack_trace);
        store.report_signal(&low, 1).expect("low");
        store.report_signal(&low, 2).expect("low again");
        store.report_signal(&critical, 1).expect("critical");

        let ready = store.ready_issues(3, 10).expect("ready");
        let severities: Vec<&str> = ready.iter().map(|i| i.severity.as_str()).collect();
        assert_eq!(severities, ["critical", "low"]);
        assert_eq!(store.ready_issues(3, 1).expect("limited").len(), 1);
        assert_eq!(store.count_ready(3).expect("count"), 2);
        assert!(
            store
                .list_issues()
                .expect("issues")
                .iter()
                .all(|i| i.state == "new"),
            "observing the queue must not flip states"
        );

        let claimed = store
            .claim_ready_issue(3, 1_000_000)
            .expect("claim")
            .expect("issue");
        assert_eq!(
            claimed.severity, "critical",
            "observation left priority intact"
        );
        assert_eq!(store.count_ready(3).expect("count"), 1);
        let remaining: Vec<String> = store
            .ready_issues(3, 10)
            .expect("ready")
            .into_iter()
            .map(|i| i.fingerprint)
            .collect();
        assert_eq!(remaining, [fp_low], "the unclaimed issue is still ready");
    }

    #[test]
    fn count_ready_honours_cooldown_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store.set_cooldown(&fp, 1_000).expect("cooldown");

        assert_eq!(store.count_ready(999).expect("count"), 0);
        assert!(store.ready_issues(999, 10).expect("ready").is_empty());
        assert_eq!(store.count_ready(1_000).expect("count"), 1);
        assert_eq!(store.ready_issues(1_000, 10).expect("ready").len(), 1);
    }

    #[test]
    fn count_events_since_filters_kinds_and_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_000_000;
        store
            .record_event("fp", t0 - 10, Some("r"), "system", "reported", "old")
            .expect("old");
        store
            .record_event("fp", t0 + 5, Some("r"), "system", "ci_green", "a")
            .expect("a");
        store
            .record_event("fp", t0 + 15, Some("r"), "system", "ci_green", "b")
            .expect("b");
        store
            .record_event("fp", t0 + 20, Some("r"), "system", "ci_red", "c")
            .expect("c");

        assert_eq!(
            store.count_events_since(&["ci_green"], t0).expect("green"),
            2
        );
        assert_eq!(
            store
                .count_events_since(&["ci_green", "ci_red"], t0)
                .expect("both"),
            3
        );
        assert_eq!(
            store
                .count_events_since(&["ci_green"], t0 + 10)
                .expect("window"),
            1
        );
        assert_eq!(store.count_events_since(&[], 0).expect("empty"), 0);
        assert_eq!(
            store
                .count_events_since(&["ci_green' OR 1=1 --"], 0)
                .expect("injection-shaped kind"),
            0,
            "kind values stay bound parameters"
        );
    }

    #[test]
    fn latest_event_returns_newest_for_fingerprint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        // Insert out of timestamp order to prove ordering is by ts, not row id.
        store
            .record_event("fp", 5, Some("r"), "runner", "state_changed", "late")
            .expect("late");
        store
            .record_event("fp", 1, Some("r"), "system", "reported", "early")
            .expect("early");
        store
            .record_event("other", 9, Some("r"), "system", "reported", "other")
            .expect("other");

        let event = store.latest_event("fp").expect("latest").expect("event");
        assert_eq!(event.ts, 5);
        assert_eq!(event.kind, "state_changed");
        assert_eq!(event.detail, "late");
        assert_eq!(event.repo, "r");
        assert!(store.latest_event("missing").expect("latest").is_none());
    }

    #[test]
    fn reports_on_sums_signal_daily_across_issues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_700_000_000_000;
        let day_ms = 86_400_000;
        store.report_signal(&signal(), t0).expect("a1");
        store.report_signal(&signal(), t0 + 1).expect("a2");
        store
            .report_signal(&issue_signal("panic in Submit", "Submit"), t0 + 2)
            .expect("b");
        store
            .report_signal(&issue_signal("panic in Load", "Load"), t0 + day_ms)
            .expect("next day");

        assert_eq!(store.reports_on(&utc_day(t0)).expect("today"), 3);
        assert_eq!(
            store.reports_on(&utc_day(t0 + day_ms)).expect("tomorrow"),
            1
        );
        assert_eq!(store.reports_on("1999-01-01").expect("empty"), 0);
    }

    #[test]
    fn repo_aware_counts_scope_reports_and_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t0 = 1_700_000_000_000;
        let app_repo = "example.com/spike-target";
        let other_repo = "example.com/other";

        let app_fp = match store.report_signal(&signal(), t0).expect("app report") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected a new issue, got {other:?}"),
        };
        store.report_signal(&signal(), t0 + 1).expect("app repeat");
        let mut other_signal = issue_signal("panic in Other", "Other");
        other_signal.repo = other_repo.to_string();
        let other_fp = match store
            .report_signal(&other_signal, t0 + 2)
            .expect("other report")
        {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected a new issue, got {other:?}"),
        };

        let day = utc_day(t0);
        assert_eq!(store.reports_on_repo(&day, None).expect("all"), 3);
        assert_eq!(store.reports_on_repo(&day, Some(app_repo)).expect("app"), 2);
        assert_eq!(
            store
                .reports_on_repo(&day, Some(other_repo))
                .expect("other"),
            1
        );
        assert_eq!(
            store
                .reports_on_repo(&day, Some("example.com/none"))
                .expect("none"),
            0
        );

        store
            .record_event(&app_fp, t0 - 1, Some(app_repo), "runner", "ci_green", "old")
            .expect("old app event");
        store
            .record_event(&app_fp, t0, Some(app_repo), "runner", "ci_green", "app")
            .expect("app event");
        store
            .record_event(
                &other_fp,
                t0,
                Some(other_repo),
                "runner",
                "ci_green",
                "other",
            )
            .expect("other event");
        // Legacy rows written before events.repo existed fall back to the
        // owning issue's repo.
        store
            .record_event(&other_fp, t0, None, "runner", "ci_green", "legacy")
            .expect("legacy event");

        assert_eq!(
            store
                .count_events_since_repo(&["ci_green"], t0, None)
                .expect("all"),
            3
        );
        assert_eq!(
            store
                .count_events_since_repo(&["ci_green"], t0, Some(app_repo))
                .expect("app"),
            1
        );
        assert_eq!(
            store
                .count_events_since_repo(&["ci_green"], t0, Some(other_repo))
                .expect("other"),
            2,
            "NULL-repo events belong to their issue's repo"
        );
        assert_eq!(
            store
                .count_events_since_repo(&["ci_red"], t0, None)
                .expect("no red"),
            0
        );
        assert_eq!(
            store
                .count_events_since_repo(&[], t0, None)
                .expect("empty kinds"),
            0
        );
    }

    #[test]
    fn similar_issues_matches_title_and_excludes_self() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let sig_a = issue_signal("panic in Average", "Average");
        let sig_b = issue_signal("panic in Average", "Submit");
        let sig_d = issue_signal("panic in Average", "Load");
        let sig_c = issue_signal("panic in Load", "Enqueue");
        let fp_a = fingerprint::compute(&sig_a.repo, "go", None, &sig_a.stack_trace);
        let fp_b = fingerprint::compute(&sig_b.repo, "go", None, &sig_b.stack_trace);
        let fp_d = fingerprint::compute(&sig_d.repo, "go", None, &sig_d.stack_trace);
        store.report_signal(&sig_a, 1).expect("a");
        store.report_signal(&sig_b, 2).expect("b");
        store.report_signal(&sig_d, 3).expect("d");
        store.report_signal(&sig_c, 4).expect("c");

        let similar = store
            .similar_issues("panic in Average", &fp_a, 10)
            .expect("similar");
        let fps: Vec<&str> = similar.iter().map(|i| i.fingerprint.as_str()).collect();
        assert_eq!(fps, [fp_d.as_str(), fp_b.as_str()]);
        assert!(similar.iter().all(|i| i.title == "panic in Average"));

        let limited = store
            .similar_issues("panic in Average", &fp_a, 1)
            .expect("similar limited");
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].fingerprint, fp_d);

        // The strip is capped, the count is the whole same-title population.
        assert_eq!(
            store
                .similar_count("panic in Average", &fp_a)
                .expect("similar count"),
            2
        );
        assert_eq!(
            store
                .similar_count("panic in Average", &fp_d)
                .expect("similar count excluding newest"),
            2
        );
        assert_eq!(
            store
                .similar_count("panic in Nonexistent", &fp_a)
                .expect("empty count"),
            0
        );

        let excluding_newest = store
            .similar_issues("panic in Average", &fp_d, 10)
            .expect("similar");
        let fps: Vec<&str> = excluding_newest
            .iter()
            .map(|i| i.fingerprint.as_str())
            .collect();
        assert_eq!(fps, [fp_b.as_str(), fp_a.as_str()]);

        assert!(store
            .similar_issues("panic in Nonexistent", &fp_a, 10)
            .expect("none")
            .is_empty());
    }

    #[test]
    fn list_events_filters_kinds_repo_fingerprint_and_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let repo_a = "example.com/spike-target";
        let repo_b = "example.com/other";
        let sig_a = signal();
        let sig_b = issue_signal("panic in Submit", "Submit");
        let fp_a = fingerprint::compute(&sig_a.repo, "go", None, &sig_a.stack_trace);
        let fp_b = fingerprint::compute(&sig_b.repo, "go", None, &sig_b.stack_trace);
        // Both issues exist so the legacy NULL-repo event can resolve its repo
        // through the issue join; their `reported` events sit at ts=1, outside
        // every window below.
        store.report_signal(&sig_a, 1).expect("issue a");
        store.report_signal(&sig_b, 1).expect("issue b");

        store
            .record_event(&fp_a, 1_000, Some(repo_a), "system", "ci_green", "g1")
            .expect("g1");
        store
            .record_event(&fp_b, 1_500, Some(repo_a), "system", "ci_green", "g2")
            .expect("g2");
        store
            .record_event(&fp_a, 1_600, Some(repo_a), "system", "ci_red", "r1")
            .expect("r1");
        store
            .record_event(&fp_a, 1_700, None, "system", "reported", "legacy")
            .expect("legacy");
        store
            .record_event(&fp_b, 1_800, Some(repo_b), "system", "ci_red", "elsewhere")
            .expect("other repo");
        store
            .record_event(&fp_b, 2_000, Some(repo_a), "system", "reported", "rep")
            .expect("rep");
        store
            .record_event(&fp_a, 2_001, Some(repo_a), "system", "ci_green", "after")
            .expect("after");
        store
            .record_event(&fp_a, 999, Some(repo_a), "system", "ci_green", "before")
            .expect("before");

        // Kind filter, newest first.
        let kinds = EventQuery {
            kinds: vec!["ci_red".into(), "ci_green".into()],
            ..EventQuery::default()
        };
        assert_eq!(
            store
                .list_events(&kinds, 10)
                .expect("kinds")
                .iter()
                .map(|e| e.ts)
                .collect::<Vec<_>>(),
            [2_001, 1_800, 1_600, 1_500, 1_000, 999]
        );

        // from/to are inclusive on both ends.
        let windowed = EventQuery {
            from_ms: Some(1_000),
            to_ms: Some(2_000),
            ..EventQuery::default()
        };
        assert_eq!(
            store
                .list_events(&windowed, 10)
                .expect("window")
                .iter()
                .map(|e| e.ts)
                .collect::<Vec<_>>(),
            [2_000, 1_800, 1_700, 1_600, 1_500, 1_000]
        );

        let by_repo = EventQuery {
            repo: Some(repo_a.to_string()),
            from_ms: Some(1_000),
            to_ms: Some(2_000),
            ..EventQuery::default()
        };
        let rows = store.list_events(&by_repo, 10).expect("repo");
        assert_eq!(
            rows.iter().map(|e| e.ts).collect::<Vec<_>>(),
            [2_000, 1_700, 1_600, 1_500, 1_000]
        );
        let legacy = rows.iter().find(|e| e.ts == 1_700).expect("legacy row");
        assert_eq!(
            legacy.repo, repo_a,
            "NULL event.repo falls back to the issue repo"
        );

        let other_repo = EventQuery {
            repo: Some(repo_b.to_string()),
            from_ms: Some(1_000),
            to_ms: Some(2_000),
            ..EventQuery::default()
        };
        assert_eq!(
            store
                .list_events(&other_repo, 10)
                .expect("other repo")
                .iter()
                .map(|e| e.ts)
                .collect::<Vec<_>>(),
            [1_800]
        );

        let by_fingerprint = EventQuery {
            fingerprint: Some(fp_b.clone()),
            from_ms: Some(1_000),
            to_ms: Some(2_000),
            ..EventQuery::default()
        };
        assert_eq!(
            store
                .list_events(&by_fingerprint, 10)
                .expect("fingerprint")
                .iter()
                .map(|e| e.ts)
                .collect::<Vec<_>>(),
            [2_000, 1_800, 1_500]
        );

        let mut limited = by_repo.clone();
        limited.kinds = Vec::new();
        let page = store.list_events(&limited, 2).expect("limit");
        assert_eq!(
            page.iter().map(|e| e.ts).collect::<Vec<_>>(),
            [2_000, 1_700]
        );
        assert!(store
            .list_events(&EventQuery::default(), 0)
            .expect("zero limit")
            .is_empty());

        let injection = EventQuery {
            kinds: vec!["ci_green' OR 1=1 --".into()],
            ..EventQuery::default()
        };
        assert!(
            store
                .list_events(&injection, 10)
                .expect("bound kind")
                .is_empty(),
            "kind values stay bound parameters"
        );
    }

    #[test]
    fn list_events_orders_by_ts_then_id_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .record_event("fp-a", 100, Some("r"), "system", "reported", "first")
            .expect("a");
        store
            .record_event("fp-b", 300, Some("r"), "system", "ci_green", "green")
            .expect("b");
        store
            .record_event("fp-c", 300, Some("r"), "system", "ci_red", "red")
            .expect("c");
        store
            .record_event("fp-d", 200, Some("r"), "system", "rejected", "no")
            .expect("d");

        let rows = store
            .list_events(&EventQuery::default(), 10)
            .expect("events");
        assert_eq!(
            rows.iter().map(|e| e.ts).collect::<Vec<_>>(),
            [300, 300, 200, 100]
        );
        assert_eq!(rows[0].kind, "ci_red", "same ts → newest row id first");
        assert_eq!(rows[1].kind, "ci_green");
    }

    #[test]
    fn list_events_caps_limit_at_5000() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        {
            let mut conn = store.lock();
            let tx = conn.transaction().expect("tx");
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO events(fingerprint, ts, repo, actor, kind, detail)
                         VALUES ('fp', ?1, 'example.com/r', 'system', 'reported', 'bulk')",
                    )
                    .expect("prepare");
                for ts in 1..=5_001 {
                    stmt.execute([ts]).expect("insert");
                }
            }
            tx.commit().expect("commit");
        }

        let rows = store
            .list_events(&EventQuery::default(), 10_000)
            .expect("capped");
        assert_eq!(rows.len(), 5_000);
        assert_eq!(rows[0].ts, 5_001);
        assert_eq!(rows[4_999].ts, 2);
    }

    #[test]
    fn trends_fill_each_source_and_apply_repo_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let repo_a = "example.com/spike-target";
        let repo_b = "example.com/other";
        let day_ms = 86_400_000i64;
        let now = 1_700_000_000_000i64;
        let today = now - now.rem_euclid(day_ms);
        let d0 = today - 2 * day_ms;
        let d1 = today - day_ms;
        let d2 = today;
        let before = d0 - day_ms;
        let at = |day: i64, hour: i64| day + hour * 3_600_000;

        let sig_a = signal();
        let sig_b = issue_signal("panic in Submit", "Submit");
        let mut sig_c = issue_signal("panic in Load", "Load");
        sig_c.repo = repo_b.to_string();
        let mut sig_d = issue_signal("panic in Enqueue", "Enqueue");
        sig_d.repo = repo_b.to_string();
        let sig_old = issue_signal("panic in Archive", "Archive");
        let fp_a = fingerprint::compute(&sig_a.repo, "go", None, &sig_a.stack_trace);
        let fp_c = fingerprint::compute(&sig_c.repo, "go", None, &sig_c.stack_trace);

        // Reports also create the issues that own them; the old issue and its
        // report fall outside the 3-day window.
        store.report_signal(&sig_a, at(d0, 1)).expect("a d0");
        store.report_signal(&sig_a, at(d1, 1)).expect("a d1");
        store.report_signal(&sig_a, at(d1, 2)).expect("a d1 again");
        store.report_signal(&sig_b, at(d1, 3)).expect("b d1");
        store.report_signal(&sig_c, at(d1, 4)).expect("c d1");
        store.report_signal(&sig_d, at(d2, 1)).expect("d d2");
        store.report_signal(&sig_a, at(d2, 2)).expect("a d2");
        store
            .report_signal(&sig_old, at(before, 3))
            .expect("old issue");

        // Tokens bucket by run start; PRs opened by pr_opened_at.
        seed_run(
            &store,
            "run-1",
            &fp_a,
            repo_a,
            at(d0, 5),
            Some(at(d1, 5)),
            (100, 50),
        );
        seed_run(
            &store,
            "run-2",
            &fp_a,
            repo_a,
            at(d1, 5),
            Some(at(d2, 5)),
            (200, 100),
        );
        seed_run(&store, "run-3", &fp_c, repo_b, at(d2, 5), None, (50, 10));
        seed_run(
            &store,
            "run-old",
            &fp_a,
            repo_a,
            at(before, 5),
            Some(at(before, 6)),
            (999, 999),
        );

        // Window is [d0 00:00, d2+1 00:00): the exact start is counted, the
        // exact end and earlier days are not.
        store
            .record_event(
                &fp_c,
                d0,
                Some(repo_b),
                "system",
                "ci_green",
                "window start",
            )
            .expect("start");
        store
            .record_event(&fp_c, at(d0, 6), Some(repo_b), "system", "ci_red", "d0 red")
            .expect("d0 red");
        store
            .record_event(
                &fp_a,
                at(d1, 6),
                Some(repo_a),
                "system",
                "merged",
                "d1 merge",
            )
            .expect("d1 merge");
        store
            .record_event(
                &fp_c,
                at(d1, 7),
                Some(repo_b),
                "system",
                "rejected",
                "d1 reject",
            )
            .expect("d1 reject");
        store
            .record_event(
                &fp_a,
                at(d2, 6),
                Some(repo_a),
                "system",
                "ci_green",
                "d2 green",
            )
            .expect("d2 green");
        store
            .record_event(
                &fp_a,
                at(d2, 7),
                Some(repo_a),
                "system",
                "ci_green",
                "d2 green 2",
            )
            .expect("d2 green 2");
        store
            .record_event(
                &fp_a,
                at(d2, 8),
                None,
                "system",
                "ci_green",
                "legacy null repo",
            )
            .expect("legacy green");
        store
            .record_event(
                &fp_a,
                at(d1, 8),
                Some(repo_a),
                "system",
                "state_changed",
                "ignored",
            )
            .expect("ignored kind");
        store
            .record_event(
                &fp_a,
                at(before, 6),
                Some(repo_a),
                "system",
                "merged",
                "before",
            )
            .expect("before");
        store
            .record_event(
                &fp_a,
                d2 + day_ms,
                Some(repo_a),
                "system",
                "merged",
                "window end",
            )
            .expect("window end");

        let expected_all = vec![
            TrendBucket {
                day: utc_day(d0),
                reports: 1,
                new_issues: 1,
                tokens: 150,
                ci_green: 1,
                ci_red: 1,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d1),
                reports: 4,
                new_issues: 2,
                prs_opened: 1,
                merged: 1,
                rejected: 1,
                tokens: 300,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d2),
                reports: 2,
                new_issues: 1,
                prs_opened: 1,
                tokens: 60,
                ci_green: 3,
                ..TrendBucket::default()
            },
        ];
        assert_eq!(store.trends(3, None, now).expect("all repos"), expected_all);

        let expected_a = vec![
            TrendBucket {
                day: utc_day(d0),
                reports: 1,
                new_issues: 1,
                tokens: 150,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d1),
                reports: 3,
                new_issues: 1,
                prs_opened: 1,
                merged: 1,
                tokens: 300,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d2),
                reports: 1,
                prs_opened: 1,
                ci_green: 3,
                ..TrendBucket::default()
            },
        ];
        assert_eq!(
            store.trends(3, Some(repo_a), now).expect("repo a"),
            expected_a
        );

        let expected_b = vec![
            TrendBucket {
                day: utc_day(d0),
                ci_green: 1,
                ci_red: 1,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d1),
                reports: 1,
                new_issues: 1,
                rejected: 1,
                ..TrendBucket::default()
            },
            TrendBucket {
                day: utc_day(d2),
                reports: 1,
                new_issues: 1,
                tokens: 60,
                ..TrendBucket::default()
            },
        ];
        assert_eq!(
            store.trends(3, Some(repo_b), now).expect("repo b"),
            expected_b
        );
    }

    #[test]
    fn trends_zero_fill_missing_days_and_handle_zero_days() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day_ms = 86_400_000i64;
        let now = 1_700_000_000_000i64;
        let today = now - now.rem_euclid(day_ms);
        store
            .report_signal(&signal(), today + 3_600_000)
            .expect("today report");

        let buckets = store.trends(5, None, now).expect("trends");
        assert_eq!(buckets.len(), 5, "exactly `days` buckets");
        let days: Vec<String> = (0..5)
            .map(|offset| utc_day(today - (4 - offset) * day_ms))
            .collect();
        for (bucket, day) in buckets.iter().take(4).zip(&days[..4]) {
            assert_eq!(
                bucket,
                &TrendBucket {
                    day: day.clone(),
                    ..TrendBucket::default()
                },
                "missing days are zero-filled: {day}"
            );
        }
        assert_eq!(buckets[4].day, days[4]);
        assert_eq!(buckets[4].reports, 1);
        assert_eq!(buckets[4].new_issues, 1);

        assert!(store.trends(0, None, now).expect("zero days").is_empty());
    }

    #[test]
    fn trends_buckets_flip_at_utc_midnight() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day_ms = 86_400_000i64;
        let now = 1_700_000_000_123i64;
        let midnight = now - now.rem_euclid(day_ms);
        store
            .report_signal(&signal(), midnight)
            .expect("midnight report");

        let before = store
            .trends(2, None, midnight - 1)
            .expect("before midnight");
        assert_eq!(before[1].day, utc_day(midnight - day_ms));
        assert_eq!(before[1].reports, 0, "midnight belongs to the next day");

        let after = store.trends(2, None, midnight).expect("at midnight");
        assert_eq!(after[1].day, utc_day(midnight));
        assert_eq!(after[1].reports, 1);
        assert_eq!(after[1].new_issues, 1);
    }

    #[test]
    fn trends_count_legacy_issue_prs_without_runs_and_dedupe_run_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day_ms = 86_400_000i64;
        let now = 1_700_000_000_000i64;
        let today = now - now.rem_euclid(day_ms);
        let repo = "example.com/spike-target";
        let other = "example.com/other";
        let t_start = today + 3_600_000;
        let t_pr = today + 7_200_000;
        let url_dup = "https://github.com/acme/spike/pull/10";

        let sig = signal();
        let fp = fingerprint::compute(&sig.repo, "go", None, &sig.stack_trace);
        store.report_signal(&sig, t_start).expect("report");
        // Pre-migration fallback: only the issue row carries the PR.
        store.set_pr(&fp, url_dup, t_pr).expect("set pr");
        // The same PR is also on a run (and its issue): count it once.
        store
            .start_run(&RunStart {
                id: "run-dup".to_string(),
                fingerprint: fp.clone(),
                runner_id: "host:runner".to_string(),
                repo: repo.to_string(),
                started_at: t_start,
                provider: None,
                model: None,
            })
            .expect("start run");
        store
            .update_run(
                "run-dup",
                &RunUpdate {
                    pr_url: Some(url_dup.to_string()),
                    pr_opened_at: Some(t_pr),
                    ..RunUpdate::default()
                },
            )
            .expect("update run");

        // A legacy-only issue PR (no run) must still count.
        let sig_legacy = issue_signal("panic in Submit", "Submit");
        let fp_legacy = fingerprint::compute(&sig_legacy.repo, "go", None, &sig_legacy.stack_trace);
        store
            .report_signal(&sig_legacy, t_start)
            .expect("legacy report");
        store
            .set_pr(&fp_legacy, "https://github.com/acme/spike/pull/11", t_pr)
            .expect("legacy pr");

        // Another repo's PR must not leak into the repo filter.
        let mut sig_other = issue_signal("panic in Load", "Load");
        sig_other.repo = other.to_string();
        let fp_other = fingerprint::compute(&sig_other.repo, "go", None, &sig_other.stack_trace);
        store
            .report_signal(&sig_other, t_start)
            .expect("other report");
        store
            .set_pr(&fp_other, "https://github.com/acme/other/pull/1", t_pr)
            .expect("other pr");

        let buckets = store.trends(1, Some(repo), now).expect("trends");
        assert_eq!(buckets.len(), 1);
        assert_eq!(
            buckets[0].prs_opened, 2,
            "legacy issue PRs count and duplicates collapse by URL"
        );
        assert_eq!(
            store.prs_opened_since(repo, today).expect("quota"),
            2,
            "trends and the daily quota agree"
        );
        assert_eq!(
            store.trends(1, Some(other), now).expect("other")[0].prs_opened,
            1
        );
    }

    #[test]
    fn trends_count_terminal_events_journalled_by_sync_and_ci() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day_ms = 86_400_000i64;
        let now = 1_700_000_000_000i64;
        let today = now - now.rem_euclid(day_ms);
        let repo = "example.com/spike-target";
        let other = "example.com/other";

        // The kinds and actors the sync path (system) and the CI-red gate
        // (runner) now journal.
        store
            .record_event(
                "fp-merged",
                today + 3_600_000,
                Some(repo),
                "system",
                "merged",
                "https://github.com/acme/spike/pull/1",
            )
            .expect("merged");
        store
            .record_event(
                "fp-rejected",
                today + 7_200_000,
                Some(repo),
                "runner",
                "rejected",
                "ci_red: PR #2 closed without merge",
            )
            .expect("rejected");
        store
            .record_event(
                "fp-other",
                today + 1_800_000,
                Some(other),
                "system",
                "merged",
                "https://github.com/acme/other/pull/9",
            )
            .expect("other merged");

        let bucket = &store.trends(1, Some(repo), now).expect("trends")[0];
        assert_eq!(bucket.merged, 1);
        assert_eq!(bucket.rejected, 1);
        assert_eq!(bucket.ci_green, 0);
        assert_eq!(bucket.ci_red, 0);
        assert_eq!(
            store.trends(1, Some(other), now).expect("other")[0].merged,
            1
        );
        let all = &store.trends(1, None, now).expect("all")[0];
        assert_eq!(all.merged, 2);
        assert_eq!(all.rejected, 1);
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;

    #[test]
    fn heartbeat_upserts_and_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        assert_eq!(store.last_heartbeat().expect("read"), None);
        store.heartbeat(1_000).expect("beat");
        store.heartbeat(2_000).expect("beat again");
        assert_eq!(store.last_heartbeat().expect("read"), Some(2_000));
    }

    #[test]
    fn events_for_returns_per_issue_timeline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let trace = "panic: runtime error: index out of range [3] with length 3\n\
            goroutine 1 [running]:\n\
            example.com/spike-target.Average(...)\n\
            \t/app/calc.go:17 +0x8c";
        let sig = Signal {
            repo: "example.com/spike-target".into(),
            language: "go".into(),
            title: "panic in Average".into(),
            severity: "high".into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: trace.into(),
        };
        store.report_signal(&sig, 1).expect("report");
        let fp = store
            .ready_issue(1)
            .expect("ready")
            .expect("issue")
            .fingerprint;
        store
            .set_state(&fp, State::Triggered, 2, Some("claimed"))
            .expect("t");
        let events = store.events_for(&fp, 10).expect("events");
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"state_changed"), "{kinds:?}");
    }
}

#[cfg(test)]
mod runner_tests {
    use super::*;

    #[test]
    fn runner_upsert_heartbeat_and_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .upsert_runner(&RunnerUpsert {
                id: "host:repo".into(),
                repo: "example.com/r".into(),
                host: "host".into(),
                pid: Some(42),
                started_at: 100,
                heartbeat_at: 100,
                status: "starting".into(),
                provider: Some("zai".into()),
                model: Some("glm-5.3-flash".into()),
                daily_pr_limit: Some(5),
                repo_path: Some("/opt/nuwa/repo".into()),
                service_unit: Some("nuwa-work".into()),
                config_json: "{}".into(),
            })
            .expect("upsert");

        store
            .heartbeat_runner(
                "host:repo",
                &RunnerHeartbeat {
                    heartbeat_at: 200,
                    status: "working".into(),
                    current_fingerprint: Some("fp1".into()),
                    phase: Some("repair".into()),
                    config_json: Some("{\"max_turns\":30}".into()),
                    last_error: None,
                },
            )
            .expect("heartbeat");

        let runners = store.list_runners().expect("list");
        assert_eq!(runners.len(), 1);
        let r = &runners[0];
        assert_eq!(r.status, "working");
        assert_eq!(r.current_fingerprint.as_deref(), Some("fp1"));
        assert_eq!(r.phase.as_deref(), Some("repair"));
        assert_eq!(r.heartbeat_at, 200);
        assert!(r.config_json.contains("max_turns"));
        assert!(store
            .heartbeat_runner(
                "missing",
                &RunnerHeartbeat {
                    heartbeat_at: 1,
                    status: "idle".into(),
                    current_fingerprint: None,
                    phase: None,
                    config_json: None,
                    last_error: None,
                }
            )
            .is_err());
    }

    fn registration(pid: i64, at: i64) -> RunnerUpsert {
        RunnerUpsert {
            id: "host:repo".into(),
            repo: "example.com/r".into(),
            host: "host".into(),
            pid: Some(pid),
            started_at: at,
            heartbeat_at: at,
            status: "starting".into(),
            provider: Some("zai".into()),
            model: None,
            daily_pr_limit: Some(5),
            repo_path: Some("/opt/nuwa/repo".into()),
            service_unit: Some("nuwa-work".into()),
            config_json: "{}".into(),
        }
    }

    #[test]
    fn runner_lease_is_exclusive_renewable_and_recoverable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let at = 1_000;
        let ttl = 60;

        assert_eq!(
            store
                .try_acquire_runner_lease(&registration(1, at), "a", at, ttl)
                .expect("acquire a"),
            RunnerLeaseOutcome::Acquired
        );

        // A contender must neither take the lease nor overwrite the live row.
        assert_eq!(
            store
                .try_acquire_runner_lease(&registration(2, at + 10), "b", at + 10, ttl)
                .expect("contend b"),
            RunnerLeaseOutcome::HeldByRunner
        );
        let runner = store.list_runners().expect("list").pop().expect("runner");
        assert_eq!(
            runner.pid,
            Some(1),
            "a losing contender must not clobber the holder's row"
        );

        // Renewal extends the deadline; a non-holder cannot renew.
        assert!(store
            .renew_runner_lease("host:repo", "a", at + 50, ttl)
            .expect("renew a"));
        assert!(!store
            .renew_runner_lease("host:repo", "b", at + 50, ttl)
            .expect("renew b"));
        assert_eq!(
            store
                .try_acquire_runner_lease(&registration(2, at + 60), "b", at + 60, ttl)
                .expect("still held"),
            RunnerLeaseOutcome::HeldByRunner
        );

        // A crashed holder stops renewing; after the TTL another process takes over.
        let expired = at + 50 + ttl + 1;
        assert_eq!(
            store
                .try_acquire_runner_lease(&registration(2, expired), "b", expired, ttl)
                .expect("takeover"),
            RunnerLeaseOutcome::Acquired
        );
        assert!(!store
            .renew_runner_lease("host:repo", "a", expired + 1, ttl)
            .expect("stale holder"));

        // A clean shutdown releases the lease immediately.
        store
            .release_runner_lease("host:repo", "b")
            .expect("release");
        assert_eq!(
            store
                .try_acquire_runner_lease(&registration(3, expired + 2), "c", expired + 2, ttl)
                .expect("after release"),
            RunnerLeaseOutcome::Acquired
        );
    }

    #[test]
    fn a_checkout_can_only_have_one_live_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let at = 1_000;
        let ttl = 60;
        let mut first = registration(1, at);
        first.id = "host:repo-a".into();
        let mut second = registration(2, at);
        second.id = "host:repo-b".into();

        assert_eq!(
            store
                .try_acquire_runner_lease(&first, "a", at, ttl)
                .expect("first"),
            RunnerLeaseOutcome::Acquired
        );
        assert_eq!(
            store
                .try_acquire_runner_lease(&second, "b", at + 1, ttl)
                .expect("second"),
            RunnerLeaseOutcome::CheckoutInUse("host:repo-a".into())
        );

        // A distinct checkout is fine; an expired holder no longer blocks.
        second.repo_path = Some("/opt/nuwa/repo-b".into());
        assert_eq!(
            store
                .try_acquire_runner_lease(&second, "b", at + 2, ttl)
                .expect("distinct checkout"),
            RunnerLeaseOutcome::Acquired
        );
        let mut third = registration(3, at);
        third.id = "host:repo-c".into();
        assert_eq!(
            store
                .try_acquire_runner_lease(&third, "c", at + ttl + 3, ttl)
                .expect("expired checkout"),
            RunnerLeaseOutcome::Acquired
        );
    }
}

#[cfg(test)]
mod repo_tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        (dir, store)
    }

    fn signal_for(title: &str, symbol: &str, severity: &str) -> Signal {
        Signal {
            repo: "example.com/spike-target".into(),
            language: "go".into(),
            title: title.into(),
            severity: severity.into(),
            message: None,
            log_window: Vec::new(),
            stack_trace: format!(
                "panic: {title}\ngoroutine 1 [running]:\nexample.com/spike-target.{symbol}()\n\t/app/a.go:1 +0x1"
            ),
        }
    }

    #[test]
    fn repos_list_in_case_insensitive_name_order() {
        let (_dir, store) = store();
        store
            .insert_repo("example.com/zed", "zed", "https://example.com/zed", "", 1)
            .expect("zed");
        store
            .insert_repo(
                "example.com/beta",
                "Beta",
                "git@example.com:beta.git",
                "",
                2,
            )
            .expect("beta");
        store
            .insert_repo(
                "example.com/alpha2",
                "Alpha",
                "https://example.com/alpha2",
                "",
                3,
            )
            .expect("alpha2");
        store
            .insert_repo(
                "example.com/alpha",
                "alpha",
                "https://example.com/alpha",
                "",
                4,
            )
            .expect("alpha");

        let repos = store.list_repos().expect("list");
        let ids: Vec<&str> = repos.iter().map(|row| row.repo.as_str()).collect();
        assert_eq!(
            ids,
            [
                "example.com/alpha",
                "example.com/alpha2",
                "example.com/beta",
                "example.com/zed"
            ],
            "name NOCASE first, then identity"
        );
    }

    #[test]
    fn repo_crud_and_duplicate_identity() {
        let (_dir, store) = store();
        assert_eq!(store.get_repo("example.com/app").expect("get"), None);
        store
            .insert_repo(
                "example.com/app",
                "App",
                "https://example.com/app",
                "primary",
                100,
            )
            .expect("insert");

        let row = store
            .get_repo("example.com/app")
            .expect("get")
            .expect("row");
        assert_eq!(
            row,
            RepoRow {
                repo: "example.com/app".into(),
                name: "App".into(),
                url: "https://example.com/app".into(),
                description: "primary".into(),
                created_at: 100,
                updated_at: 100,
            }
        );

        match store.insert_repo("example.com/app", "Again", "https://x", "", 200) {
            Err(ShanError::RepoExists(repo)) => assert_eq!(repo, "example.com/app"),
            other => panic!("expected RepoExists, got {other:?}"),
        }

        assert!(store
            .update_repo(
                "example.com/app",
                "App v2",
                "git@example.com:app.git",
                "",
                200,
            )
            .expect("update"));
        let row = store
            .get_repo("example.com/app")
            .expect("get")
            .expect("row");
        assert_eq!(row.name, "App v2");
        assert_eq!(row.url, "git@example.com:app.git");
        assert_eq!(row.description, "");
        assert_eq!(row.created_at, 100, "created_at is immutable");
        assert_eq!(row.updated_at, 200);
        assert!(!store
            .update_repo("example.com/missing", "N", "U", "", 300)
            .expect("update missing"));

        assert!(store.delete_repo("example.com/app").expect("delete"));
        assert_eq!(store.get_repo("example.com/app").expect("get"), None);
        assert!(!store.delete_repo("example.com/app").expect("delete again"));
        assert!(store.list_repos().expect("list").is_empty());
    }

    #[test]
    fn repos_survive_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.db");
        {
            let store = Store::open(&path).expect("open");
            store
                .insert_repo("example.com/app", "App", "https://x", "", 1)
                .expect("insert");
        }
        let store = Store::open(&path).expect("reopen");
        assert_eq!(store.list_repos().expect("list").len(), 1);
    }

    #[test]
    fn ignore_mutes_reports_and_reopen_resets_attempts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t = 1_000_000;
        let signal = signal_for("panic in Average", "Average", "high");
        let fp = match store.report_signal(&signal, t).expect("report") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected new, got {other:?}"),
        };

        store
            .ignore_issue(&fp, "not a bug — expected upstream", "owner", t + 1)
            .expect("ignore");
        let row = store
            .list_issues()
            .expect("issues")
            .into_iter()
            .find(|row| row.fingerprint == fp)
            .expect("issue");
        assert_eq!(row.state, "ignored");

        let muted = store.report_signal(&signal, t + 2).expect("report");
        assert!(
            matches!(muted, ReportOutcome::Muted { .. }),
            "ignored reports must not reopen: {muted:?}"
        );

        assert_eq!(store.increment_attempts(&fp).expect("attempt"), 1);
        store
            .reopen_issue(&fp, t + 3, Some("retry after upstream deploy"))
            .expect("reopen");
        assert_eq!(store.attempts_for(&fp).expect("attempts"), 0);
        let event = store.latest_event(&fp).expect("event").expect("event");
        assert!(event.detail.contains("retry after upstream deploy"));
    }

    #[test]
    fn severity_dominates_occurrences_in_ready_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let t = 1_000_000;

        let medium = signal_for("medium noise", "MediumNoise", "medium");
        let medium_fp = match store.report_signal(&medium, t).expect("medium") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected new, got {other:?}"),
        };
        for i in 0..6 {
            store.report_signal(&medium, t + i).expect("medium repeat");
        }

        let critical = signal_for("critical bug", "CriticalBug", "critical");
        let critical_fp = match store.report_signal(&critical, t).expect("critical") {
            ReportOutcome::NewIssue { fingerprint } => fingerprint,
            other => panic!("expected new, got {other:?}"),
        };

        let picked = store.ready_issue(t).expect("ready").expect("issue");
        assert_eq!(picked.fingerprint, critical_fp);
        assert_ne!(picked.fingerprint, medium_fp);
        assert_eq!(picked.attempts, 0);
    }

    fn memory_entry(fingerprint: &str, recorded_at: i64) -> RepairMemoryEntry {
        RepairMemoryEntry {
            repo: "example.com/spike-target".to_string(),
            fingerprint: fingerprint.to_string(),
            commit_sha: format!("sha-{fingerprint}"),
            pr_url: Some(format!("https://github.com/acme/app/pull/{recorded_at}")),
            root_cause: "ROOT CAUSE: fixed".to_string(),
            change_stat: "1 file changed".to_string(),
            files: 1,
            recorded_at,
        }
    }

    #[test]
    fn repair_memory_keeps_latest_per_fingerprint_with_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .record_repair_memory(&memory_entry("fp-a", 10))
            .expect("first");
        let mut newer = memory_entry("fp-a", 20);
        newer.commit_sha = "sha-a-2".to_string();
        store.record_repair_memory(&newer).expect("upsert");
        store
            .record_repair_memory(&memory_entry("fp-b", 30))
            .expect("second");

        let rows = store
            .recent_repair_memory("example.com/spike-target", "fp-b", 3)
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fingerprint, "fp-a");
        assert_eq!(
            rows[0].commit_sha, "sha-a-2",
            "upsert keeps the newest evidence"
        );
        assert!(rows[0].pr_url.is_some());
        assert!(rows[0].root_cause.contains("ROOT CAUSE"));

        let other_repo = store
            .recent_repair_memory("example.com/other", "fp-b", 3)
            .expect("rows");
        assert!(other_repo.is_empty(), "memory never crosses repositories");
    }

    #[test]
    fn repair_memory_is_pruned_per_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        for index in 0..(REPAIR_MEMORY_KEEP as i64 + 5) {
            store
                .record_repair_memory(&memory_entry(&format!("fp-{index}"), index))
                .expect("record");
        }
        let rows = store
            .recent_repair_memory("example.com/spike-target", "none", 1_000)
            .expect("rows");
        assert_eq!(rows.len(), REPAIR_MEMORY_KEEP);
        assert_eq!(
            rows[0].fingerprint,
            format!("fp-{}", REPAIR_MEMORY_KEEP + 4)
        );
    }

    #[test]
    fn repair_memory_can_be_disabled_and_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .record_repair_memory(&memory_entry("fp-a", 10))
            .expect("a");
        store
            .record_repair_memory(&memory_entry("fp-b", 20))
            .expect("b");

        assert!(store
            .set_repair_memory_disabled("example.com/spike-target", "fp-a", true)
            .expect("disable"));
        let rows = store
            .recent_repair_memory("example.com/spike-target", "none", 10)
            .expect("rows");
        assert_eq!(
            rows.len(),
            1,
            "disabled memory must not enter prior_context"
        );
        assert_eq!(rows[0].fingerprint, "fp-b");

        let (listed, total) = store
            .list_repair_memory("example.com/spike-target", 10, 0)
            .expect("list");
        assert_eq!(total, 2);
        assert!(listed
            .iter()
            .any(|row| row.fingerprint == "fp-a" && row.disabled));

        assert!(store
            .set_repair_memory_disabled("example.com/spike-target", "fp-a", false)
            .expect("enable"));
        assert!(store
            .delete_repair_memory("example.com/spike-target", "fp-a")
            .expect("delete"));
        assert!(!store
            .delete_repair_memory("example.com/spike-target", "missing")
            .expect("missing"));
        let (_, total) = store
            .list_repair_memory("example.com/spike-target", 10, 0)
            .expect("list");
        assert_eq!(total, 1);
    }

    #[test]
    fn benchmark_run_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store
            .start_benchmark_run(&BenchmarkStart {
                id: "bench-1".into(),
                started_at: 1_000,
                provider: "faux".into(),
                model: Some("faux-1".into()),
                mode: "faux".into(),
                scenarios: 2,
            })
            .expect("start");
        store
            .insert_benchmark_result(&BenchmarkResultInput {
                run_id: "bench-1".into(),
                scenario: "go-off-by-one".into(),
                language: "go".into(),
                expected_fixed: true,
                repaired: true,
                claimed_fixed: true,
                delivery_ok: true,
                turns: 3,
                tokens_uncached: 120,
                tokens_cached: 30,
                duration_ms: 900,
                root_cause: Some("ROOT CAUSE: loop bound".into()),
                final_report: Some("VERIFICATION: go test ./... ok".into()),
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
                tokens_uncached: 80,
                tokens_cached: 0,
                duration_ms: 400,
                root_cause: None,
                final_report: Some("VERIFICATION: claimed".into()),
                error: Some("verification failed".into()),
            })
            .expect("result");
        store
            .finish_benchmark_run(
                "bench-1",
                &BenchmarkFinish {
                    finished_at: 2_000,
                    status: "completed".into(),
                    repaired: 1,
                    claimed: 2,
                    false_positive: 1,
                    delivery_ok: 1,
                    turns: 5,
                    tokens_uncached: 200,
                    tokens_cached: 30,
                    duration_ms: 1_300,
                    error: None,
                },
            )
            .expect("finish");

        let runs = store.list_benchmark_runs(10).expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "completed");
        assert_eq!(runs[0].false_positive, 1);
        let detail = store
            .get_benchmark_run("bench-1")
            .expect("detail")
            .expect("run");
        assert_eq!(detail.claimed, 2);
        let results = store.benchmark_results("bench-1").expect("results");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|row| row.scenario == "go-off-by-one" && row.repaired));
        assert!(results
            .iter()
            .any(|row| row.scenario == "node-noop-claim" && !row.repaired && row.claimed_fixed));
        assert!(store
            .get_benchmark_run("missing")
            .expect("missing")
            .is_none());
    }
}
