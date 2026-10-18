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

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

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
            _ => return None,
        })
    }

    fn is_terminal(&self) -> bool {
        matches!(self, State::Merged | State::Rejected | State::Escalated)
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
            | (Merged, New)
            | (Rejected, New)
            | (Escalated, New)
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
    Deduped { fingerprint: String, occurrences: u64 },
    /// Same fingerprint inside its cooldown — counted, not re-triggered.
    CooldownActive {
        fingerprint: String,
        occurrences: u64,
        retry_after_ms: i64,
    },
    /// Terminal-state issue reported again: regression, reopened as `new`.
    Regression { fingerprint: String, occurrences: u64 },
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
    pub stack_sample: String,
}

/// Full issue row — the `nuwa status` / ops view.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRow {
    pub fingerprint: String,
    pub title: String,
    pub language: String,
    pub repo: String,
    pub severity: String,
    pub state: String,
    pub occurrences: u64,
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
pub struct EventRow {
    pub fingerprint: String,
    pub ts: i64,
    pub kind: String,
    pub detail: String,
    pub repo: String,
    pub actor: Option<String>,
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
pub struct RunnerRow {
    pub id: String,
    pub repo: String,
    pub host: String,
    pub pid: Option<i64>,
    pub started_at: i64,
    pub heartbeat_at: i64,
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

/// Priority ordering shared by the claim and observer queries:
/// `severity_weight × occurrences`, then oldest sighting.
const READY_ORDER: &str = "(CASE severity
        WHEN 'critical' THEN 10 WHEN 'high' THEN 5 WHEN 'medium' THEN 2 ELSE 1 END)
        * occurrences DESC,
        last_seen ASC";

/// Full [`IssueRow`] projection; keep aligned with [`Store::issue_from_row`].
const ISSUE_VIEW_SELECT: &str = "SELECT i.fingerprint, i.title, i.language, i.repo, i.severity,
        i.state, i.occurrences, i.pr_url, i.first_seen, i.last_seen,
        CASE WHEN i.state_changed_at = 0 THEN i.first_seen ELSE i.state_changed_at END AS state_changed_at,
        i.owner_runner_id,
        (SELECT r.ci_verdict FROM runs r
         WHERE r.fingerprint = i.fingerprint
         ORDER BY r.started_at DESC, r.id DESC LIMIT 1) AS ci_verdict
 FROM issues i";

/// Event projection shared by the feed/detail/timeline queries; legacy rows
/// with `events.repo` NULL fall back to the issue repo. Keep aligned with
/// [`Store::event_from_row`].
const EVENT_SELECT: &str = "SELECT e.fingerprint, e.ts, COALESCE(e.kind, ''), COALESCE(e.detail, ''),
        COALESCE(e.repo, i.repo, ''), e.actor
 FROM events e LEFT JOIN issues i ON i.fingerprint = e.fingerprint";

/// Full [`RunRow`] projection; keep aligned with [`Store::run_from_row`].
const RUN_COLUMNS: &str = "id, harness_run_id, fingerprint, runner_id, repo, started_at, finished_at,
        phase, outcome, ci_verdict, turns, tokens_uncached, tokens_cached, provider,
        model, root_cause, change_stat, branch, commit_sha, pr_url, pr_opened_at,
        verification_ok, verification_detail, session_dir, error";

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
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fingerprint a signal, dedup, and decide whether it may trigger a run.
    /// Every report also bumps the per-day aggregate in `signal_daily`; only
    /// the first sighting writes a `reported` event (dedup must not spam the
    /// activity feed).
    pub fn report_signal(&self, signal: &Signal, now_ms: i64) -> ShanResult<ReportOutcome> {
        let fp = fingerprint::compute(
            &signal.language,
            signal.message.as_deref(),
            &signal.stack_trace,
        );
        let conn = self.lock();
        let existing: Option<(String, u64, i64)> = conn
            .query_row(
                "SELECT state, occurrences, cooldown_until FROM issues WHERE fingerprint = ?1",
                [&fp],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;

        let Some((state, occurrences, cooldown_until)) = existing else {
            conn.execute(
                "INSERT INTO issues(fingerprint, title, language, severity, state, occurrences, first_seen, last_seen, cooldown_until, repo, stack_sample)
                 VALUES (?1, ?2, ?3, ?4, 'new', 1, ?5, ?5, 0, ?6, ?7)",
                rusqlite::params![
                    fp,
                    signal.title,
                    signal.language,
                    signal.severity,
                    now_ms,
                    signal.repo,
                    signal.stack_trace,
                ],
            )?;
            bump_signal_daily(&conn, &fp, &signal.repo, now_ms)?;
            drop(conn);
            self.record_event(&fp, now_ms, Some(&signal.repo), "system", "reported", "first sighting")?;
            return Ok(ReportOutcome::NewIssue { fingerprint: fp });
        };

        conn.execute(
            "UPDATE issues SET occurrences = occurrences + 1, last_seen = ?2, stack_sample = ?3, severity = ?4 WHERE fingerprint = ?1",
            rusqlite::params![fp, now_ms, signal.stack_trace, signal.severity],
        )?;
        bump_signal_daily(&conn, &fp, &signal.repo, now_ms)?;
        let occurrences = occurrences + 1;
        drop(conn);

        let state = State::parse(&state).ok_or(ShanError::UnknownState(state))?;
        if state.is_terminal() && state != State::Escalated {
            self.set_state(&fp, State::New, now_ms, Some("regression: terminal issue reported again"))?;
            self.set_cooldown(&fp, 0)?;
            return Ok(ReportOutcome::Regression {
                fingerprint: fp,
                occurrences,
            });
        }
        if now_ms < cooldown_until {
            return Ok(ReportOutcome::CooldownActive {
                fingerprint: fp,
                occurrences,
                retry_after_ms: cooldown_until - now_ms,
            });
        }
        Ok(ReportOutcome::Deduped {
            fingerprint: fp,
            occurrences,
        })
    }

    /// Highest-priority pickable issue: state `new`, cooldown passed,
    /// ordered by `severity_weight × occurrences` then oldest sighting.
    pub fn ready_issue(&self, now_ms: i64) -> ShanResult<Option<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT fingerprint, title, language, repo, severity, occurrences, stack_sample
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
            "SELECT fingerprint, title, language, repo, severity, occurrences, stack_sample
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

    fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueSnapshot> {
        Ok(IssueSnapshot {
            fingerprint: row.get(0)?,
            title: row.get(1)?,
            language: row.get(2)?,
            repo: row.get(3)?,
            severity: row.get(4)?,
            occurrences: row.get(5)?,
            stack_sample: row.get(6)?,
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
        let conn = self.lock();
        let claimed: Option<IssueSnapshot> = conn
            .query_row(
                &format!(
                    "UPDATE issues SET state = 'triggered', cooldown_until = ?2, state_changed_at = ?1
                     WHERE fingerprint = (
                         SELECT fingerprint FROM issues
                         WHERE state = 'new' AND cooldown_until <= ?1
                         ORDER BY {READY_ORDER}
                         LIMIT 1
                     )
                     RETURNING fingerprint, title, language, repo, severity, occurrences, stack_sample"
                ),
                rusqlite::params![now_ms, cooldown_until_ms],
                Self::snapshot_from_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        drop(conn);
        if let Some(snapshot) = &claimed {
            self.record_event(
                &snapshot.fingerprint,
                now_ms,
                Some(&snapshot.repo),
                "runner",
                "state_changed",
                "claimed by runner",
            )?;
        }
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

        let owner_state: Option<(String, Option<String>, i64)> = match owner.as_deref() {
            Some(id) => conn
                .query_row(
                    "SELECT status, current_fingerprint, heartbeat_at FROM runners WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?,
            None => None,
        };

        let (owner_heartbeat, healthy_owner) = match &owner_state {
            Some((status, current, heartbeat)) => (
                Some(*heartbeat),
                status == "working"
                    && current.as_deref() == Some(fingerprint)
                    && now_ms - *heartbeat <= stuck_after_ms,
            ),
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
            return Err(ShanError::InvalidTransition { from, to: State::New });
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
            if force { "forced by owner" } else { "stuck requeued by owner" },
        )
    }

    /// Reopen an `escalated`/`rejected` issue as `new` (owner decision).
    pub fn reopen_issue(&self, fingerprint: &str, now_ms: i64) -> ShanResult<()> {
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
        if !matches!(from, State::Escalated | State::Rejected) {
            return Err(ShanError::InvalidTransition { from, to: State::New });
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
            "state_changed",
            "reopened by owner",
        )
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
        conn.execute(
            "UPDATE issues SET state = ?2, state_changed_at = ?3 WHERE fingerprint = ?1",
            rusqlite::params![fingerprint, to.as_str(), now_ms],
        )?;
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
        self.record_event(fingerprint, now_ms, Some(&repo), "runner", "pr_opened", pr_url)
    }

    /// PRs opened for `repo` since `since_ms` — the counter behind the
    /// per-repo daily PR limit (ADR-003 频控). Counts distinct PR URLs across
    /// the run ledger and legacy issue rows so retries of the same PR never
    /// consume quota twice.
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
    /// are kept for 365 days, runs after `run_days`, and runner rows whose
    /// last heartbeat is older than 30 days. Callers schedule this (M1b/M2).
    pub fn prune(&self, now_ms: i64, event_days: u32, run_days: u32) -> ShanResult<u64> {
        let event_cutoff = now_ms - i64::from(event_days) * 86_400_000;
        let audit_cutoff = now_ms - 365 * 86_400_000;
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
        deleted += conn.execute("DELETE FROM runners WHERE heartbeat_at < ?1", [runner_cutoff])? as u64;
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
        let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params), |row| {
            row.get(0)
        })?;
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
        let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params), |row| {
            row.get(0)
        })?;
        Ok(count as u64)
    }

    pub fn get(&self, fingerprint: &str) -> ShanResult<Option<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, title, language, repo, severity, occurrences, stack_sample
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
        self.issue_rows(&format!("{ISSUE_VIEW_SELECT} ORDER BY i.last_seen DESC"), [])
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

    fn issue_rows(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> ShanResult<Vec<IssueRow>> {
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
            .query_row("SELECT ts FROM heartbeat WHERE id = 1", [], |row| row.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(ts)
    }

    pub fn upsert_runner(&self, r: &RunnerUpsert) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO runners(id, repo, host, pid, started_at, heartbeat_at, status,
                                 current_fingerprint, phase, provider, model, daily_pr_limit,
                                 repo_path, service_unit, config_json, last_error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,NULL,?8,?9,?10,?11,?12,?13,NULL)
             ON CONFLICT(id) DO UPDATE SET
                repo=excluded.repo, host=excluded.host, pid=excluded.pid,
                started_at=excluded.started_at, heartbeat_at=excluded.heartbeat_at,
                status=excluded.status, current_fingerprint=NULL, phase=NULL,
                provider=excluded.provider, model=excluded.model,
                daily_pr_limit=excluded.daily_pr_limit, repo_path=excluded.repo_path,
                service_unit=excluded.service_unit, config_json=excluded.config_json,
                last_error=NULL",
            rusqlite::params![
                r.id, r.repo, r.host, r.pid, r.started_at, r.heartbeat_at, r.status,
                r.provider, r.model, r.daily_pr_limit, r.repo_path, r.service_unit, r.config_json,
            ],
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
                id, hb.heartbeat_at, hb.status, hb.current_fingerprint, hb.phase,
                hb.config_json, hb.last_error,
            ],
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
                    repo_path, service_unit, config_json, last_error
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

    pub fn start_run(&self, r: &RunStart) -> ShanResult<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO runs(id, fingerprint, runner_id, repo, started_at, phase, provider, model)
             VALUES (?1, ?2, ?3, ?4, ?5, 'claim', ?6, ?7)",
            rusqlite::params![
                r.id, r.fingerprint, r.runner_id, r.repo, r.started_at, r.provider, r.model
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
                turns = COALESCE(?3, turns),
                tokens_uncached = COALESCE(?4, tokens_uncached),
                tokens_cached = COALESCE(?5, tokens_cached),
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
                error = COALESCE(?17, error)
             WHERE id = ?1 AND finished_at IS NULL",
            rusqlite::params![
                id, u.phase, u.turns, u.tokens_uncached, u.tokens_cached, u.harness_run_id,
                u.root_cause, u.verification_ok, u.verification_detail, u.change_stat, u.branch,
                u.commit_sha, u.pr_url, u.pr_opened_at, u.ci_verdict, u.session_dir, u.error,
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
        let conn = self.lock();
        conn.execute(
            "INSERT INTO events(fingerprint, ts, repo, actor, kind, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![fingerprint, now_ms, repo, actor, kind, detail],
        )?;
        Ok(())
    }
}

/// UTC calendar day (`YYYY-MM-DD`) containing `now_ms` — the `signal_daily`
/// bucket key. Out-of-range timestamps fall back to the epoch day.
pub fn utc_day(now_ms: i64) -> String {
    use time::OffsetDateTime;
    let seconds = now_ms.div_euclid(1_000);
    let dt = OffsetDateTime::from_unix_timestamp(seconds).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", dt.year(), u8::from(dt.month()), dt.day())
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
        let fp = store.ready_issue(t0 + 2).expect("ready").expect("issue").fingerprint;
        store.set_state(&fp, State::Triggered, t0 + 3, None).expect("state");
        store.set_cooldown(&fp, t0 + 3_600_000).expect("cooldown");
        match store.report_signal(&signal(), t0 + 10).expect("report") {
            ReportOutcome::CooldownActive { occurrences, retry_after_ms, .. } => {
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
        let fp = store.ready_issue(t0).expect("ready").expect("issue").fingerprint;
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
        let fp = store.ready_issue(t0).expect("ready").expect("issue").fingerprint;
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
        assert!(store.claim_ready_issue(12, 1_000_000).expect("claim 3").is_none());

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
        let fp_a = fingerprint::compute("go", None, &signal().stack_trace);
        let mut c = signal();
        c.stack_trace = TRACE.replace("Average", "Load");
        let fp_c = fingerprint::compute("go", None, &c.stack_trace);
        store.report_signal(&c, 3).expect("c");

        store.set_pr(&fp_a, "https://x/pr/1", 10).expect("pr1");
        assert_eq!(
            store.prs_opened_since("example.com/spike-target", 0).expect("count"),
            1
        );
        assert_eq!(store.prs_opened_since("other/repo", 0).expect("count"), 0);

        store.set_pr(&fp_c, "https://x/pr/2", 20).expect("pr2");
        assert_eq!(
            store.prs_opened_since("example.com/spike-target", 0).expect("count"),
            2
        );
        assert_eq!(
            store.prs_opened_since("example.com/spike-target", 15).expect("count"),
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
        let fp = store.ready_issue(now).expect("ready").expect("issue").fingerprint;
        store.set_pr(&fp, "https://github.com/a/b/pull/1", now).expect("legacy pr");
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
        assert_eq!(store.prs_opened_since(&signal().repo, now - 1).expect("count"), 1);

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
        assert_eq!(store.prs_opened_since(&signal().repo, now - 1).expect("count"), 2);
    }

    #[test]
    fn prune_removes_old_rows_and_keeps_audit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        let day = 86_400_000;
        let now = 400 * day;
        store.record_event("fp", now - 100 * day, Some("r"), "system", "reported", "old").expect("e");
        store.record_event("fp", now - 100 * day, Some("r"), "owner", "config_changed", "k").expect("audit");
        store
            .record_event("fp", now - 400 * day, Some("r"), "owner", "config_changed", "old-audit")
            .expect("audit old");
        store
            .upsert_runner(&RunnerUpsert {
                id: "r".into(), repo: "r".into(), host: "h".into(), pid: None,
                started_at: 0, heartbeat_at: now - 40 * day, status: "idle".into(),
                provider: None, model: None, daily_pr_limit: None, repo_path: None,
                service_unit: None, config_json: "{}".into(),
            })
            .expect("runner");
        let deleted = store.prune(now, 90, 365).expect("prune");
        assert!(deleted >= 3);
        let events = store.recent_events(100).expect("events");
        assert!(events.iter().any(|e| e.kind == "config_changed" && e.detail == "k"));
        assert!(!events.iter().any(|e| e.detail == "old"));
        assert!(!events.iter().any(|e| e.detail == "old-audit"));
        assert!(store.list_runners().expect("runners").is_empty());
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
            .record_event("fp1", 1, Some("example.com/r"), "owner", "requeued", "manual")
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;

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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;

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

        seed_run(&store, "run-old", &fp, "example.com/spike-target", 100, None, (0, 0));
        seed_run(&store, "run-new", &fp, "example.com/spike-target", 200, None, (0, 0));
        seed_run(&store, "run-other", &other, "example.com/spike-target", 300, None, (0, 0));

        // An old run id is found directly, without scanning recent runs.
        let old = store
            .run_for(&fp, "run-old")
            .expect("run_for")
            .expect("old run");
        assert_eq!(old.id, "run-old");
        assert_eq!(old.fingerprint, fp);
        // A run of another fingerprint is never visible through this issue.
        assert!(store.run_for(&other, "run-old").expect("cross fp").is_none());
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
        assert!(store.requeue_stuck(&fp, false, old + 400_000, 300_000).is_ok());
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
        assert!(store.requeue_stuck(&fp, false, old + 400_021, 300_000).is_err());
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
        let fp = store.ready_issue(t).expect("ready").expect("issue").fingerprint;
        store.set_state(&fp, State::Triggered, t, None).expect("t");
        store.set_state(&fp, State::Fixing, t, None).expect("f");
        store.set_state(&fp, State::Escalated, t, None).expect("e");
        store.reopen_issue(&fp, t + 1).expect("reopen");
        assert!(store.ready_issue(t + 2).expect("ready").is_some());
        assert!(store.reopen_issue(&fp, t + 3).is_err(), "new is not reopenable");
    }

    #[test]
    fn mark_interrupted_closes_open_runs_for_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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

        let fp = store.ready_issue(t0).expect("ready").expect("issue").fingerprint;
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
        assert!(store.runs_for("does-not-exist", 10).expect("runs").is_empty());
    }

    #[test]
    fn mark_interrupted_preserves_existing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1).expect("report");
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
                &RunUpdate { error: Some("provider exploded".into()), ..RunUpdate::default() },
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
                &RunUpdate { phase: Some("ci".into()), ..RunUpdate::default() },
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
            .upsert_runner(&RunnerUpsert { config_json: "{\"v\":2}".into(), ..base })
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
        store.set_state_as(&fp, State::Triggered, 2, None, "runner").expect("triggered");
        store.set_state_as(&fp, State::Fixing, 3, None, "runner").expect("fixing");
        store.set_state_as(&fp, State::Escalated, 4, None, "runner").expect("escalated");
        store.set_state_as(&fp, State::New, 5, Some("reopen"), "owner").expect("new");
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
                &RunUpdate { ci_verdict: Some("success".into()), ..RunUpdate::default() },
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
                &RunUpdate { ci_verdict: Some("success".into()), ..RunUpdate::default() },
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
                &RunUpdate { ci_verdict: Some("failure".into()), ..RunUpdate::default() },
            )
            .expect("ci b");

        let row = store.list_issues().expect("issues").pop().expect("issue");
        assert_eq!(row.ci_verdict.as_deref(), Some("failure"), "newest run wins");
        assert_eq!(row.owner_runner_id.as_deref(), Some("run-b-runner"));
    }

    #[test]
    fn issue_row_serializes_ms_suffixed_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        store.report_signal(&signal(), 1_700_000_000_000).expect("report");
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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
        let low = Signal { severity: "low".into(), ..issue_signal("low", "Load") };
        let critical = Signal { severity: "critical".into(), ..issue_signal("critical", "Submit") };
        let fp_low = fingerprint::compute("go", None, &low.stack_trace);
        store.report_signal(&low, 1).expect("low");
        store.report_signal(&low, 2).expect("low again");
        store.report_signal(&critical, 1).expect("critical");

        let ready = store.ready_issues(3, 10).expect("ready");
        let severities: Vec<&str> = ready.iter().map(|i| i.severity.as_str()).collect();
        assert_eq!(severities, ["critical", "low"]);
        assert_eq!(store.ready_issues(3, 1).expect("limited").len(), 1);
        assert_eq!(store.count_ready(3).expect("count"), 2);
        assert!(
            store.list_issues().expect("issues").iter().all(|i| i.state == "new"),
            "observing the queue must not flip states"
        );

        let claimed = store
            .claim_ready_issue(3, 1_000_000)
            .expect("claim")
            .expect("issue");
        assert_eq!(claimed.severity, "critical", "observation left priority intact");
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
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
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

        assert_eq!(store.count_events_since(&["ci_green"], t0).expect("green"), 2);
        assert_eq!(
            store.count_events_since(&["ci_green", "ci_red"], t0).expect("both"),
            3
        );
        assert_eq!(
            store.count_events_since(&["ci_green"], t0 + 10).expect("window"),
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
        store.report_signal(&issue_signal("panic in Submit", "Submit"), t0 + 2).expect("b");
        store
            .report_signal(&issue_signal("panic in Load", "Load"), t0 + day_ms)
            .expect("next day");

        assert_eq!(store.reports_on(&utc_day(t0)).expect("today"), 3);
        assert_eq!(store.reports_on(&utc_day(t0 + day_ms)).expect("tomorrow"), 1);
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
        store
            .report_signal(&signal(), t0 + 1)
            .expect("app repeat");
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
        assert_eq!(
            store.reports_on_repo(&day, Some(app_repo)).expect("app"),
            2
        );
        assert_eq!(
            store.reports_on_repo(&day, Some(other_repo)).expect("other"),
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
            .record_event(&other_fp, t0, Some(other_repo), "runner", "ci_green", "other")
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
        let fp_a = fingerprint::compute("go", None, &sig_a.stack_trace);
        let fp_b = fingerprint::compute("go", None, &sig_b.stack_trace);
        let fp_d = fingerprint::compute("go", None, &sig_d.stack_trace);
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
        let fps: Vec<&str> = excluding_newest.iter().map(|i| i.fingerprint.as_str()).collect();
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
        let fp_a = fingerprint::compute("go", None, &sig_a.stack_trace);
        let fp_b = fingerprint::compute("go", None, &sig_b.stack_trace);
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
        assert_eq!(legacy.repo, repo_a, "NULL event.repo falls back to the issue repo");

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
        assert_eq!(page.iter().map(|e| e.ts).collect::<Vec<_>>(), [2_000, 1_700]);
        assert!(store
            .list_events(&EventQuery::default(), 0)
            .expect("zero limit")
            .is_empty());

        let injection = EventQuery {
            kinds: vec!["ci_green' OR 1=1 --".into()],
            ..EventQuery::default()
        };
        assert!(
            store.list_events(&injection, 10).expect("bound kind").is_empty(),
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
        let fp_a = fingerprint::compute("go", None, &sig_a.stack_trace);
        let fp_c = fingerprint::compute("go", None, &sig_c.stack_trace);

        // Reports also create the issues that own them; the old issue and its
        // report fall outside the 3-day window.
        store.report_signal(&sig_a, at(d0, 1)).expect("a d0");
        store.report_signal(&sig_a, at(d1, 1)).expect("a d1");
        store.report_signal(&sig_a, at(d1, 2)).expect("a d1 again");
        store.report_signal(&sig_b, at(d1, 3)).expect("b d1");
        store.report_signal(&sig_c, at(d1, 4)).expect("c d1");
        store.report_signal(&sig_d, at(d2, 1)).expect("d d2");
        store.report_signal(&sig_a, at(d2, 2)).expect("a d2");
        store.report_signal(&sig_old, at(before, 3)).expect("old issue");

        // Tokens bucket by run start; PRs opened by pr_opened_at.
        seed_run(&store, "run-1", &fp_a, repo_a, at(d0, 5), Some(at(d1, 5)), (100, 50));
        seed_run(&store, "run-2", &fp_a, repo_a, at(d1, 5), Some(at(d2, 5)), (200, 100));
        seed_run(&store, "run-3", &fp_c, repo_b, at(d2, 5), None, (50, 10));
        seed_run(&store, "run-old", &fp_a, repo_a, at(before, 5), Some(at(before, 6)), (999, 999));

        // Window is [d0 00:00, d2+1 00:00): the exact start is counted, the
        // exact end and earlier days are not.
        store
            .record_event(&fp_c, d0, Some(repo_b), "system", "ci_green", "window start")
            .expect("start");
        store
            .record_event(&fp_c, at(d0, 6), Some(repo_b), "system", "ci_red", "d0 red")
            .expect("d0 red");
        store
            .record_event(&fp_a, at(d1, 6), Some(repo_a), "system", "merged", "d1 merge")
            .expect("d1 merge");
        store
            .record_event(&fp_c, at(d1, 7), Some(repo_b), "system", "rejected", "d1 reject")
            .expect("d1 reject");
        store
            .record_event(&fp_a, at(d2, 6), Some(repo_a), "system", "ci_green", "d2 green")
            .expect("d2 green");
        store
            .record_event(&fp_a, at(d2, 7), Some(repo_a), "system", "ci_green", "d2 green 2")
            .expect("d2 green 2");
        store
            .record_event(&fp_a, at(d2, 8), None, "system", "ci_green", "legacy null repo")
            .expect("legacy green");
        store
            .record_event(&fp_a, at(d1, 8), Some(repo_a), "system", "state_changed", "ignored")
            .expect("ignored kind");
        store
            .record_event(&fp_a, at(before, 6), Some(repo_a), "system", "merged", "before")
            .expect("before");
        store
            .record_event(&fp_a, d2 + day_ms, Some(repo_a), "system", "merged", "window end")
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
        assert_eq!(store.trends(3, Some(repo_a), now).expect("repo a"), expected_a);

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
        assert_eq!(store.trends(3, Some(repo_b), now).expect("repo b"), expected_b);
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
                &TrendBucket { day: day.clone(), ..TrendBucket::default() },
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

        let before = store.trends(2, None, midnight - 1).expect("before midnight");
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
        let fp = fingerprint::compute("go", None, &sig.stack_trace);
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
        let fp_legacy = fingerprint::compute("go", None, &sig_legacy.stack_trace);
        store
            .report_signal(&sig_legacy, t_start)
            .expect("legacy report");
        store
            .set_pr(&fp_legacy, "https://github.com/acme/spike/pull/11", t_pr)
            .expect("legacy pr");

        // Another repo's PR must not leak into the repo filter.
        let mut sig_other = issue_signal("panic in Load", "Load");
        sig_other.repo = other.to_string();
        let fp_other = fingerprint::compute("go", None, &sig_other.stack_trace);
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
        assert_eq!(store.trends(1, Some(other), now).expect("other")[0].merged, 1);
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
            stack_trace: trace.into(),
        };
        store.report_signal(&sig, 1).expect("report");
        let fp = store.ready_issue(1).expect("ready").expect("issue").fingerprint;
        store.set_state(&fp, State::Triggered, 2, Some("claimed")).expect("t");
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
}
