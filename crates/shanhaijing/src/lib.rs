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
";

/// Claim cooldown: while a run is in flight, further reports of the same
/// fingerprint only count occurrences (ADR-002 — no re-triggering).
pub const DEFAULT_COOLDOWN_MS: i64 = 6 * 60 * 60 * 1000;

impl Store {
    pub fn open(path: impl AsRef<Path>) -> ShanResult<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.execute_batch(SCHEMA)?;
        // Older databases (pre pr-columns): add missing columns idempotently.
        for column in ["pr_url TEXT", "pr_opened_at INTEGER"] {
            let _ = conn.execute(&format!("ALTER TABLE issues ADD COLUMN {column}"), []);
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic in another thread must not wedge the store forever.
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fingerprint a signal, dedup, and decide whether it may trigger a run.
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
            drop(conn);
            self.record_event(&fp, now_ms, "reported", Some("first sighting"))?;
            return Ok(ReportOutcome::NewIssue { fingerprint: fp });
        };

        conn.execute(
            "UPDATE issues SET occurrences = occurrences + 1, last_seen = ?2, stack_sample = ?3, severity = ?4 WHERE fingerprint = ?1",
            rusqlite::params![fp, now_ms, signal.stack_trace, signal.severity],
        )?;
        let occurrences = occurrences + 1;
        drop(conn);

        let state = State::parse(&state).ok_or(ShanError::UnknownState(state))?;
        if state.is_terminal() && state != State::Escalated {
            self.set_state(&fp, State::New, now_ms, Some("regression: terminal issue reported again"))?;
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
        self.record_event(&fp, now_ms, "reported", Some("dedup"))?;
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
                "state_changed",
                Some("claimed by runner"),
            )?;
        }
        Ok(claimed)
    }

    /// Validate + apply a state transition, journaling the event.
    pub fn set_state(
        &self,
        fingerprint: &str,
        to: State,
        now_ms: i64,
        detail: Option<&str>,
    ) -> ShanResult<()> {
        let conn = self.lock();
        let from: String = conn
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
        let from = State::parse(&from).ok_or_else(|| ShanError::UnknownState(from))?;
        if !can_transition(from, to) {
            return Err(ShanError::InvalidTransition { from, to });
        }
        conn.execute(
            "UPDATE issues SET state = ?2 WHERE fingerprint = ?1",
            rusqlite::params![fingerprint, to.as_str()],
        )?;
        drop(conn);
        self.record_event(fingerprint, now_ms, "state_changed", Some(detail.unwrap_or("")))
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
        self.record_event(fingerprint, now_ms, "pr_opened", Some(pr_url))
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

    fn record_event(
        &self,
        fingerprint: &str,
        now_ms: i64,
        kind: &str,
        detail: Option<&str>,
    ) -> ShanResult<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO events(fingerprint, ts, kind, detail) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![fingerprint, now_ms, kind, detail],
        )?;
        Ok(())
    }
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
}
