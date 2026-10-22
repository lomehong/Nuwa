use rusqlite::Connection;

use crate::ShanResult;

pub const SCHEMA_VERSION: i64 = 5;

pub fn apply(conn: &mut Connection) -> ShanResult<()> {
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.execute_batch(
        "PRAGMA busy_timeout=5000;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;",
    )?;
    // v0 基线表（issues/events/heartbeat）。CREATE IF NOT EXISTS 对旧库无副作用。
    conn.execute_batch(super::SCHEMA)?;

    let mut version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < 1 {
        let tx = conn.transaction()?;
        migrate_v1(&tx)?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
        version = 1;
    }
    if version < 2 {
        let tx = conn.transaction()?;
        migrate_v2(&tx)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        version = 2;
    }
    if version < 3 {
        let tx = conn.transaction()?;
        migrate_v3(&tx)?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
        version = 3;
    }
    if version < 4 {
        let tx = conn.transaction()?;
        migrate_v4(&tx)?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
        version = 4;
    }
    if version < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        migrate_v5(&tx)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
    }
    Ok(())
}

fn migrate_v1(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "issues", "pr_url", "TEXT")?;
    add_column_if_missing(conn, "issues", "pr_opened_at", "INTEGER")?;
    add_column_if_missing(conn, "issues", "state_changed_at", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "issues", "owner_runner_id", "TEXT")?;
    add_column_if_missing(conn, "events", "repo", "TEXT")?;
    add_column_if_missing(conn, "events", "actor", "TEXT")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS runners (
            id                  TEXT PRIMARY KEY,
            repo                TEXT NOT NULL,
            host                TEXT NOT NULL,
            pid                 INTEGER,
            started_at          INTEGER NOT NULL,
            heartbeat_at        INTEGER NOT NULL,
            status              TEXT NOT NULL,
            current_fingerprint TEXT,
            phase               TEXT,
            provider            TEXT,
            model               TEXT,
            daily_pr_limit      INTEGER,
            repo_path           TEXT,
            service_unit        TEXT,
            config_json         TEXT NOT NULL DEFAULT '{}',
            last_error          TEXT
        );
        CREATE TABLE IF NOT EXISTS runs (
            id                  TEXT PRIMARY KEY,
            harness_run_id      TEXT,
            fingerprint         TEXT NOT NULL,
            runner_id           TEXT NOT NULL,
            repo                TEXT NOT NULL,
            started_at          INTEGER NOT NULL,
            finished_at         INTEGER,
            phase               TEXT NOT NULL,
            outcome             TEXT,
            ci_verdict          TEXT,
            turns               INTEGER DEFAULT 0,
            tokens_uncached     INTEGER DEFAULT 0,
            tokens_cached       INTEGER DEFAULT 0,
            provider            TEXT,
            model               TEXT,
            root_cause          TEXT,
            change_stat         TEXT,
            branch              TEXT,
            commit_sha          TEXT,
            pr_url              TEXT,
            pr_opened_at        INTEGER,
            verification_ok     INTEGER,
            verification_detail TEXT,
            session_dir         TEXT,
            error               TEXT
        );
        CREATE TABLE IF NOT EXISTS signal_daily (
            fingerprint TEXT NOT NULL,
            repo        TEXT NOT NULL,
            day         TEXT NOT NULL,
            count       INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (fingerprint, repo, day)
        );
        CREATE INDEX IF NOT EXISTS idx_issues_state_last ON issues(state, last_seen DESC);
        CREATE INDEX IF NOT EXISTS idx_issues_repo_last  ON issues(repo, last_seen DESC);
        CREATE INDEX IF NOT EXISTS idx_events_ts         ON events(ts DESC);
        CREATE INDEX IF NOT EXISTS idx_events_kind       ON events(kind, ts DESC);
        CREATE INDEX IF NOT EXISTS idx_runs_fp           ON runs(fingerprint, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_runs_started      ON runs(started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_signal_day        ON signal_daily(day, repo);",
    )?;

    conn.execute_batch(
        "UPDATE issues SET state_changed_at = COALESCE(
            (SELECT MAX(ts) FROM events
             WHERE events.fingerprint = issues.fingerprint AND events.kind = 'state_changed'),
            last_seen
        ) WHERE state_changed_at = 0;
        UPDATE events SET repo = (
            SELECT repo FROM issues WHERE issues.fingerprint = events.fingerprint
        ) WHERE repo IS NULL;",
    )?;
    Ok(())
}

/// v2：控制台仓库注册表。这里只存展示身份；仓库内容（failing_check /
/// forbidden_paths / notes）仍以目标仓 `nuwa.repo.json` 为唯一来源，只读。
fn migrate_v2(conn: &Connection) -> ShanResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS repos (
            repo        TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            url         TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );",
    )?;
    Ok(())
}

/// v3: owner mute (`ignored`), issue-level attempt counting, and the runner's
/// last real progress timestamp (heartbeat liveness is separate).
fn migrate_v3(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "issues", "attempts", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "runners", "progress_at", "INTEGER NOT NULL DEFAULT 0")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_issues_state_attempts ON issues(state, attempts);",
    )?;
    Ok(())
}

/// v4：已验证修复记忆。来源（commit/PR/时间）与内容同表保存，注入时随内容
/// 一起展示；同一 (repo, fingerprint) 只保留最新一次。
fn migrate_v4(conn: &Connection) -> ShanResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS repair_memory (
            repo        TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            commit_sha  TEXT NOT NULL,
            pr_url      TEXT,
            root_cause  TEXT NOT NULL,
            change_stat TEXT NOT NULL DEFAULT '',
            files       INTEGER NOT NULL DEFAULT 0,
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY (repo, fingerprint)
        );
        CREATE INDEX IF NOT EXISTS idx_repair_memory_repo
            ON repair_memory(repo, recorded_at DESC);",
    )?;
    Ok(())
}

/// v5：上下文预算可见。压缩次数、模型窗口与会话消息数随 run 落库——这是
/// 「为什么这次修复烧了这么多 token」的直接证据。
fn migrate_v5(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "runs", "compactions", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "runs", "context_window", "INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(conn, "runs", "messages", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> ShanResult<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    if !columns.iter().any(|c| c == column) {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .expect("query")
            > 0
    }

    fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("prepare");
        stmt.query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect")
            .iter()
            .any(|c| c == column)
    }

    fn v0_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE issues (
                fingerprint TEXT PRIMARY KEY, title TEXT NOT NULL, language TEXT NOT NULL,
                severity TEXT NOT NULL DEFAULT 'medium', state TEXT NOT NULL DEFAULT 'new',
                occurrences INTEGER NOT NULL DEFAULT 1, first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL, cooldown_until INTEGER NOT NULL DEFAULT 0,
                repo TEXT NOT NULL, stack_sample TEXT NOT NULL
            );
            CREATE TABLE events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, fingerprint TEXT NOT NULL,
                ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT
            );
            CREATE TABLE heartbeat (id INTEGER PRIMARY KEY CHECK (id = 1), ts INTEGER NOT NULL);",
        )
        .expect("v0 schema");
        conn.execute(
            "INSERT INTO issues(fingerprint,title,language,repo,stack_sample,first_seen,last_seen,state)
             VALUES ('fp1','t','go','example.com/r','stack',100,200,'fixing')",
            [],
        )
        .expect("issue");
        conn.execute(
            "INSERT INTO events(fingerprint,ts,kind,detail) VALUES ('fp1',150,'state_changed','claimed')",
            [],
        )
        .expect("event");
    }

    #[test]
    fn fresh_database_is_current() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fresh.db");
        let _store = Store::open(&path).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
        for table in ["issues", "events", "runners", "runs", "signal_daily", "repos", "repair_memory"] {
            assert!(table_exists(&conn, table), "missing {table}");
        }
        for (table, column) in [
            ("issues", "state_changed_at"),
            ("issues", "owner_runner_id"),
            ("issues", "attempts"),
            ("runners", "progress_at"),
            ("events", "repo"),
            ("events", "actor"),
            ("repos", "created_at"),
            ("repos", "updated_at"),
            ("runs", "compactions"),
            ("runs", "context_window"),
            ("runs", "messages"),
        ] {
            assert!(column_exists(&conn, table, column), "missing {table}.{column}");
        }
    }

    #[test]
    fn migrates_v0_and_backfills() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v0.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
        }
        let _store = Store::open(&path).expect("migrate");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5, "v0 must run v1 through v5");
        assert!(table_exists(&conn, "repos"));
        let (state_changed_at, owner): (i64, Option<String>) = conn
            .query_row(
                "SELECT state_changed_at, owner_runner_id FROM issues WHERE fingerprint='fp1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("issue");
        assert_eq!(state_changed_at, 150, "backfill from latest state_changed event");
        assert_eq!(owner, None);
        let repo: String = conn
            .query_row("SELECT repo FROM events WHERE fingerprint='fp1'", [], |row| row.get(0))
            .expect("event repo");
        assert_eq!(repo, "example.com/r");
        assert!(column_exists(&conn, "issues", "pr_url"));
        assert!(column_exists(&conn, "issues", "pr_opened_at"));
    }

    #[test]
    fn migrates_v1_database_to_current() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v1.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1 migration");
            conn.pragma_update(None, "user_version", 1).expect("version");
        }
        let _store = Store::open(&path).expect("migrate v2/v3/v4/v5");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
        assert!(table_exists(&conn, "repos"));
        assert!(column_exists(&conn, "issues", "attempts"));
        // v1 data survives the upgrade.
        let title: String = conn
            .query_row("SELECT title FROM issues WHERE fingerprint='fp1'", [], |row| {
                row.get(0)
            })
            .expect("issue title");
        assert_eq!(title, "t");
    }

    #[test]
    fn migrates_v2_database_to_current() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v2.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1");
            migrate_v2(&conn).expect("v2");
            conn.pragma_update(None, "user_version", 2).expect("version");
        }
        let _store = Store::open(&path).expect("migrate v3/v4/v5");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
        assert!(column_exists(&conn, "issues", "attempts"));
        assert!(column_exists(&conn, "runners", "progress_at"));
        let title: String = conn
            .query_row("SELECT title FROM issues WHERE fingerprint='fp1'", [], |row| {
                row.get(0)
            })
            .expect("issue title");
        assert_eq!(title, "t");
    }

    #[test]
    fn migrates_v3_database_to_current() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v3.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1");
            migrate_v2(&conn).expect("v2");
            migrate_v3(&conn).expect("v3");
            conn.pragma_update(None, "user_version", 3).expect("version");
        }
        let _store = Store::open(&path).expect("migrate v4/v5");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
        assert!(table_exists(&conn, "repair_memory"));
        let title: String = conn
            .query_row("SELECT title FROM issues WHERE fingerprint='fp1'", [], |row| {
                row.get(0)
            })
            .expect("issue title");
        assert_eq!(title, "t");
    }

    #[test]
    fn migrates_v4_database_to_v5() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v4.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1");
            migrate_v2(&conn).expect("v2");
            migrate_v3(&conn).expect("v3");
            migrate_v4(&conn).expect("v4");
            conn.pragma_update(None, "user_version", 4).expect("version");
        }
        let _store = Store::open(&path).expect("migrate v5");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
        for column in ["compactions", "context_window", "messages"] {
            assert!(column_exists(&conn, "runs", column), "missing runs.{column}");
        }
        assert!(table_exists(&conn, "repair_memory"));
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idem.db");
        drop(Store::open(&path).expect("first"));
        drop(Store::open(&path).expect("second"));
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 5);
    }

    #[test]
    fn wal_and_busy_timeout_are_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.db");
        let _store = Store::open(&path).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy timeout");
        assert_eq!(timeout, 5000);
    }
}
