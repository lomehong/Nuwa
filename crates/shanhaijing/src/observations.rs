use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::{record_event_on, ShanError, ShanResult, Signal, Store};

pub const MAX_ENVIRONMENT_BYTES: usize = 128;
pub const MAX_RELEASE_SHA_BYTES: usize = 128;
pub const MAX_SOURCE_EVENT_ID_BYTES: usize = 256;
pub const MAX_TRUSTED_SOURCE_BYTES: usize = 128;
pub const SIGNAL_OBSERVATIONS_KEEP: u32 = 100;
pub const SIGNAL_OBSERVATIONS_RETENTION_MS: i64 = 30 * crate::DAY_MS;

/// Receipt history, not issue identity or global exactly-once proof. Invalid
/// and future times remain visible but cannot authorize lifecycle decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignalObservation {
    pub id: i64,
    pub fingerprint: String,
    pub received_at_ms: i64,
    pub observed_at_ms: Option<i64>,
    pub environment: Option<String>,
    pub release_sha: Option<String>,
    pub source_event_id: Option<String>,
    pub trusted_source: Option<String>,
    pub time_status: String,
    pub disposition: String,
    pub duplicate_of: Option<i64>,
}

impl SignalObservation {
    pub(super) fn new(
        fp: &str,
        signal: &Signal,
        received: i64,
        source: Option<&str>,
    ) -> ShanResult<Self> {
        for (value, max) in [
            (signal.environment.as_deref(), MAX_ENVIRONMENT_BYTES),
            (signal.release_sha.as_deref(), MAX_RELEASE_SHA_BYTES),
            (signal.source_event_id.as_deref(), MAX_SOURCE_EVENT_ID_BYTES),
            (source, MAX_TRUSTED_SOURCE_BYTES),
        ] {
            if value.is_some_and(|s| {
                s.trim().is_empty() || s.len() > max || s.chars().any(char::is_control)
            }) {
                return Err(ShanError::InvalidObservation);
            }
        }
        let time_status = match signal.observed_at {
            None => "missing",
            Some(at) if at <= 0 => "invalid",
            Some(at) if at > received => "future",
            Some(_) if source.is_none() => "untrusted",
            Some(_) => "trusted",
        };
        Ok(Self {
            id: 0,
            fingerprint: fp.into(),
            received_at_ms: received,
            observed_at_ms: signal.observed_at,
            environment: signal.environment.clone(),
            // Full SHA identities are normalized consistently, not truncated.
            release_sha: signal.release_sha.as_ref().map(|sha| {
                if full_sha(sha) {
                    sha.to_ascii_lowercase()
                } else {
                    sha.clone()
                }
            }),
            source_event_id: signal.source_event_id.clone(),
            trusted_source: source.map(str::to_owned),
            time_status: time_status.into(),
            disposition: String::new(),
            duplicate_of: None,
        })
    }

    pub(super) fn trusted_time(&self) -> Option<i64> {
        self.observed_at_ms
            .filter(|_| self.time_status == "trusted")
    }

    pub(super) fn check_duplicate_on(&mut self, conn: &Connection) -> ShanResult<()> {
        let (Some(source), Some(event)) = (&self.trusted_source, &self.source_event_id) else {
            return Ok(());
        };
        let prior = conn
            .query_row(
                "SELECT id,observed_at_ms,environment,release_sha FROM signal_observations
                 WHERE fingerprint=?1 AND trusted_source=?2 AND source_event_id=?3
                 ORDER BY id ASC LIMIT 1",
                rusqlite::params![self.fingerprint, source, event],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, observed, environment, release)) = prior {
            if observed != self.observed_at_ms
                || environment != self.environment
                || release != self.release_sha
            {
                return Err(ShanError::InvalidObservation);
            }
            self.duplicate_of = Some(id);
        }
        Ok(())
    }

    pub(super) fn persist_on(&self, conn: &Connection) -> ShanResult<()> {
        conn.execute(
            "INSERT INTO signal_observations(fingerprint,received_at_ms,observed_at_ms,
                environment,release_sha,source_event_id,trusted_source,time_status,disposition,duplicate_of)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                self.fingerprint,
                self.received_at_ms,
                self.observed_at_ms,
                self.environment,
                self.release_sha,
                self.source_event_id,
                self.trusted_source,
                self.time_status,
                self.disposition,
                self.duplicate_of
            ],
        )?;
        conn.execute(
            "DELETE FROM signal_observations WHERE fingerprint=?1 AND id NOT IN
             (SELECT id FROM signal_observations WHERE fingerprint=?1
              ORDER BY received_at_ms DESC,id DESC LIMIT ?2)",
            rusqlite::params![self.fingerprint, SIGNAL_OBSERVATIONS_KEEP],
        )?;
        Ok(())
    }

    pub(super) fn revoke_memory_on(&self, conn: &Connection, repo: &str) -> ShanResult<()> {
        let (Some(at), Some(environment), Some(sha)) =
            (self.trusted_time(), &self.environment, &self.release_sha)
        else {
            return Ok(());
        };
        if environment.trim().is_empty()
            || !full_sha(sha)
            || matches!(self.disposition.as_str(), "delayed" | "duplicate" | "muted")
        {
            return Ok(());
        }
        let changed = conn.execute(
            "UPDATE repair_memory_evidence SET valid=0,
                revoked_at=COALESCE(revoked_at,?5),
                revocation_reason=COALESCE(revocation_reason,'trusted_regression')
             WHERE repo=?1 AND fingerprint=?2 AND commit_sha=?3 COLLATE NOCASE
                AND recorded_at < ?4 AND valid=1",
            rusqlite::params![repo, self.fingerprint, sha, at, self.received_at_ms],
        )?;
        if changed > 0 {
            record_event_on(
                conn,
                &self.fingerprint,
                self.received_at_ms,
                Some(repo),
                "system",
                "memory_revoked",
                "trusted_regression: exact observed release",
            )?;
        }
        Ok(())
    }
}

fn full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Store {
    /// Newest receipt time/id first, capped at 100. Eviction and age pruning
    /// deliberately bound duplicate detection to retained history only.
    pub fn observations_for(&self, fp: &str, limit: u32) -> ShanResult<Vec<SignalObservation>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id,fingerprint,received_at_ms,observed_at_ms,environment,release_sha,
                source_event_id,trusted_source,time_status,disposition,duplicate_of
             FROM signal_observations WHERE fingerprint=?1
             ORDER BY received_at_ms DESC,id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![fp, limit.min(SIGNAL_OBSERVATIONS_KEEP)],
            |r| {
                Ok(SignalObservation {
                    id: r.get(0)?,
                    fingerprint: r.get(1)?,
                    received_at_ms: r.get(2)?,
                    observed_at_ms: r.get(3)?,
                    environment: r.get(4)?,
                    release_sha: r.get(5)?,
                    source_event_id: r.get(6)?,
                    trusted_source: r.get(7)?,
                    time_status: r.get(8)?,
                    disposition: r.get(9)?,
                    duplicate_of: r.get(10)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fingerprint, tests::signal, RepairMemoryEntry, ReportOutcome};

    fn fp(signal: &Signal) -> String {
        fingerprint::compute(
            &signal.repo,
            &signal.language,
            signal.message.as_deref(),
            &signal.stack_trace,
        )
    }

    fn state(store: &Store, signal: &Signal, state: &str, at: i64) {
        store
            .lock()
            .execute(
                "UPDATE issues SET state=?2,state_changed_at=?3 WHERE fingerprint=?1",
                rusqlite::params![fp(signal), state, at],
            )
            .expect("fixture state");
    }

    fn observed() -> Signal {
        Signal {
            observed_at: Some(150),
            environment: Some("production".into()),
            release_sha: Some("a".repeat(40)),
            ..signal()
        }
    }

    fn memory(store: &Store, signal: &Signal, source: &str, sha: &str, at: i64) {
        store
            .record_verified_repair_memory(
                &RepairMemoryEntry {
                    repo: signal.repo.clone(),
                    fingerprint: fp(signal),
                    commit_sha: sha.into(),
                    pr_url: None,
                    root_cause: "verified cause".into(),
                    change_stat: "one file".into(),
                    files: 1,
                    recorded_at: at,
                },
                "local_verification",
                source,
            )
            .expect("memory");
    }

    fn valid(store: &Store, source: &str) -> bool {
        store
            .lock()
            .query_row(
                "SELECT valid FROM repair_memory_evidence WHERE source_id=?1",
                [source],
                |r| r.get(0),
            )
            .expect("valid")
    }

    // Every column of every table changed by reporting, including payload and
    // evidence tombstones, so a partial rollback cannot pass unnoticed.
    fn snapshot(store: &Store) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let conn = store.lock();
        [
            "issues",
            "events",
            "signal_daily",
            "signal_observations",
            "repair_memory",
            "repair_memory_evidence",
        ]
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .expect("prepare");
            let columns = stmt.column_count();
            stmt.query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
                .expect("query")
                .collect::<Result<_, _>>()
                .expect("rows")
        })
        .collect()
    }

    #[test]
    fn legacy_json_defaults_and_json_cannot_assert_trust() {
        let mut json = serde_json::to_value(signal()).expect("json");
        for key in [
            "observed_at",
            "environment",
            "release_sha",
            "source_event_id",
        ] {
            json.as_object_mut().expect("object").remove(key);
        }
        json.as_object_mut().expect("object").remove("severity");
        let legacy: Signal = serde_json::from_value(json.clone()).expect("legacy");
        assert_eq!(legacy.severity, "medium");
        assert_eq!(legacy.observed_at, None);
        assert_eq!(legacy.environment, None);
        assert_eq!(legacy.release_sha, None);
        assert_eq!(legacy.source_event_id, None);
        let store = Store::open(":memory:").expect("store");
        assert!(matches!(
            store.report_signal(&legacy, 200).expect("legacy"),
            ReportOutcome::NewIssue { .. }
        ));
        json["observed_at"] = 50.into();
        json["environment"] = "production".into();
        json["release_sha"] = "a".repeat(40).into();
        json["source_event_id"] = "event".into();
        json["trusted_source"] = "owner".into();
        let incoming: Signal = serde_json::from_value(json).expect("metadata");
        assert_eq!(fp(&legacy), fp(&incoming));
        state(&store, &legacy, "merged", 100);
        assert!(matches!(
            store.report_signal(&incoming, 300).expect("untrusted"),
            ReportOutcome::Regression { occurrences: 2, .. }
        ));
        let rows = store.observations_for(&fp(&legacy), 100).expect("rows");
        assert_eq!(rows[0].time_status, "untrusted");
        assert_eq!(rows[0].trusted_source, None);
        assert_eq!(rows[1].time_status, "missing");
        assert_eq!(rows[1].disposition, "new");
    }

    #[test]
    fn time_status_retains_missing_invalid_future_and_untrusted_times() {
        let store = Store::open(":memory:").expect("store");
        for (at, source, expected) in [
            (None, None, "missing"),
            (None, Some("owner"), "missing"),
            (Some(0), Some("owner"), "invalid"),
            (Some(-5), None, "invalid"),
            (Some(201), Some("owner"), "future"),
            (Some(201), None, "future"),
            (Some(200), None, "untrusted"),
            (Some(200), Some("owner"), "trusted"),
        ] {
            let incoming = Signal {
                observed_at: at,
                ..signal()
            };
            store
                .report_signal_with_source(&incoming, 200, source)
                .expect("receipt");
            let row = &store.observations_for(&fp(&incoming), 1).expect("rows")[0];
            assert_eq!(row.time_status, expected);
            assert_eq!(row.observed_at_ms, at);
            assert_eq!(row.trusted_source.as_deref(), source);
            assert_eq!(row.fingerprint, fp(&signal()));
        }
        assert_eq!(store.list_issues().expect("issues").len(), 1);
        assert_eq!(store.reports_on(&crate::utc_day(200)).expect("daily"), 8);
    }

    #[test]
    fn only_trusted_strictly_earlier_terminal_observations_are_delayed() {
        for terminal in ["merged", "rejected"] {
            for (at, boundary, source, delayed) in [
                (Some(99), 100, Some("owner"), true),
                (Some(100), 100, Some("owner"), false),
                (Some(101), 100, Some("owner"), false),
                (Some(99), 100, None, false),
                (Some(99), 0, Some("owner"), false),
                (None, 100, Some("owner"), false),
                (Some(0), 100, Some("owner"), false),
                (Some(301), 100, Some("owner"), false),
            ] {
                let store = Store::open(":memory:").expect("store");
                store.report_signal(&signal(), 200).expect("first");
                state(&store, &signal(), terminal, boundary);
                let incoming = Signal {
                    observed_at: at,
                    ..signal()
                };
                let outcome = store
                    .report_signal_with_source(&incoming, 300, source)
                    .expect("report");
                assert_eq!(
                    matches!(outcome, ReportOutcome::Delayed { occurrences: 2, .. }),
                    delayed
                );
                assert_eq!(
                    matches!(outcome, ReportOutcome::Regression { occurrences: 2, .. }),
                    !delayed
                );
                let issue = &store.list_issues().expect("issues")[0];
                assert_eq!(issue.state, if delayed { terminal } else { "new" });
                assert_eq!(issue.occurrences, 2);
                assert_eq!(store.reports_on(&crate::utc_day(300)).expect("daily"), 2);
                let row = &store.observations_for(&fp(&incoming), 1).expect("rows")[0];
                assert_eq!(
                    row.disposition,
                    if delayed { "delayed" } else { "regression" }
                );
                assert_eq!(
                    store.events_for(&fp(&incoming), 100).expect("events").len(),
                    if delayed { 1 } else { 2 }
                );
            }
        }
        for active in ["new", "triggered", "fixing", "pr_open", "escalated"] {
            let store = Store::open(":memory:").expect("store");
            store.report_signal(&signal(), 1).expect("first");
            state(&store, &signal(), active, 100);
            let incoming = Signal {
                observed_at: Some(50),
                ..signal()
            };
            assert!(matches!(
                store
                    .report_signal_with_source(&incoming, 200, Some("owner"))
                    .expect("report"),
                ReportOutcome::Deduped { .. }
            ));
            assert_eq!(store.list_issues().expect("issues")[0].state, active);
        }
    }

    #[test]
    fn trusted_duplicates_count_and_keep_terminal_even_without_time() {
        for at in [None, Some(50)] {
            for terminal in ["merged", "rejected", "ignored"] {
                let store = Store::open(":memory:").expect("store");
                let mut incoming = Signal {
                    observed_at: at,
                    source_event_id: Some("event".into()),
                    ..signal()
                };
                store
                    .report_signal_with_source(&incoming, 300, Some("owner"))
                    .expect("first");
                let first = store.observations_for(&fp(&incoming), 1).expect("rows")[0].id;
                state(&store, &incoming, terminal, 100);
                incoming.severity = "critical".into();
                incoming.log_window = vec!["new payload".into()];
                assert!(matches!(
                    store
                        .report_signal_with_source(&incoming, 200, Some("owner"))
                        .expect("duplicate"),
                    ReportOutcome::Duplicate { occurrences: 2, .. }
                ));
                let rows = store.observations_for(&fp(&incoming), 100).expect("rows");
                assert_eq!(rows[1].duplicate_of, Some(first));
                assert_eq!(rows[1].disposition, "duplicate");
                let issue = &store.list_issues().expect("issues")[0];
                assert_eq!(issue.state, terminal);
                assert_eq!(issue.last_seen, 300);
                assert_eq!(issue.severity, "critical");
                let log: String = store
                    .lock()
                    .query_row("SELECT log_sample FROM issues", [], |r| r.get(0))
                    .expect("log");
                assert_eq!(log, "[\"new payload\"]");
                assert_eq!(store.reports_on(&crate::utc_day(200)).expect("daily"), 2);
                assert_eq!(
                    store.events_for(&fp(&incoming), 100).expect("events").len(),
                    1
                );
            }
        }
    }

    #[test]
    fn source_event_identity_is_scoped_to_trusted_source_and_fingerprint() {
        for (first_source, next_source) in [
            (None, None),
            (None, Some("owner")),
            (Some("owner"), None),
            (Some("owner"), Some("other-owner")),
        ] {
            let store = Store::open(":memory:").expect("store");
            let incoming = Signal {
                source_event_id: Some("event".into()),
                ..signal()
            };
            store
                .report_signal_with_source(&incoming, 1, first_source)
                .expect("first");
            state(&store, &incoming, "merged", 10);
            let next = Signal {
                environment: Some("different-environment".into()),
                ..incoming
            };
            assert!(matches!(
                store
                    .report_signal_with_source(&next, 20, next_source)
                    .expect("next"),
                ReportOutcome::Regression { .. }
            ));
        }
        let store = Store::open(":memory:").expect("store");
        let incoming = Signal {
            source_event_id: Some("event".into()),
            ..signal()
        };
        store
            .report_signal_with_source(&incoming, 1, Some("owner"))
            .expect("first");
        let other = Signal {
            repo: "another/repo".into(),
            ..incoming
        };
        assert!(matches!(
            store
                .report_signal_with_source(&other, 2, Some("owner"))
                .expect("other fp"),
            ReportOutcome::NewIssue { .. }
        ));
    }

    #[test]
    fn conflicting_event_reuse_rolls_back_every_consequence() {
        let store = Store::open(":memory:").expect("store");
        let incoming = Signal {
            source_event_id: Some("event".into()),
            ..observed()
        };
        store
            .report_signal_with_source(&incoming, 200, Some("owner"))
            .expect("first");
        memory(&store, &incoming, "proof", &"a".repeat(40), 100);
        state(&store, &incoming, "merged", 100);
        let before = snapshot(&store);
        let mut changed_time = incoming.clone();
        changed_time.observed_at = Some(151);
        let mut changed_env = incoming.clone();
        changed_env.environment = Some("staging".into());
        let mut changed_sha = incoming.clone();
        changed_sha.release_sha = Some("b".repeat(40));
        for conflicting in [changed_time, changed_env, changed_sha] {
            assert!(matches!(
                store.report_signal_with_source(&conflicting, 300, Some("owner")),
                Err(ShanError::InvalidObservation)
            ));
            assert_eq!(snapshot(&store), before);
        }
    }

    #[test]
    fn ignored_never_reopens_or_revokes_and_cooldown_is_retained() {
        let store = Store::open(":memory:").expect("store");
        let incoming = observed();
        store.report_signal(&incoming, 1).expect("first");
        memory(&store, &incoming, "proof", &"a".repeat(40), 100);
        store
            .ignore_issue(&fp(&incoming), "owner mute", "owner", 120)
            .expect("mute");
        assert!(matches!(
            store
                .report_signal_with_source(&incoming, 200, Some("owner"))
                .expect("muted"),
            ReportOutcome::Muted { occurrences: 2, .. }
        ));
        assert!(valid(&store, "proof"));
        assert_eq!(store.list_issues().expect("issues")[0].state, "ignored");
        assert_eq!(
            store.observations_for(&fp(&incoming), 1).expect("rows")[0].disposition,
            "muted"
        );
        state(&store, &incoming, "triggered", 100);
        store
            .lock()
            .execute("UPDATE issues SET cooldown_until=400", [])
            .expect("cooldown");
        assert!(matches!(
            store.report_signal(&incoming, 300).expect("cooldown"),
            ReportOutcome::CooldownActive {
                occurrences: 3,
                retry_after_ms: 100,
                ..
            }
        ));
        assert_eq!(
            store.observations_for(&fp(&incoming), 1).expect("rows")[0].disposition,
            "cooldown"
        );
    }

    #[test]
    fn metadata_limits_are_bytes_and_never_truncate_identity() {
        for field in 0..4 {
            let max = match field {
                0 => MAX_ENVIRONMENT_BYTES,
                1 => MAX_RELEASE_SHA_BYTES,
                2 => MAX_SOURCE_EVENT_ID_BYTES,
                _ => MAX_TRUSTED_SOURCE_BYTES,
            };
            for value in [
                "".into(),
                "  ".into(),
                "bad\nvalue".into(),
                "bad\u{7f}".into(),
                "x".repeat(max + 1),
                "é".repeat(max / 2 + 1),
            ] {
                let store = Store::open(":memory:").expect("store");
                let mut incoming = signal();
                let source = match field {
                    0 => {
                        incoming.environment = Some(value.clone());
                        None
                    }
                    1 => {
                        incoming.release_sha = Some(value.clone());
                        None
                    }
                    2 => {
                        incoming.source_event_id = Some(value.clone());
                        None
                    }
                    _ => Some(value.as_str()),
                };
                let before = snapshot(&store);
                assert!(matches!(
                    store.report_signal_with_source(&incoming, 1, source),
                    Err(ShanError::InvalidObservation)
                ));
                assert_eq!(snapshot(&store), before);
            }
        }
        let store = Store::open(":memory:").expect("store");
        let incoming = Signal {
            environment: Some("é".repeat(MAX_ENVIRONMENT_BYTES / 2)),
            release_sha: Some("r".repeat(MAX_RELEASE_SHA_BYTES)),
            source_event_id: Some("e".repeat(MAX_SOURCE_EVENT_ID_BYTES)),
            ..signal()
        };
        let source = "s".repeat(MAX_TRUSTED_SOURCE_BYTES);
        store
            .report_signal_with_source(&incoming, 1, Some(&source))
            .expect("exact limits");
        let row = &store.observations_for(&fp(&incoming), 1).expect("rows")[0];
        assert_eq!(row.environment, incoming.environment);
        assert_eq!(row.release_sha, incoming.release_sha);
        assert_eq!(row.source_event_id, incoming.source_event_id);
        assert_eq!(row.trusted_source.as_deref(), Some(source.as_str()));
    }

    #[test]
    fn retention_is_per_issue_received_then_id_and_eviction_is_not_global_dedup() {
        let store = Store::open(":memory:").expect("store");
        let mut incoming = Signal {
            source_event_id: Some("old".into()),
            ..signal()
        };
        store
            .report_signal_with_source(&incoming, 1, Some("owner"))
            .expect("first");
        let other = Signal {
            repo: "other/repo".into(),
            ..signal()
        };
        store.report_signal(&other, 1).expect("other");
        for i in 0..105 {
            incoming.source_event_id = Some(format!("event-{i}"));
            store
                .report_signal_with_source(&incoming, 200, Some("owner"))
                .expect("report");
        }
        let rows = store
            .observations_for(&fp(&incoming), u32::MAX)
            .expect("cap");
        assert_eq!(rows.len(), 100);
        assert!(rows
            .windows(2)
            .all(|p| (p[0].received_at_ms, p[0].id) > (p[1].received_at_ms, p[1].id)));
        assert_eq!(rows[0].source_event_id.as_deref(), Some("event-104"));
        assert_eq!(rows[99].source_event_id.as_deref(), Some("event-5"));
        store
            .report_signal(&signal(), 0)
            .expect("old receipt still counts");
        assert_eq!(
            store.observations_for(&fp(&incoming), 100).expect("rows"),
            rows
        );
        assert!(store
            .observations_for(&fp(&incoming), 0)
            .expect("zero")
            .is_empty());
        assert_eq!(
            store
                .observations_for(&fp(&other), 100)
                .expect("other")
                .len(),
            1
        );
        incoming.source_event_id = Some("old".into());
        state(&store, &incoming, "merged", 100);
        assert!(matches!(
            store
                .report_signal_with_source(&incoming, 300, Some("owner"))
                .expect("evicted id"),
            ReportOutcome::Regression {
                occurrences: 108,
                ..
            }
        ));
        assert_eq!(
            store
                .list_issues()
                .expect("issues")
                .iter()
                .find(|r| r.fingerprint == fp(&incoming))
                .expect("issue")
                .last_seen,
            300
        );
    }

    #[test]
    fn trusted_regression_revokes_only_exact_sha_and_strictly_older_evidence() {
        for length in [40, 64] {
            let store = Store::open(":memory:").expect("store");
            let incoming = Signal {
                release_sha: Some("A".repeat(length)),
                ..observed()
            };
            store.report_signal(&incoming, 1).expect("first");
            let sha = "a".repeat(length);
            memory(&store, &incoming, "old-exact", &sha, 100);
            memory(
                &store,
                &incoming,
                "old-exact-upper",
                &sha.to_ascii_uppercase(),
                101,
            );
            memory(&store, &incoming, "equal-exact", &sha, 150);
            memory(&store, &incoming, "newer-exact", &sha, 151);
            memory(
                &store,
                &incoming,
                "independent-commit",
                &"b".repeat(length),
                102,
            );
            let other = Signal {
                repo: "other/repo".into(),
                ..incoming.clone()
            };
            memory(&store, &other, "other-repo", &sha, 100);
            let other_fp = Signal {
                stack_trace: "panic: unrelated fault".into(),
                ..incoming.clone()
            };
            memory(&store, &other_fp, "other-fp", &sha, 100);
            state(&store, &incoming, "merged", 120);
            assert!(
                valid(&store, "old-exact"),
                "merge alone is not a deployment observation"
            );
            assert!(matches!(
                store
                    .report_signal_with_source(&incoming, 200, Some("owner-import"))
                    .expect("trusted"),
                ReportOutcome::Regression { .. }
            ));
            assert!(!valid(&store, "old-exact"));
            assert!(!valid(&store, "old-exact-upper"));
            for source in [
                "equal-exact",
                "newer-exact",
                "independent-commit",
                "other-repo",
                "other-fp",
            ] {
                assert!(valid(&store, source), "{source}");
            }
            let revoked: (i64, String) = store
                .lock()
                .query_row(
                    "SELECT revoked_at,revocation_reason FROM repair_memory_evidence
                     WHERE source_id='old-exact'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("revocation provenance");
            assert_eq!(revoked, (200, "trusted_regression".into()));
            assert_eq!(
                store.observations_for(&fp(&incoming), 1).expect("rows")[0]
                    .release_sha
                    .as_deref(),
                Some(sha.as_str())
            );
            let audits = store.events_for(&fp(&incoming), 100).expect("audits");
            assert_eq!(
                audits.iter().filter(|e| e.kind == "memory_revoked").count(),
                1
            );
            store
                .report_signal_with_source(&incoming, 300, Some("owner-import"))
                .expect("repeat");
            let still_revoked: i64 = store
                .lock()
                .query_row(
                    "SELECT revoked_at FROM repair_memory_evidence WHERE source_id='old-exact'",
                    [],
                    |r| r.get(0),
                )
                .expect("original tombstone");
            assert_eq!(still_revoked, 200);
        }
    }

    #[test]
    fn uncertain_observations_never_revoke_memory() {
        for case in 0..12 {
            let store = Store::open(":memory:").expect("store");
            let mut incoming = observed();
            store.report_signal(&incoming, 1).expect("first");
            memory(&store, &incoming, "proof", &"a".repeat(40), 100);
            let mut source = Some("owner");
            match case {
                0 => source = None,
                1 => incoming.observed_at = None,
                2 => incoming.observed_at = Some(0),
                3 => incoming.observed_at = Some(201),
                4 => incoming.observed_at = Some(100),
                5 => incoming.observed_at = Some(99),
                6 => incoming.environment = None,
                7 => incoming.release_sha = None,
                8 => incoming.release_sha = Some("a".repeat(39)),
                9 => incoming.release_sha = Some("z".repeat(40)),
                10 => incoming.release_sha = Some("a".repeat(41)),
                _ => state(&store, &incoming, "merged", 180),
            }
            store
                .report_signal_with_source(&incoming, 200, source)
                .expect("report");
            assert!(valid(&store, "proof"), "case {case}");
            assert!(!store
                .events_for(&fp(&incoming), 100)
                .expect("events")
                .iter()
                .any(|e| e.kind == "memory_revoked"));
        }
    }

    #[test]
    fn duplicates_do_not_revoke_but_disabled_evidence_still_becomes_invalid() {
        let store = Store::open(":memory:").expect("store");
        let incoming = Signal {
            source_event_id: Some("event".into()),
            ..observed()
        };
        store
            .report_signal_with_source(&incoming, 200, Some("owner"))
            .expect("first");
        memory(&store, &incoming, "proof", &"a".repeat(40), 100);
        state(&store, &incoming, "merged", 100);
        assert!(matches!(
            store
                .report_signal_with_source(&incoming, 300, Some("owner"))
                .expect("duplicate"),
            ReportOutcome::Duplicate { .. }
        ));
        assert!(valid(&store, "proof"));
        store
            .set_repair_memory_disabled(&incoming.repo, &fp(&incoming), true)
            .expect("disable");
        let next = Signal {
            source_event_id: Some("different-event".into()),
            ..incoming
        };
        store
            .report_signal_with_source(&next, 400, Some("owner"))
            .expect("disabled");
        assert!(!valid(&store, "proof"));
        let (rows, _) = store
            .list_repair_memory(&next.repo, 100, 0)
            .expect("memory");
        assert!(rows[0].disabled);
        assert_eq!(rows[0].revoked_at, Some(400));
        store
            .set_repair_memory_disabled(&next.repo, &fp(&next), false)
            .expect("owner enables summary");
        assert!(
            !valid(&store, "proof"),
            "enable cannot resurrect revoked evidence"
        );
    }

    #[test]
    fn receipt_failures_roll_back_counts_state_payload_observations_evidence_and_audits() {
        for trigger in [
            "CREATE TEMP TRIGGER fail_receipt BEFORE UPDATE ON repair_memory_evidence
             BEGIN SELECT RAISE(ABORT,'injected evidence failure'); END;",
            "CREATE TEMP TRIGGER fail_receipt BEFORE INSERT ON events
             WHEN NEW.kind='memory_revoked'
             BEGIN SELECT RAISE(ABORT,'injected revocation audit failure'); END;",
            "CREATE TEMP TRIGGER fail_receipt BEFORE INSERT ON signal_observations
             BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;",
            "CREATE TEMP TRIGGER fail_receipt BEFORE DELETE ON signal_observations
             BEGIN SELECT RAISE(ABORT,'injected retention failure'); END;",
        ] {
            let store = Store::open(":memory:").expect("store");
            for received in 1..=100 {
                store
                    .report_signal(&signal(), received)
                    .expect("fill history");
            }
            let incoming = Signal {
                severity: "critical".into(),
                log_window: vec!["updated sample".into()],
                ..observed()
            };
            memory(&store, &incoming, "proof", &"a".repeat(40), 100);
            state(&store, &incoming, "merged", 120);
            let before = snapshot(&store);
            store.lock().execute_batch(trigger).expect("inject failure");
            assert!(store
                .report_signal_with_source(&incoming, 200, Some("owner"))
                .is_err());
            assert_eq!(snapshot(&store), before, "{trigger}");
            store
                .lock()
                .execute_batch("DROP TRIGGER fail_receipt")
                .expect("remove test trigger");
            assert!(matches!(
                store
                    .report_signal_with_source(&incoming, 200, Some("owner"))
                    .expect("retry"),
                ReportOutcome::Regression {
                    occurrences: 101,
                    ..
                }
            ));
            assert!(!valid(&store, "proof"));
            assert_eq!(
                store
                    .observations_for(&fp(&incoming), 100)
                    .expect("history")
                    .len(),
                100
            );
        }
    }

    #[test]
    fn first_receipt_failure_does_not_leave_an_issue_or_memory_revocation() {
        let store = Store::open(":memory:").expect("store");
        let incoming = observed();
        memory(&store, &incoming, "proof", &"a".repeat(40), 100);
        let before = snapshot(&store);
        store
            .lock()
            .execute_batch(
                "CREATE TEMP TRIGGER fail_receipt BEFORE INSERT ON signal_observations
             BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;",
            )
            .expect("trigger");
        assert!(store
            .report_signal_with_source(&incoming, 200, Some("owner"))
            .is_err());
        assert_eq!(snapshot(&store), before);
    }

    #[test]
    fn independent_connections_serialize_trusted_event_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("observations.db");
        let first = Store::open(&path).expect("first");
        let second = Store::open(&path).expect("second");
        let incoming = Signal {
            source_event_id: Some("same-event".into()),
            ..signal()
        };
        let barrier = std::sync::Barrier::new(2);
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                first.report_signal_with_source(&incoming, 200, Some("owner"))
            });
            let b = scope.spawn(|| {
                barrier.wait();
                second.report_signal_with_source(&incoming, 200, Some("owner"))
            });
            (
                a.join().expect("join first").expect("report first"),
                b.join().expect("join second").expect("report second"),
            )
        });
        assert!(matches!(
            (&a, &b),
            (
                ReportOutcome::NewIssue { .. },
                ReportOutcome::Duplicate { occurrences: 2, .. }
            ) | (
                ReportOutcome::Duplicate { occurrences: 2, .. },
                ReportOutcome::NewIssue { .. }
            )
        ));
        let rows = first
            .observations_for(&fp(&incoming), 100)
            .expect("history");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].duplicate_of, Some(rows[1].id));
        assert_eq!(first.list_issues().expect("issues")[0].occurrences, 2);
        assert_eq!(first.reports_on(&crate::utc_day(200)).expect("daily"), 2);
    }

    #[test]
    fn prune_uses_thirty_day_receipt_age_not_observation_age() {
        let store = Store::open(":memory:").expect("store");
        let incoming = Signal {
            source_event_id: Some("expired".into()),
            ..signal()
        };
        store
            .report_signal_with_source(&incoming, 1, Some("owner"))
            .expect("expired");
        store
            .report_signal_with_source(
                &Signal {
                    observed_at: Some(1),
                    ..signal()
                },
                2,
                Some("owner"),
            )
            .expect("boundary");
        store
            .prune(SIGNAL_OBSERVATIONS_RETENTION_MS + 2, 365, 365, 365)
            .expect("prune");
        let rows = store.observations_for(&fp(&incoming), 100).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].received_at_ms, 2);
        assert_eq!(rows[0].observed_at_ms, Some(1));
        state(&store, &incoming, "merged", 100);
        assert!(matches!(
            store
                .report_signal_with_source(
                    &incoming,
                    SIGNAL_OBSERVATIONS_RETENTION_MS + 3,
                    Some("owner"),
                )
                .expect("history expired"),
            ReportOutcome::Regression { .. }
        ));
        assert_eq!(store.list_issues().expect("issues")[0].occurrences, 3);
    }
}
