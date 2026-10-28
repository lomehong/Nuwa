use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::{record_event_on, ShanError, ShanResult, Store};

/// Opaque revision identity only: configuration values are never stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConfigRevision {
    pub env_id: String,
    pub revision: String,
    pub saved_at_ms: i64,
}

/// Configuration save authority scoped to one environment and transaction.
/// The connection is private so callers cannot commit or release the lock.
pub struct ConfigSaveTx<'a> {
    conn: &'a Connection,
    env_id: &'a str,
}

impl ConfigSaveTx<'_> {
    pub fn current(&self) -> ShanResult<Option<SavedConfigRevision>> {
        saved_config_on(self.conn, self.env_id)
    }

    /// Record the opaque revision and one audit event per changed key. Pass
    /// key names only, never configuration values (including secret values).
    pub fn record_saved(&self, revision: &str, at: i64, changed_keys: &[String]) -> ShanResult<()> {
        self.conn.execute(
            "INSERT INTO config_saved(env_id, revision, saved_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(env_id) DO UPDATE SET revision=excluded.revision,
                saved_at_ms=excluded.saved_at_ms",
            rusqlite::params![self.env_id, revision, at],
        )?;
        for key in changed_keys {
            record_event_on(
                self.conn,
                "__config__",
                at,
                None,
                "owner",
                "config_changed",
                key,
            )?;
        }
        Ok(())
    }
}

impl Store {
    pub fn saved_config(&self, env_id: &str) -> ShanResult<Option<SavedConfigRevision>> {
        saved_config_on(&self.lock(), env_id)
    }

    /// Serialize saves across Store connections before invoking the callback.
    /// An error (or panic) rolls back revision and audit writes. Do not reenter
    /// this Store from the callback: its connection mutex stays locked. External
    /// file writes are the caller's responsibility and cannot be rolled back here.
    pub fn with_config_save<T, E>(
        &self,
        env_id: &str,
        save: impl FnOnce(&ConfigSaveTx<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<ShanError>,
    {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(ShanError::from)?;
        let result = save(&ConfigSaveTx { conn: &tx, env_id })?;
        tx.commit().map_err(ShanError::from)?;
        Ok(result)
    }

    /// Set a secret-free startup snapshot once per live lease instance. An
    /// identical retry succeeds; a different snapshot, expired or stale owner,
    /// or missing runner returns false without changing the row.
    pub fn set_runner_startup_config(
        &self,
        runner_id: &str,
        instance_id: &str,
        json: &str,
        now: i64,
    ) -> ShanResult<bool> {
        let conn = self.lock();
        Ok(conn.execute(
            "UPDATE runners SET startup_config_json=?3
             WHERE id=?1 AND instance_id=?2 AND lease_until > ?4
               AND (startup_config_json IS NULL OR startup_config_json=?3)",
            rusqlite::params![runner_id, instance_id, json, now],
        )? == 1)
    }
}

fn saved_config_on(conn: &Connection, env_id: &str) -> ShanResult<Option<SavedConfigRevision>> {
    Ok(conn
        .query_row(
            "SELECT env_id, revision, saved_at_ms FROM config_saved WHERE env_id=?1",
            [env_id],
            |row| {
                Ok(SavedConfigRevision {
                    env_id: row.get(0)?,
                    revision: row.get(1)?,
                    saved_at_ms: row.get(2)?,
                })
            },
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Debug)]
    enum SaveError {
        Store(ShanError),
        Aborted,
    }

    impl From<ShanError> for SaveError {
        fn from(error: ShanError) -> Self {
            Self::Store(error)
        }
    }

    #[test]
    fn revisions_are_environment_scoped_and_audits_are_key_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.db");
        let store = Store::open(&path).expect("open");
        assert!(store.saved_config("production").expect("unknown").is_none());
        let revision = uuid::Uuid::new_v4().to_string();
        let returned = store
            .with_config_save("production", |tx| -> ShanResult<_> {
                assert!(tx.current()?.is_none());
                tx.record_saved(&revision, 10, &["MODEL".into(), "API_KEY".into()])?;
                assert_eq!(
                    tx.current()?.expect("uncommitted revision").revision,
                    revision
                );
                Ok(42)
            })
            .expect("save");
        assert_eq!(returned, 42);
        store
            .with_config_save("staging", |tx| tx.record_saved("stage", 11, &[]))
            .expect("other environment");
        store
            .with_config_save("production", |tx| -> ShanResult<()> {
                assert_eq!(tx.current()?.expect("previous").revision, revision);
                tx.record_saved("next", 12, &[])
            })
            .expect("upsert without changed keys");
        drop(store);
        let store = Store::open(&path).expect("reopen");
        assert_eq!(
            store.saved_config("production").expect("saved"),
            Some(SavedConfigRevision {
                env_id: "production".into(),
                revision: "next".into(),
                saved_at_ms: 12,
            })
        );
        assert_eq!(
            store
                .saved_config("staging")
                .expect("staging")
                .expect("row")
                .revision,
            "stage"
        );
        let events = store.events_for("__config__", 10).expect("audit");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].detail, "API_KEY");
        assert_eq!(events[1].detail, "MODEL");
        for event in events {
            assert_eq!(event.actor.as_deref(), Some("owner"));
            assert_eq!(event.kind, "config_changed");
            assert_eq!(event.ts, 10);
            assert!(event.repo.is_empty());
        }
    }

    #[test]
    fn callback_errors_roll_back_new_and_existing_revisions_and_audits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollback.db");
        let store = Store::open(&path).expect("writer");
        let observer = Store::open(&path).expect("independent observer");
        for existing in [false, true] {
            if existing {
                store
                    .with_config_save("env", |tx| tx.record_saved("original", 1, &[]))
                    .expect("seed revision");
            }
            let before = observer.saved_config("env").expect("before");
            let result: Result<(), SaveError> = store.with_config_save("env", |tx| {
                assert_eq!(tx.current()?, before);
                tx.record_saved("discarded", 2, &["MODEL".into(), "API_KEY".into()])?;
                assert_eq!(tx.current()?.expect("pending").revision, "discarded");
                Err(SaveError::Aborted)
            });
            assert!(matches!(result, Err(SaveError::Aborted)));
            assert_eq!(observer.saved_config("env").expect("after"), before);
            assert!(observer
                .events_for("__config__", 10)
                .expect("audit")
                .is_empty());
        }
    }

    #[test]
    fn audit_failure_propagates_and_rolls_back_partial_save() {
        let store = Store::open(":memory:").expect("store");
        store
            .lock()
            .execute_batch(
                "CREATE TRIGGER fail_config_audit BEFORE INSERT ON events
                 WHEN NEW.kind='config_changed' AND NEW.detail='SECOND'
                 BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
            )
            .expect("audit failure fixture");
        let result: Result<(), SaveError> = store.with_config_save("env", |tx| {
            tx.record_saved("discarded", 2, &["FIRST".into(), "SECOND".into()])?;
            Ok(())
        });
        assert!(matches!(result, Err(SaveError::Store(ShanError::Db(_)))));
        assert!(store.saved_config("env").expect("rollback").is_none());
        assert!(store
            .events_for("__config__", 10)
            .expect("audit")
            .is_empty());
        store
            .lock()
            .execute_batch("DROP TRIGGER fail_config_audit")
            .expect("unblock");
        store
            .with_config_save("env", |tx| tx.record_saved("retry", 3, &["MODEL".into()]))
            .expect("lock and transaction released on error");
    }

    #[test]
    fn independent_connections_serialize_the_entire_save_callback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("serialized.db");
        let first = Store::open(&path).expect("first");
        let second = Store::open(&path).expect("second");
        let observer = Store::open(&path).expect("observer");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            first.with_config_save("env", |tx| -> ShanResult<()> {
                assert!(tx.current()?.is_none());
                tx.record_saved("first", 1, &["FIRST".into()])?;
                entered_tx.send(()).expect("notify callback");
                release_rx
                    .recv_timeout(Duration::from_secs(3))
                    .expect("release callback");
                Ok(())
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("first callback entered");
        let (started_tx, started_rx) = mpsc::channel();
        let (contender_tx, contender_rx) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            started_tx.send(()).expect("notify attempt");
            second.with_config_save("env", |tx| -> ShanResult<()> {
                contender_tx.send(()).expect("notify callback entry");
                assert_eq!(
                    tx.current()?.expect("serialized revision").revision,
                    "first"
                );
                tx.record_saved("second", 2, &["SECOND".into()])
            })
        });
        started_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("second attempting");
        assert!(matches!(
            contender_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(observer
            .saved_config("env")
            .expect("uncommitted revision hidden")
            .is_none());
        assert!(observer
            .events_for("__config__", 10)
            .expect("uncommitted audit hidden")
            .is_empty());
        release_tx.send(()).expect("release first");
        writer
            .join()
            .expect("first thread")
            .expect("first committed");
        contender
            .join()
            .expect("second thread")
            .expect("second committed");
        contender_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("second callback ran");
        assert_eq!(
            observer
                .saved_config("env")
                .expect("final")
                .expect("revision")
                .revision,
            "second"
        );
        let events = observer
            .events_for("__config__", 10)
            .expect("committed audits");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].detail, "SECOND");
        assert_eq!(events[1].detail, "FIRST");
    }
}
