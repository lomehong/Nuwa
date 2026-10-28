use rusqlite::Connection;

use crate::{ShanError, ShanResult};

pub const SCHEMA_VERSION: i64 = 14;

pub fn apply(conn: &mut Connection) -> ShanResult<()> {
    // Reject a newer database before even changing its journal mode or schema.
    let mut version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(ShanError::UnsupportedSchema(version));
    }
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.execute_batch(
        "PRAGMA busy_timeout=5000;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;",
    )?;
    // v0 基线表（issues/events/heartbeat）。CREATE IF NOT EXISTS 对旧库无副作用。
    conn.execute_batch(super::SCHEMA)?;

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
    if version < 5 {
        let tx = conn.transaction()?;
        migrate_v5(&tx)?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
        version = 5;
    }
    if version < 6 {
        let tx = conn.transaction()?;
        migrate_v6(&tx)?;
        tx.pragma_update(None, "user_version", 6)?;
        tx.commit()?;
        version = 6;
    }
    if version < 7 {
        let tx = conn.transaction()?;
        migrate_v7(&tx)?;
        tx.pragma_update(None, "user_version", 7)?;
        tx.commit()?;
        version = 7;
    }
    if version < 8 {
        let tx = conn.transaction()?;
        migrate_v8(&tx)?;
        tx.pragma_update(None, "user_version", 8)?;
        tx.commit()?;
        version = 8;
    }
    if version < 9 {
        let tx = conn.transaction()?;
        migrate_v9(&tx)?;
        tx.pragma_update(None, "user_version", 9)?;
        tx.commit()?;
        version = 9;
    }
    if version < 10 {
        let tx = conn.transaction()?;
        migrate_v10(&tx)?;
        tx.pragma_update(None, "user_version", 10)?;
        tx.commit()?;
        version = 10;
    }
    if version < 11 {
        let tx = conn.transaction()?;
        migrate_v11(&tx)?;
        tx.pragma_update(None, "user_version", 11)?;
        tx.commit()?;
        version = 11;
    }
    if version < 12 {
        let tx = conn.transaction()?;
        migrate_v12(&tx)?;
        tx.pragma_update(None, "user_version", 12)?;
        tx.commit()?;
        version = 12;
    }
    if version < 13 {
        let tx = conn.transaction()?;
        migrate_v13(&tx)?;
        tx.pragma_update(None, "user_version", 13)?;
        tx.commit()?;
        version = 13;
    }
    if version < 14 {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        migrate_v14(&tx)?;
        tx.pragma_update(None, "user_version", 14)?;
        tx.commit()?;
    }
    Ok(())
}

fn migrate_v1(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "issues", "pr_url", "TEXT")?;
    add_column_if_missing(conn, "issues", "pr_opened_at", "INTEGER")?;
    add_column_if_missing(
        conn,
        "issues",
        "state_changed_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
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

/// v6：修复记忆可停用（注入跳过但保留来源），以及修复基准运行与逐场景结果。
fn migrate_v6(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(
        conn,
        "repair_memory",
        "disabled",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS benchmark_runs (
            id              TEXT PRIMARY KEY,
            started_at      INTEGER NOT NULL,
            finished_at     INTEGER,
            provider        TEXT NOT NULL,
            model           TEXT,
            mode            TEXT NOT NULL,
            status          TEXT NOT NULL,
            scenarios       INTEGER NOT NULL DEFAULT 0,
            repaired        INTEGER NOT NULL DEFAULT 0,
            claimed         INTEGER NOT NULL DEFAULT 0,
            false_positive  INTEGER NOT NULL DEFAULT 0,
            delivery_ok     INTEGER NOT NULL DEFAULT 0,
            turns           INTEGER NOT NULL DEFAULT 0,
            tokens_uncached INTEGER NOT NULL DEFAULT 0,
            tokens_cached   INTEGER NOT NULL DEFAULT 0,
            duration_ms     INTEGER NOT NULL DEFAULT 0,
            error           TEXT
        );
        CREATE TABLE IF NOT EXISTS benchmark_results (
            run_id          TEXT NOT NULL,
            scenario        TEXT NOT NULL,
            language        TEXT NOT NULL,
            expected_fixed  INTEGER NOT NULL,
            repaired        INTEGER NOT NULL,
            claimed_fixed   INTEGER NOT NULL,
            delivery_ok     INTEGER NOT NULL,
            turns           INTEGER NOT NULL DEFAULT 0,
            tokens_uncached INTEGER NOT NULL DEFAULT 0,
            tokens_cached   INTEGER NOT NULL DEFAULT 0,
            duration_ms     INTEGER NOT NULL DEFAULT 0,
            root_cause      TEXT,
            final_report    TEXT,
            error           TEXT,
            PRIMARY KEY (run_id, scenario),
            FOREIGN KEY (run_id) REFERENCES benchmark_runs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_runs_started
            ON benchmark_runs(started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_benchmark_results_run
            ON benchmark_results(run_id);",
    )?;
    Ok(())
}

/// v7：采集器的日志窗口（JSON 数组，有界）。修复 prompt 会把它展示给 agent，
/// 所以必须随问题单落库——此前 `Signal.log_window` 字段存在，但从未被接收。
fn migrate_v7(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "issues", "log_sample", "TEXT")?;
    Ok(())
}

/// v8：runner 单实例租约。两个同 id 的活进程会在启动时按 id 互相「孤儿回收」，
/// 重复修同一张单、双推同一分支；租约把「同 id 只能有一个活进程」变成启动
/// 前置条件，心跳续租，崩溃后最迟一个 TTL 失效。
fn migrate_v8(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "runners", "instance_id", "TEXT")?;
    add_column_if_missing(conn, "runners", "lease_until", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

/// v9：交付 intent 与持续 CI 观察。交付记录让“远端已成功、本地未落库”的
/// 断点可恢复；CI watch 让观察生命周期脱离一次修复进程，serve 重启后继续。
fn migrate_v9(conn: &Connection) -> ShanResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS deliveries (
            id          TEXT PRIMARY KEY,
            run_id      TEXT,
            fingerprint TEXT NOT NULL,
            repo        TEXT NOT NULL,
            branch      TEXT NOT NULL,
            base        TEXT NOT NULL,
            head_sha    TEXT NOT NULL,
            state       TEXT NOT NULL,
            pr_url      TEXT,
            pr_number   INTEGER,
            detail      TEXT,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL,
            UNIQUE(repo, branch, head_sha)
        );
        CREATE INDEX IF NOT EXISTS idx_deliveries_pending
            ON deliveries(state, updated_at);
        CREATE INDEX IF NOT EXISTS idx_deliveries_issue
            ON deliveries(fingerprint, created_at DESC);

        CREATE TABLE IF NOT EXISTS ci_watches (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id          TEXT,
            fingerprint     TEXT NOT NULL,
            repo            TEXT NOT NULL,
            pr_url          TEXT NOT NULL,
            slug            TEXT NOT NULL,
            pr_number       INTEGER NOT NULL,
            head_sha        TEXT NOT NULL,
            state           TEXT NOT NULL DEFAULT 'pending',
            last_verdict    TEXT,
            last_checked_at INTEGER,
            next_check_at   INTEGER NOT NULL,
            check_attempts  INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL,
            terminal_at     INTEGER,
            last_error      TEXT,
            root_cause      TEXT,
            change_stat     TEXT,
            memory_written  INTEGER NOT NULL DEFAULT 0,
            max_attempts    INTEGER NOT NULL DEFAULT 3,
            UNIQUE(repo, pr_url, head_sha)
        );
        CREATE INDEX IF NOT EXISTS idx_ci_watches_due
            ON ci_watches(state, next_check_at);
        CREATE INDEX IF NOT EXISTS idx_ci_watches_issue
            ON ci_watches(fingerprint, created_at DESC);",
    )?;
    Ok(())
}

/// v10: fenced delivery ownership and checkout exclusion. Released rows retain
/// their generation so an old holder can never regain authority by renewal.
fn migrate_v10(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "deliveries", "claim_owner", "TEXT")?;
    add_column_if_missing(conn, "deliveries", "claim_token", "TEXT")?;
    add_column_if_missing(
        conn,
        "deliveries",
        "claim_generation",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "deliveries",
        "claim_until",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_deliveries_claim
            ON deliveries(repo, state, claim_until, created_at);
         CREATE TABLE IF NOT EXISTS checkout_leases (
            path        TEXT PRIMARY KEY,
            owner       TEXT,
            token       TEXT,
            generation  INTEGER NOT NULL DEFAULT 0,
            lease_until INTEGER NOT NULL DEFAULT 0,
            runner_id   TEXT,
            runner_instance_id TEXT
         );",
    )?;
    add_column_if_missing(conn, "checkout_leases", "runner_id", "TEXT")?;
    add_column_if_missing(conn, "checkout_leases", "runner_instance_id", "TEXT")?;
    Ok(())
}

/// v11: CI ownership is independent of scheduling; evidence retains its own
/// content and permanent revocation even when a newer source replaces a summary.
fn migrate_v11(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "ci_watches", "claim_token", "TEXT")?;
    add_column_if_missing(
        conn,
        "ci_watches",
        "claim_generation",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "ci_watches",
        "claim_until",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_ci_watches_identity
            ON ci_watches(slug, pr_number, head_sha);
         CREATE INDEX IF NOT EXISTS idx_ci_watches_claim
            ON ci_watches(state, next_check_at, claim_until);
         CREATE TABLE repair_memory_evidence (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            source_kind TEXT NOT NULL CHECK(source_kind IN ('legacy_unknown','local_verification','ci_green')),
            source_id TEXT NOT NULL,
            commit_sha TEXT NOT NULL,
            pr_url TEXT,
            root_cause TEXT NOT NULL,
            change_stat TEXT NOT NULL DEFAULT '',
            files INTEGER NOT NULL DEFAULT 0,
            recorded_at INTEGER NOT NULL,
            valid INTEGER NOT NULL DEFAULT 1,
            revoked_at INTEGER,
            revocation_reason TEXT,
            UNIQUE(repo, fingerprint, source_kind, source_id, commit_sha)
         );
         CREATE INDEX idx_repair_memory_evidence_lookup
            ON repair_memory_evidence(repo, fingerprint, valid, recorded_at DESC);
         INSERT INTO repair_memory_evidence
            (repo,fingerprint,source_kind,source_id,commit_sha,pr_url,root_cause,
             change_stat,files,recorded_at)
         SELECT repo,fingerprint,'legacy_unknown','legacy',commit_sha,pr_url,
            root_cause,change_stat,files,recorded_at FROM repair_memory;",
    )?;
    Ok(())
}

/// v12: saved configuration revisions and per-instance runner startup snapshots.
/// Neither revision metadata nor key-only audits contain configuration values.
fn migrate_v12(conn: &Connection) -> ShanResult<()> {
    add_column_if_missing(conn, "runners", "startup_config_json", "TEXT")?;
    conn.execute_batch(
        "CREATE TABLE config_saved (
            env_id TEXT PRIMARY KEY,
            revision TEXT NOT NULL,
            saved_at_ms INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

/// v13: bounded receipt history, separate from issue identity. Historical
/// issues have no fabricated observations and source authority is caller-only.
fn migrate_v13(conn: &Connection) -> ShanResult<()> {
    conn.execute_batch(
        "CREATE TABLE signal_observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fingerprint TEXT NOT NULL,
            received_at_ms INTEGER NOT NULL,
            observed_at_ms INTEGER,
            environment TEXT,
            release_sha TEXT,
            source_event_id TEXT,
            trusted_source TEXT,
            time_status TEXT NOT NULL CHECK(time_status IN
                ('missing','invalid','future','untrusted','trusted')),
            disposition TEXT NOT NULL CHECK(disposition IN
                ('new','deduped','cooldown','muted','regression','delayed','duplicate')),
            duplicate_of INTEGER
         );
         CREATE INDEX idx_signal_observations_issue
            ON signal_observations(fingerprint, received_at_ms DESC, id DESC);
         CREATE INDEX idx_signal_observations_source
            ON signal_observations(fingerprint, trusted_source, source_event_id);
         CREATE INDEX idx_signal_observations_received
            ON signal_observations(received_at_ms);",
    )?;
    Ok(())
}

/// v14: durable daily PR admissions, independent of delivery lease expiry and
/// actual PR creation metrics. Multiple deliveries can confirm the same PR.
/// Legacy pr_opened_at is local observation: it dates admission/confirmation,
/// but never supplies the slot's GitHub creation timestamp.
fn migrate_v14(conn: &Connection) -> ShanResult<()> {
    conn.execute_batch(
        "CREATE TABLE pr_slots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo TEXT NOT NULL,
            evidence_identity TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN
                ('reserved','requested','ambiguous','consumed','released')),
            admitted_at_ms INTEGER NOT NULL,
            admission_day_start_ms INTEGER NOT NULL,
            pr_url TEXT,
            created_at_ms INTEGER,
            confirmed_at_ms INTEGER,
            UNIQUE(repo, evidence_identity),
            CHECK(pr_url IS NULL OR state = 'consumed')
         );
         CREATE UNIQUE INDEX idx_pr_slots_url ON pr_slots(repo, pr_url)
            WHERE pr_url IS NOT NULL;
         CREATE INDEX idx_pr_slots_day ON pr_slots(repo, admission_day_start_ms, state);
         CREATE TABLE pr_slot_deliveries (
            delivery_id TEXT PRIMARY KEY REFERENCES deliveries(id),
            slot_id INTEGER NOT NULL REFERENCES pr_slots(id)
         );
         CREATE INDEX idx_pr_slot_deliveries_slot ON pr_slot_deliveries(slot_id);

         INSERT INTO pr_slots(repo,evidence_identity,state,admitted_at_ms,
            admission_day_start_ms,pr_url,created_at_ms,confirmed_at_ms)
         WITH evidence(repo,identity,pr_url,opened,recorded) AS (
            SELECT repo,CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL
                THEN 'pr:' || pr_url ELSE 'run:' || id END,
                CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL THEN pr_url END,
                pr_opened_at,COALESCE(pr_opened_at,started_at)
            FROM runs WHERE NULLIF(TRIM(pr_url),'') IS NOT NULL OR pr_opened_at IS NOT NULL
            UNION ALL
            SELECT repo,CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL
                THEN 'pr:' || pr_url ELSE 'issue:' || fingerprint END,
                CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL THEN pr_url END,
                pr_opened_at,COALESCE(pr_opened_at,first_seen)
            FROM issues WHERE NULLIF(TRIM(pr_url),'') IS NOT NULL OR pr_opened_at IS NOT NULL
            UNION ALL
            SELECT repo,CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL
                THEN 'pr:' || pr_url ELSE 'delivery:' || id END,
                CASE WHEN NULLIF(TRIM(pr_url),'') IS NOT NULL THEN pr_url END,NULL,created_at
            FROM deliveries WHERE state='pr_created'
         ), grouped AS (
            SELECT repo,identity,MAX(pr_url) AS pr_url,
                MIN(recorded) AS confirmed,COALESCE(MIN(opened),MIN(recorded)) AS admitted
            FROM evidence GROUP BY repo,identity
         )
         SELECT repo,identity,'consumed',admitted,
            admitted - ((admitted % 86400000 + 86400000) % 86400000),
            pr_url,NULL,confirmed FROM grouped;

         INSERT INTO pr_slot_deliveries(delivery_id,slot_id)
         SELECT d.id,s.id FROM deliveries d JOIN pr_slots s ON s.repo=d.repo
            AND s.evidence_identity=CASE WHEN NULLIF(TRIM(d.pr_url),'') IS NOT NULL
                THEN 'pr:' || d.pr_url ELSE 'delivery:' || d.id END
         WHERE d.state IN ('pr_created','pr_requested','ambiguous');

         INSERT INTO pr_slots(repo,evidence_identity,state,admitted_at_ms,admission_day_start_ms)
         SELECT repo,'delivery:' || id,'ambiguous',created_at,
            created_at - ((created_at % 86400000 + 86400000) % 86400000)
         FROM deliveries d WHERE state IN ('pr_requested','ambiguous')
            AND NOT EXISTS(SELECT 1 FROM pr_slot_deliveries m WHERE m.delivery_id=d.id);
         INSERT INTO pr_slot_deliveries(delivery_id,slot_id)
         SELECT d.id,s.id FROM deliveries d JOIN pr_slots s ON s.repo=d.repo
            AND s.evidence_identity='delivery:' || d.id
         WHERE NOT EXISTS(SELECT 1 FROM pr_slot_deliveries m WHERE m.delivery_id=d.id);",
    )?;
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

    #[test]
    fn v9_adds_deliveries_and_ci_watches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("s.db")).expect("open");
        drop(store);
        let conn = Connection::open(dir.path().join("s.db")).expect("reopen");
        assert!(table_exists(&conn, "deliveries"));
        assert!(table_exists(&conn, "ci_watches"));
        for column in [
            "run_id",
            "fingerprint",
            "repo",
            "branch",
            "base",
            "head_sha",
            "state",
            "pr_url",
            "pr_number",
            "detail",
            "created_at",
            "updated_at",
        ] {
            assert!(column_exists(&conn, "deliveries", column), "{column}");
        }
        for column in [
            "run_id",
            "fingerprint",
            "repo",
            "pr_url",
            "slug",
            "pr_number",
            "head_sha",
            "state",
            "last_verdict",
            "last_checked_at",
            "next_check_at",
            "check_attempts",
            "created_at",
            "updated_at",
            "terminal_at",
            "last_error",
            "root_cause",
            "change_stat",
            "memory_written",
            "max_attempts",
        ] {
            assert!(column_exists(&conn, "ci_watches", column), "{column}");
        }
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
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

    fn v9_fixture(conn: &Connection) {
        v0_schema(conn);
        for migrate in [
            migrate_v1, migrate_v2, migrate_v3, migrate_v4, migrate_v5, migrate_v6, migrate_v7,
            migrate_v8, migrate_v9,
        ] {
            migrate(conn).expect("historical migration");
        }
        conn.execute_batch(
            "INSERT INTO deliveries(id,run_id,fingerprint,repo,branch,base,head_sha,state,
                pr_url,pr_number,detail,created_at,updated_at)
             VALUES ('d1','r1','fp1','example.com/r','agent/fix/fp1','main','abc','ambiguous',
                'https://github.com/o/r/pull/1',1,'uncertain',10,20);
             INSERT INTO ci_watches(run_id,fingerprint,repo,pr_url,slug,pr_number,head_sha,
                next_check_at,created_at,updated_at,state,check_attempts)
             VALUES ('r1','fp1','example.com/r','https://github.com/o/r/pull/1','o/r',1,
                'abc',30,10,20,'green',2);
             INSERT INTO runners(id,repo,host,started_at,heartbeat_at,status,instance_id,lease_until)
             VALUES ('runner','example.com/r','host',1,2,'working','instance',60);
             INSERT INTO runs(id,fingerprint,runner_id,repo,started_at,phase,commit_sha)
             VALUES ('r1','fp1','runner','example.com/r',1,'deliver','abc');
             PRAGMA user_version=9;",
        ).expect("v9 data");
    }

    #[test]
    fn v9_to_v10_preserves_data_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v9.db");
        {
            let conn = Connection::open(&path).expect("fixture");
            v9_fixture(&conn);
        }
        for _ in 0..2 {
            drop(Store::open(&path).expect("upgrade/reopen"));
            let conn = Connection::open(&path).expect("inspect");
            let version: i64 = conn
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .expect("version");
            assert_eq!(version, SCHEMA_VERSION);
            for table in [
                "issues",
                "events",
                "runs",
                "runners",
                "deliveries",
                "ci_watches",
            ] {
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .expect("count");
                assert_eq!(count, 1, "{table}");
            }
            let delivery: (String, String, String, i64, i64, i64, Option<String>) = conn.query_row(
                "SELECT state,head_sha,detail,created_at,updated_at,claim_generation,claim_token
                 FROM deliveries WHERE id='d1'", [],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
            ).expect("delivery");
            assert_eq!(
                delivery,
                (
                    "ambiguous".into(),
                    "abc".into(),
                    "uncertain".into(),
                    10,
                    20,
                    0,
                    None
                )
            );
            let watch: (String, i64) = conn
                .query_row("SELECT state,check_attempts FROM ci_watches", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .expect("watch");
            assert_eq!(watch, ("green".into(), 2));
            let runner: (String, i64) = conn
                .query_row("SELECT instance_id,lease_until FROM runners", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .expect("runner");
            assert_eq!(runner, ("instance".into(), 60));
            let run: (String, String) = conn
                .query_row("SELECT phase,commit_sha FROM runs WHERE id='r1'", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .expect("run");
            assert_eq!(run, ("deliver".into(), "abc".into()));
            let pr: (String, i64) = conn
                .query_row(
                    "SELECT pr_url,pr_number FROM deliveries WHERE id='d1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("PR");
            assert_eq!(pr, ("https://github.com/o/r/pull/1".into(), 1));
            assert!(table_exists(&conn, "checkout_leases"));
            for column in ["runner_id", "runner_instance_id"] {
                assert!(column_exists(&conn, "checkout_leases", column), "{column}");
            }
        }
    }

    #[test]
    fn v10_keeps_existing_standalone_checkout_provenance_null() {
        let mut conn = Connection::open_in_memory().expect("fixture");
        v9_fixture(&conn);
        conn.execute_batch(
            "CREATE TABLE checkout_leases (
                path TEXT PRIMARY KEY, owner TEXT, token TEXT,
                generation INTEGER NOT NULL DEFAULT 0, lease_until INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO checkout_leases(path,owner,token,generation,lease_until)
             VALUES ('/repo','standalone','fixture-token',4,200);",
        )
        .expect("existing checkout");
        apply(&mut conn).expect("v10");
        let saved: (String, i64, i64, bool, bool) = conn
            .query_row(
                "SELECT owner,generation,lease_until,runner_id IS NULL,runner_instance_id IS NULL
             FROM checkout_leases WHERE path='/repo'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("preserved checkout");
        assert_eq!(saved, ("standalone".into(), 4, 200, true, true));
    }

    #[test]
    fn v10_failure_rolls_back_columns_and_version() {
        let mut conn = Connection::open_in_memory().expect("fixture");
        v9_fixture(&conn);
        // Fail after ALTER TABLE has run, not before the migration starts.
        conn.execute_batch("CREATE TABLE idx_deliveries_claim(blocker INTEGER)")
            .expect("blocker");
        assert!(apply(&mut conn).is_err());
        assert!(!column_exists(&conn, "deliveries", "claim_owner"));
        assert!(!column_exists(&conn, "deliveries", "claim_generation"));
        assert!(!table_exists(&conn, "checkout_leases"));
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("version");
        assert_eq!(version, 9);
        let state: String = conn
            .query_row("SELECT state FROM deliveries WHERE id='d1'", [], |r| {
                r.get(0)
            })
            .expect("preserved");
        assert_eq!(state, "ambiguous");
        conn.execute_batch("DROP TABLE idx_deliveries_claim")
            .expect("remove fixture blocker");
        apply(&mut conn).expect("retry migration");
        assert!(column_exists(&conn, "deliveries", "claim_generation"));
    }

    #[test]
    fn v10_to_v11_preserves_legacy_memory_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v10.db");
        {
            let conn = Connection::open(&path).expect("fixture");
            v9_fixture(&conn);
            migrate_v10(&conn).expect("v10");
            conn.execute_batch(
                "INSERT INTO repair_memory(repo,fingerprint,commit_sha,pr_url,root_cause,
                    change_stat,files,recorded_at,disabled)
                 VALUES ('example.com/r','fp1','old-sha','https://github.com/o/r/pull/1',
                    'legacy cause','one file',1,42,1);
                 PRAGMA user_version=10;",
            )
            .expect("legacy memory");
        }
        for _ in 0..2 {
            let store = Store::open(&path).expect("upgrade/reopen");
            let (rows, total) = store
                .list_repair_memory("example.com/r", 10, 0)
                .expect("list");
            assert_eq!(total, 1);
            let row = &rows[0];
            assert_eq!(row.commit_sha, "old-sha");
            assert_eq!(row.pr_url.as_deref(), Some("https://github.com/o/r/pull/1"));
            assert_eq!(row.root_cause, "legacy cause");
            assert_eq!(row.change_stat, "one file");
            assert_eq!(row.recorded_at, 42);
            assert_eq!(row.files, 1);
            assert!(row.disabled);
            assert!(!row.evidence_valid);
            assert_eq!(row.verification_source, "legacy_unknown");
            assert!(row.revoked_at.is_none());
            assert!(row.revocation_reason.is_none());
            store
                .set_repair_memory_disabled("example.com/r", "fp1", false)
                .expect("enable");
            assert!(store
                .recent_repair_memory("example.com/r", "", 10)
                .expect("prompt")
                .is_empty());
            store
                .set_repair_memory_disabled("example.com/r", "fp1", true)
                .expect("restore flag");
            drop(store);
            let conn = Connection::open(&path).expect("inspect");
            let fence: (i64, i64, bool) = conn
                .query_row(
                    "SELECT claim_generation,claim_until,claim_token IS NULL FROM ci_watches",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("unclaimed old watch");
            assert_eq!(fence, (0, 0, true));
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                    .expect("version"),
                SCHEMA_VERSION
            );
        }
    }

    #[test]
    fn v11_failure_rolls_back_columns_evidence_and_version() {
        let mut conn = Connection::open_in_memory().expect("fixture");
        v9_fixture(&conn);
        migrate_v10(&conn).expect("v10");
        conn.execute_batch(
            "PRAGMA user_version=10; CREATE TABLE idx_repair_memory_evidence_lookup(x INTEGER)",
        )
        .expect("blocker after evidence creation");
        assert!(apply(&mut conn).is_err());
        assert!(!column_exists(&conn, "ci_watches", "claim_token"));
        assert!(!column_exists(&conn, "ci_watches", "claim_generation"));
        assert!(!table_exists(&conn, "repair_memory_evidence"));
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("version");
        assert_eq!(version, 10);
        conn.execute_batch("DROP TABLE idx_repair_memory_evidence_lookup")
            .expect("unblock");
        apply(&mut conn).expect("retry");
        assert!(column_exists(&conn, "ci_watches", "claim_until"));
        assert!(table_exists(&conn, "repair_memory_evidence"));
    }

    fn v11_fixture(conn: &Connection) {
        v9_fixture(conn);
        migrate_v10(conn).expect("v10");
        conn.execute_batch(
            "INSERT INTO repair_memory(repo,fingerprint,commit_sha,root_cause,recorded_at,disabled)
             VALUES ('example.com/r','fp1','abc','preserved summary',40,1);
             UPDATE deliveries SET claim_owner='delivery-owner',claim_token='delivery-fixture',
                claim_generation=4,claim_until=100 WHERE id='d1';
             INSERT INTO checkout_leases(path,owner,token,generation,lease_until,
                runner_id,runner_instance_id)
             VALUES ('/repo','runner','checkout-fixture',5,200,'runner','instance');",
        )
        .expect("batch1 state");
        migrate_v11(conn).expect("v11");
        conn.execute_batch(
            "UPDATE ci_watches SET claim_token='ci-fixture',claim_generation=7,claim_until=300,
                last_verdict='green',last_checked_at=25,root_cause='watch cause',
                change_stat='watch change',memory_written=1;
             INSERT INTO repair_memory_evidence
                (repo,fingerprint,source_kind,source_id,commit_sha,pr_url,root_cause,
                 change_stat,files,recorded_at,valid,revoked_at,revocation_reason)
             VALUES ('example.com/r','fp1','ci_green','watch-1','abc',
                'https://github.com/o/r/pull/1','revoked cause','revoked change',2,41,
                0,50,'ci_red');
             INSERT INTO repair_memory_evidence
                (repo,fingerprint,source_kind,source_id,commit_sha,root_cause,
                 change_stat,files,recorded_at)
             VALUES ('example.com/r','fp1','local_verification','run-local','abc',
                'independent cause','independent change',3,42);
             PRAGMA user_version=11;",
        )
        .expect("batch2 state");
    }

    const V11_TABLES: [&str; 9] = [
        "issues",
        "events",
        "runs",
        "runners",
        "deliveries",
        "checkout_leases",
        "ci_watches",
        "repair_memory",
        "repair_memory_evidence",
    ];

    /// Compare every historical value, not just counts or the new columns.
    fn v11_rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let projection = if table == "runners" {
            "id,repo,host,pid,started_at,heartbeat_at,status,current_fingerprint,
             phase,provider,model,daily_pr_limit,repo_path,service_unit,config_json,
             last_error,progress_at,instance_id,lease_until"
        } else {
            "*"
        };
        let mut stmt = conn
            .prepare(&format!("SELECT {projection} FROM {table} ORDER BY rowid"))
            .expect("snapshot statement");
        let columns = stmt.column_count();
        stmt.query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
            .expect("snapshot query")
            .collect::<Result<_, _>>()
            .expect("snapshot rows")
    }

    #[test]
    fn v11_to_v12_preserves_fences_ci_evidence_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v11.db");
        let before: Vec<_> = {
            let conn = Connection::open(&path).expect("fixture");
            v11_fixture(&conn);
            V11_TABLES
                .iter()
                .map(|table| v11_rows(&conn, table))
                .collect()
        };
        for _ in 0..2 {
            let store = Store::open(&path).expect("upgrade/reopen");
            assert!(store
                .saved_config("env")
                .expect("no saved revision")
                .is_none());
            assert!(store.list_runners().expect("legacy runner")[0]
                .startup_config_json
                .is_none());
            drop(store);
            let conn = Connection::open(&path).expect("inspect");
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                    .expect("version"),
                SCHEMA_VERSION
            );
            assert!(table_exists(&conn, "config_saved"));
            for column in ["env_id", "revision", "saved_at_ms"] {
                assert!(column_exists(&conn, "config_saved", column), "{column}");
            }
            for (table, expected) in V11_TABLES.iter().zip(&before) {
                assert_eq!(&v11_rows(&conn, table), expected, "{table}");
            }
        }
    }

    #[test]
    fn v12_failure_rolls_back_startup_column_and_preserves_v11_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v12-failure.db");
        let before: Vec<_> = {
            let conn = Connection::open(&path).expect("fixture");
            v11_fixture(&conn);
            // Table creation fails after the startup column has been added.
            conn.execute_batch("CREATE VIEW config_saved AS SELECT 1 AS blocker")
                .expect("migration blocker");
            V11_TABLES
                .iter()
                .map(|table| v11_rows(&conn, table))
                .collect()
        };
        assert!(Store::open(&path).is_err());
        {
            let conn = Connection::open(&path).expect("inspect failed upgrade");
            assert!(!column_exists(&conn, "runners", "startup_config_json"));
            assert!(!table_exists(&conn, "config_saved"));
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                    .expect("unchanged version"),
                11
            );
            for (table, expected) in V11_TABLES.iter().zip(&before) {
                assert_eq!(&v11_rows(&conn, table), expected, "{table}");
            }
            assert_eq!(
                conn.query_row::<i64, _, _>("SELECT blocker FROM config_saved", [], |row| {
                    row.get(0)
                })
                .expect("preexisting blocker unchanged"),
                1
            );
            conn.execute_batch("DROP VIEW config_saved")
                .expect("unblock fixture");
        }
        drop(Store::open(&path).expect("retry upgrade"));
        drop(Store::open(&path).expect("reopen after retry"));
        let conn = Connection::open(&path).expect("inspect retry");
        assert!(column_exists(&conn, "runners", "startup_config_json"));
        assert!(table_exists(&conn, "config_saved"));
        for (table, expected) in V11_TABLES.iter().zip(&before) {
            assert_eq!(&v11_rows(&conn, table), expected, "{table}");
        }
    }

    fn v12_fixture(conn: &Connection) {
        v11_fixture(conn);
        migrate_v12(conn).expect("v12");
        conn.execute_batch(
            "INSERT INTO config_saved(env_id,revision,saved_at_ms)
             VALUES ('production','saved-revision',123);
             UPDATE runners SET startup_config_json='{}';
             PRAGMA user_version=12;",
        )
        .expect("v12 state");
    }

    fn v12_rows(conn: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        V11_TABLES
            .iter()
            .copied()
            .chain(["config_saved"])
            .map(|table| {
                let mut stmt = conn
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .expect("snapshot statement");
                let columns = stmt.column_count();
                stmt.query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                    .expect("snapshot query")
                    .collect::<Result<_, _>>()
                    .expect("snapshot rows")
            })
            .collect()
    }

    #[test]
    fn v12_to_v13_preserves_all_prior_batches_without_fake_observations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v12.db");
        let before = {
            let conn = Connection::open(&path).expect("fixture");
            v12_fixture(&conn);
            v12_rows(&conn)
        };
        for _ in 0..2 {
            let store = Store::open(&path).expect("upgrade/reopen");
            assert!(store
                .observations_for("fp1", 100)
                .expect("no backfill")
                .is_empty());
            drop(store);
            let conn = Connection::open(&path).expect("inspect");
            assert_eq!(v12_rows(&conn), before);
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                    .expect("version"),
                SCHEMA_VERSION
            );
            for column in [
                "id",
                "fingerprint",
                "received_at_ms",
                "observed_at_ms",
                "environment",
                "release_sha",
                "source_event_id",
                "trusted_source",
                "time_status",
                "disposition",
                "duplicate_of",
            ] {
                assert!(
                    column_exists(&conn, "signal_observations", column),
                    "{column}"
                );
            }
        }
    }

    #[test]
    fn v13_failure_rolls_back_observations_indexes_and_version_preserving_v12() {
        let mut conn = Connection::open_in_memory().expect("fixture");
        v12_fixture(&conn);
        let before = v12_rows(&conn);
        // Fail after creating the table and first index, exercising DDL rollback.
        conn.execute_batch("CREATE TABLE idx_signal_observations_source(blocker INTEGER)")
            .expect("blocker");
        assert!(apply(&mut conn).is_err());
        assert_eq!(v12_rows(&conn), before);
        assert!(!table_exists(&conn, "signal_observations"));
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='index' AND name='idx_signal_observations_issue'",
                [],
                |r| r.get(0),
            )
            .expect("index rolled back"),
            0
        );
        assert_eq!(
            conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .expect("version"),
            12
        );
        conn.execute_batch("DROP TABLE idx_signal_observations_source")
            .expect("unblock fixture");
        apply(&mut conn).expect("retry");
        apply(&mut conn).expect("reopen");
        assert_eq!(v12_rows(&conn), before);
        assert!(table_exists(&conn, "signal_observations"));
    }

    fn v13_fixture(conn: &Connection) {
        v12_fixture(conn);
        migrate_v13(conn).expect("v13");
        conn.execute_batch(
            "INSERT INTO signal_observations(fingerprint,received_at_ms,time_status,disposition)
             VALUES ('fp1',100,'missing','deduped');
             UPDATE runs SET pr_url='https://github.com/o/r/pull/1',pr_opened_at=86400005;
             UPDATE issues SET pr_url='https://github.com/o/r/pull/1',pr_opened_at=86400010;
             INSERT INTO runs(id,fingerprint,runner_id,repo,started_at,phase,pr_url,pr_opened_at)
             VALUES ('duplicate','fp1','runner','example.com/r',2,'deliver',
                    'https://github.com/o/r/pull/1',172800005),
                ('unknown-run','fp1','runner','example.com/r',3,'deliver',NULL,172800008),
                ('url-without-time','fp1','runner','example.com/r',259200003,'deliver',
                    'https://github.com/o/r/pull/2',NULL),
                ('other-repo','fp1','runner','other/repo',4,'deliver',
                    'https://github.com/o/r/pull/1',86400005);
             INSERT INTO issues(fingerprint,title,language,repo,stack_sample,first_seen,last_seen,
                pr_url,pr_opened_at)
             VALUES ('unknown-issue','unknown','go','example.com/r','stack',1,2,NULL,172800008);
             INSERT INTO deliveries(id,fingerprint,repo,branch,base,head_sha,state,pr_url,
                created_at,updated_at)
             VALUES ('d2','fp1','example.com/r','b2','main','h2','pr_created',
                    'https://github.com/o/r/pull/1',172800009,172800010),
                ('no-url-created','fp1','example.com/r','b3','main','h3','pr_created',NULL,
                    172800009,172800010),
                ('requested','fp1','example.com/r','b4','main','h4','pr_requested',
                    'https://github.com/o/r/pull/3',
                    86400003,172800010),
                ('uncertain','fp1','example.com/r','b5','main','h5','ambiguous',NULL,-1,172800010),
                ('intent','fp1','example.com/r','b6','main','h6','intent',
                    'https://github.com/o/r/pull/1',10,20),
                ('pushed','fp1','example.com/r','b7','main','h7','pushed',
                    'https://github.com/o/r/pull/1',10,20);
             PRAGMA user_version=13;",
        )
        .expect("v13 history");
    }

    fn v13_rows(conn: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let mut rows = v12_rows(conn);
        rows.push(v11_rows(conn, "signal_observations"));
        rows
    }

    #[test]
    fn v13_to_v14_preserves_history_deduplicates_and_reopens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v13.db");
        let before = {
            let conn = Connection::open(&path).expect("fixture");
            v13_fixture(&conn);
            v13_rows(&conn)
        };
        for _ in 0..2 {
            let store = Store::open(&path).expect("upgrade/reopen");
            let first = store
                .pr_slot_for_delivery("d1")
                .expect("slot")
                .expect("consumed");
            assert_eq!(first.state, "consumed");
            assert_eq!(
                first.created_at_ms, None,
                "legacy observation is not API evidence"
            );
            assert_eq!(first.confirmed_at_ms, Some(86_400_005));
            assert_eq!(first.admitted_at_ms, 86_400_005);
            assert_eq!(first.admission_day_start_ms, 86_400_000);
            assert_eq!(
                store.pr_slot_for_delivery("d2").expect("duplicate"),
                Some(first)
            );
            let pending = store
                .pr_slot_for_delivery("requested")
                .expect("pending")
                .expect("slot");
            assert_eq!(pending.state, "ambiguous");
            assert_eq!(pending.admitted_at_ms, 86_400_003);
            assert_eq!(pending.created_at_ms, None);
            assert_eq!(pending.confirmed_at_ms, None);
            assert_eq!(
                pending.pr_url, None,
                "unconfirmed identity is not creation evidence"
            );
            let uncertain = store
                .pr_slot_for_delivery("uncertain")
                .expect("uncertain")
                .expect("slot");
            assert_eq!(uncertain.state, "ambiguous");
            assert_eq!(uncertain.admission_day_start_ms, -86_400_000);
            assert_eq!(
                store
                    .pr_slots_used("example.com/r", -86_400_000)
                    .expect("negative day"),
                1
            );
            assert_eq!(
                store
                    .pr_slots_used("example.com/r", 0)
                    .expect("no fallback double charge"),
                0
            );
            assert_eq!(
                store
                    .pr_slots_used("example.com/r", 86_400_000)
                    .expect("canonical + pending"),
                2
            );
            assert_eq!(
                store
                    .pr_slots_used("example.com/r", 172_800_000)
                    .expect("distinct unknown evidence"),
                3
            );
            assert_eq!(
                store
                    .pr_slots_used("example.com/r", 259_200_000)
                    .expect("known URL unknown time"),
                1
            );
            assert_eq!(
                store
                    .pr_slots_used("other/repo", 86_400_000)
                    .expect("repo scoped"),
                1
            );
            for id in ["intent", "pushed"] {
                assert!(store
                    .pr_slot_for_delivery(id)
                    .expect("not requested")
                    .is_none());
            }
            assert!(store
                .pr_slot_for_delivery("no-url-created")
                .expect("unknown PR")
                .expect("slot")
                .created_at_ms
                .is_none());
            assert_eq!(
                store
                    .prs_opened_since("example.com/r", 0)
                    .expect("unchanged metric"),
                1
            );
            drop(store);
            let conn = Connection::open(&path).expect("inspect");
            assert_eq!(
                v13_rows(&conn),
                before,
                "all prior rows preserved byte-for-value"
            );
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                    .expect("version"),
                SCHEMA_VERSION
            );
            let unknown_created: Option<i64> = conn
                .query_row(
                    "SELECT created_at_ms FROM pr_slots WHERE repo='example.com/r'
                 AND pr_url='https://github.com/o/r/pull/2'",
                    [],
                    |r| r.get(0),
                )
                .expect("no fabricated GitHub time");
            assert_eq!(unknown_created, None);
        }
    }

    #[test]
    fn v14_failure_rolls_back_tables_indexes_version_and_all_v13_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v14-failure.db");
        let before = {
            let conn = Connection::open(&path).expect("fixture");
            v13_fixture(&conn);
            // Fail after both slot tables and the first three indexes exist.
            conn.execute_batch("CREATE TABLE idx_pr_slot_deliveries_slot(blocker INTEGER)")
                .expect("blocker");
            v13_rows(&conn)
        };
        assert!(Store::open(&path).is_err());
        {
            let conn = Connection::open(&path).expect("inspect failure");
            assert_eq!(v13_rows(&conn), before);
            assert!(!table_exists(&conn, "pr_slots"));
            assert!(!table_exists(&conn, "pr_slot_deliveries"));
            assert_eq!(conn.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_pr_slots_url'",
                [], |r| r.get(0)
            ).expect("index rollback"), 0);
            assert_eq!(
                conn.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                    .expect("version"),
                13
            );
            conn.execute_batch("DROP TABLE idx_pr_slot_deliveries_slot")
                .expect("unblock fixture");
        }
        drop(Store::open(&path).expect("retry"));
        drop(Store::open(&path).expect("reopen"));
        let conn = Connection::open(&path).expect("inspect retry");
        assert_eq!(v13_rows(&conn), before);
        assert!(table_exists(&conn, "pr_slots"));
        assert!(table_exists(&conn, "pr_slot_deliveries"));
    }

    #[test]
    fn future_schema_is_rejected_before_any_database_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        for populated in [false, true] {
            let path = dir.path().join(format!("future-{populated}.db"));
            {
                let conn = Connection::open(&path).expect("fixture");
                if populated {
                    v11_fixture(&conn);
                }
                conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                    .expect("future version");
            }
            let before = std::fs::read(&path).expect("fixture bytes");
            assert!(matches!(
                Store::open(&path),
                Err(ShanError::UnsupportedSchema(version)) if version == SCHEMA_VERSION + 1
            ));
            assert!(before == std::fs::read(&path).expect("unchanged fixture"));
            let conn = Connection::open(&path).expect("inspect");
            assert_eq!(table_exists(&conn, "issues"), populated);
            assert!(!table_exists(&conn, "config_saved"));
            assert!(!column_exists(&conn, "runners", "startup_config_json"));
            let mode: String = conn
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .expect("mode");
            assert_eq!(mode, "delete");
        }
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
        assert_eq!(version, SCHEMA_VERSION);
        for table in [
            "issues",
            "events",
            "runners",
            "runs",
            "signal_daily",
            "repos",
            "repair_memory",
            "benchmark_runs",
            "benchmark_results",
            "config_saved",
            "signal_observations",
            "pr_slots",
            "pr_slot_deliveries",
        ] {
            assert!(table_exists(&conn, table), "missing {table}");
        }
        for (table, column) in [
            ("issues", "state_changed_at"),
            ("issues", "owner_runner_id"),
            ("issues", "attempts"),
            ("issues", "log_sample"),
            ("runners", "progress_at"),
            ("runners", "instance_id"),
            ("runners", "lease_until"),
            ("runners", "startup_config_json"),
            ("checkout_leases", "runner_id"),
            ("checkout_leases", "runner_instance_id"),
            ("events", "repo"),
            ("events", "actor"),
            ("repos", "created_at"),
            ("repos", "updated_at"),
            ("runs", "compactions"),
            ("runs", "context_window"),
            ("runs", "messages"),
            ("repair_memory", "disabled"),
        ] {
            assert!(
                column_exists(&conn, table, column),
                "missing {table}.{column}"
            );
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
        assert_eq!(version, SCHEMA_VERSION, "v0 must run every migration");
        assert!(table_exists(&conn, "repos"));
        let (state_changed_at, owner): (i64, Option<String>) = conn
            .query_row(
                "SELECT state_changed_at, owner_runner_id FROM issues WHERE fingerprint='fp1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("issue");
        assert_eq!(
            state_changed_at, 150,
            "backfill from latest state_changed event"
        );
        assert_eq!(owner, None);
        let repo: String = conn
            .query_row(
                "SELECT repo FROM events WHERE fingerprint='fp1'",
                [],
                |row| row.get(0),
            )
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
            conn.pragma_update(None, "user_version", 1)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v2/v3/v4/v5/v6");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(table_exists(&conn, "repos"));
        assert!(column_exists(&conn, "issues", "attempts"));
        // v1 data survives the upgrade.
        let title: String = conn
            .query_row(
                "SELECT title FROM issues WHERE fingerprint='fp1'",
                [],
                |row| row.get(0),
            )
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
            conn.pragma_update(None, "user_version", 2)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v3/v4/v5/v6");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(column_exists(&conn, "issues", "attempts"));
        assert!(column_exists(&conn, "runners", "progress_at"));
        let title: String = conn
            .query_row(
                "SELECT title FROM issues WHERE fingerprint='fp1'",
                [],
                |row| row.get(0),
            )
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
            conn.pragma_update(None, "user_version", 3)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v4/v5/v6");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(table_exists(&conn, "repair_memory"));
        let title: String = conn
            .query_row(
                "SELECT title FROM issues WHERE fingerprint='fp1'",
                [],
                |row| row.get(0),
            )
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
            conn.pragma_update(None, "user_version", 4)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v5");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        for column in ["compactions", "context_window", "messages"] {
            assert!(
                column_exists(&conn, "runs", column),
                "missing runs.{column}"
            );
        }
        assert!(table_exists(&conn, "repair_memory"));
    }

    #[test]
    fn migrates_v5_database_to_v6() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v5.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1");
            migrate_v2(&conn).expect("v2");
            migrate_v3(&conn).expect("v3");
            migrate_v4(&conn).expect("v4");
            migrate_v5(&conn).expect("v5");
            conn.pragma_update(None, "user_version", 5)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v6");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(table_exists(&conn, "benchmark_runs"));
        assert!(table_exists(&conn, "benchmark_results"));
        assert!(column_exists(&conn, "repair_memory", "disabled"));
        assert!(column_exists(&conn, "runs", "messages"));
    }

    #[test]
    fn migrates_v7_database_to_v8() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("v7.db");
        {
            let conn = Connection::open(&path).expect("open");
            v0_schema(&conn);
            migrate_v1(&conn).expect("v1");
            migrate_v2(&conn).expect("v2");
            migrate_v3(&conn).expect("v3");
            migrate_v4(&conn).expect("v4");
            migrate_v5(&conn).expect("v5");
            migrate_v6(&conn).expect("v6");
            migrate_v7(&conn).expect("v7");
            conn.pragma_update(None, "user_version", 7)
                .expect("version");
        }
        let _store = Store::open(&path).expect("migrate v8");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(column_exists(&conn, "runners", "instance_id"));
        assert!(column_exists(&conn, "runners", "lease_until"));
        let title: String = conn
            .query_row(
                "SELECT title FROM issues WHERE fingerprint='fp1'",
                [],
                |row| row.get(0),
            )
            .expect("issue title");
        assert_eq!(title, "t");
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
        assert_eq!(version, SCHEMA_VERSION);
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
