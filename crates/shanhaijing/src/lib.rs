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
    pub first_seen: i64,
    pub last_seen: i64,
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
        let mut stmt = conn.prepare(
            "SELECT fingerprint, title, language, repo, severity, occurrences, stack_sample
             FROM issues
             WHERE state = 'new' AND cooldown_until <= ?1
             ORDER BY (CASE severity
                 WHEN 'critical' THEN 10 WHEN 'high' THEN 5 WHEN 'medium' THEN 2 ELSE 1 END)
                 * occurrences DESC,
                 last_seen ASC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([now_ms])?;
        match rows.next()? {
            Some(row) => Ok(Some(IssueSnapshot {
                fingerprint: row.get(0)?,
                title: row.get(1)?,
                language: row.get(2)?,
                repo: row.get(3)?,
                severity: row.get(4)?,
                occurrences: row.get(5)?,
                stack_sample: row.get(6)?,
            })),
            None => Ok(None),
        }
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
                "UPDATE issues SET state = 'triggered', cooldown_until = ?2
                 WHERE fingerprint = (
                     SELECT fingerprint FROM issues
                     WHERE state = 'new' AND cooldown_until <= ?1
                     ORDER BY (CASE severity
                         WHEN 'critical' THEN 10 WHEN 'high' THEN 5 WHEN 'medium' THEN 2 ELSE 1 END)
                         * occurrences DESC,
                         last_seen ASC
                     LIMIT 1
                 )
                 RETURNING fingerprint, title, language, repo, severity, occurrences, stack_sample",
                rusqlite::params![now_ms, cooldown_until_ms],
                |row| {
                    Ok(IssueSnapshot {
                        fingerprint: row.get(0)?,
                        title: row.get(1)?,
                        language: row.get(2)?,
                        repo: row.get(3)?,
                        severity: row.get(4)?,
                        occurrences: row.get(5)?,
                        stack_sample: row.get(6)?,
                    })
                },
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
    /// per-repo daily PR limit (ADR-003 频控).
    pub fn prs_opened_since(&self, repo: &str, since_ms: i64) -> ShanResult<u64> {
        let conn = self.lock();
        let count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE repo = ?1 AND pr_opened_at IS NOT NULL AND pr_opened_at >= ?2",
            rusqlite::params![repo, since_ms],
            |row| row.get(0),
        )?;
        Ok(count)
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

    pub fn get(&self, fingerprint: &str) -> ShanResult<Option<IssueSnapshot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, title, language, repo, severity, occurrences, stack_sample
             FROM issues WHERE fingerprint = ?1",
        )?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(IssueSnapshot {
                fingerprint: row.get(0)?,
                title: row.get(1)?,
                language: row.get(2)?,
                repo: row.get(3)?,
                severity: row.get(4)?,
                occurrences: row.get(5)?,
                stack_sample: row.get(6)?,
            })),
            None => Ok(None),
        }
    }

    /// All issues, newest-sighting first — the `nuwa status` view.
    pub fn list_issues(&self) -> ShanResult<Vec<IssueRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT fingerprint, title, language, repo, severity, state, occurrences,
                    pr_url, first_seen, last_seen
             FROM issues ORDER BY last_seen DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(IssueRow {
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
            });
        }
        Ok(out)
    }

    /// Recent lifecycle events, newest first — the console activity feed.
    pub fn recent_events(&self, limit: u32) -> ShanResult<Vec<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.fingerprint, e.ts, COALESCE(e.kind, ''), COALESCE(e.detail, ''),
                    COALESCE(e.repo, i.repo, ''), e.actor
             FROM events e LEFT JOIN issues i ON i.fingerprint = e.fingerprint
             ORDER BY e.id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query([limit])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(EventRow {
                fingerprint: row.get(0)?,
                ts: row.get(1)?,
                kind: row.get(2)?,
                detail: row.get(3)?,
                repo: row.get(4)?,
                actor: row.get(5)?,
            });
        }
        Ok(out)
    }

    /// Per-issue lifecycle events, newest first — the detail page timeline.
    pub fn events_for(&self, fingerprint: &str, limit: u32) -> ShanResult<Vec<EventRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.fingerprint, e.ts, COALESCE(e.kind, ''), COALESCE(e.detail, ''),
                    COALESCE(e.repo, i.repo, ''), e.actor
             FROM events e LEFT JOIN issues i ON i.fingerprint = e.fingerprint
             WHERE e.fingerprint = ?1
             ORDER BY e.id DESC LIMIT ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![fingerprint, limit])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(EventRow {
                fingerprint: row.get(0)?,
                ts: row.get(1)?,
                kind: row.get(2)?,
                detail: row.get(3)?,
                repo: row.get(4)?,
                actor: row.get(5)?,
            });
        }
        Ok(out)
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
        let conn = self.lock();
        conn.execute(
            "INSERT INTO runs(id, fingerprint, runner_id, repo, started_at, phase, provider, model)
             VALUES (?1, ?2, ?3, ?4, ?5, 'claim', ?6, ?7)",
            rusqlite::params![
                r.id, r.fingerprint, r.runner_id, r.repo, r.started_at, r.provider, r.model
            ],
        )?;
        conn.execute(
            "UPDATE issues SET owner_runner_id = ?2 WHERE fingerprint = ?1",
            rusqlite::params![r.fingerprint, r.runner_id],
        )?;
        Ok(())
    }

    pub fn update_run(&self, id: &str, u: &RunUpdate) -> ShanResult<()> {
        let conn = self.lock();
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
             WHERE id = ?1",
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
                error='runner restarted before completion'
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
        let mut stmt = conn.prepare(
            "SELECT id, harness_run_id, fingerprint, runner_id, repo, started_at, finished_at,
                    phase, outcome, ci_verdict, turns, tokens_uncached, tokens_cached, provider,
                    model, root_cause, change_stat, branch, commit_sha, pr_url, pr_opened_at,
                    verification_ok, verification_detail, session_dir, error
             FROM runs WHERE fingerprint=?1 ORDER BY started_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([fingerprint])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::run_from_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn runs_for(&self, fingerprint: &str, limit: u32) -> ShanResult<Vec<RunRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, harness_run_id, fingerprint, runner_id, repo, started_at, finished_at,
                    phase, outcome, ci_verdict, turns, tokens_uncached, tokens_cached, provider,
                    model, root_cause, change_stat, branch, commit_sha, pr_url, pr_opened_at,
                    verification_ok, verification_detail, session_dir, error
             FROM runs WHERE fingerprint=?1 ORDER BY started_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fingerprint, limit], |row| {
            Self::run_from_row(row)
        })?;
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
