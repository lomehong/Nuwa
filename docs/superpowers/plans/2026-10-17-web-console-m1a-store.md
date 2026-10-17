# Web 控制台 M1a 数据层与 Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 v3 控制台建立数据基础：`user_version` 迁移、`runners`/`runs`/`signal_daily`、events 审计列、卡单判定与 requeue/reopen Store API。本计划不改变 `nuwa work` 的修复行为，只提供后续遥测接入所需的接口。

**Architecture:** 在 `shanhaijing` 内新增 `migration` 模块，按 `PRAGMA user_version` 顺序迁移；所有 SQL 继续收敛在 Store；旧 UI/CLI 保持可编译。M1b 再把这些接口接到 `work`。

**Tech Stack:** Rust 2021、rusqlite 0.31（bundled）、serde、time 0.3、tempfile（测试）。

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md) §7、§8.4–8.5、§14 M1。

## Global Constraints

- 自建 crate 零 clippy warning：`cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings`。
- 迁移只增列/增表，不删旧数据；必须幂等，并从 v0 fixture 测试通过。
- 不改变 `repair.rs` / `deliver.rs` / `guard.rs` 的修复与护栏行为（M1b 才接遥测）。
- 不碰 `crates/rpi-*`。
- 所有测试使用临时 SQLite 文件，不依赖本机 `shanhaijing.db`。
- 每个任务一个 commit。

---

### Task 1: 迁移框架、WAL 与 v1 schema

**Files:**
- Create: `crates/shanhaijing/src/migration.rs`
- Modify: `crates/shanhaijing/src/lib.rs`（`mod migration;`、`Store::open`）
- Test: `crates/shanhaijing/src/migration.rs`（内嵌 tests）

**Interfaces:**
- Produces: `migration::apply(&mut rusqlite::Connection) -> ShanResult<()>`；打开连接即设置 WAL/busy_timeout/synchronous/foreign_keys，并把 `user_version` 从 0 迁到 1。
- Consumes: 现有 `lib.rs` 的 `SCHEMA` 常量（子模块可访问父模块私有项）。

- [ ] **Step 1: 写失败测试**

在 `lib.rs` 顶部加入 `mod migration;`，并新建 `migration.rs`，先写测试骨架：

```rust
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use std::path::Path;

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
    fn fresh_database_is_v1() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fresh.db");
        let _store = Store::open(&path).expect("open");
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 1);
        for table in ["issues", "events", "runners", "runs", "signal_daily"] {
            assert!(table_exists(&conn, table), "missing {table}");
        }
        for (table, column) in [
            ("issues", "state_changed_at"),
            ("issues", "owner_runner_id"),
            ("events", "repo"),
            ("events", "actor"),
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
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idem.db");
        drop(Store::open(&path).expect("first"));
        drop(Store::open(&path).expect("second"));
        let conn = Connection::open(&path).expect("reopen");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, 1);
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing migration::tests -- --nocapture`

Expected: 编译失败，`migration::apply` 不存在；或 fresh DB 没有 `runners` 表。

- [ ] **Step 3: 实现迁移模块**

在 `migration.rs` 的 tests 模块之前加入：

```rust
use rusqlite::Connection;

use crate::{ShanError, ShanResult};

pub const SCHEMA_VERSION: i64 = 1;

pub fn apply(conn: &mut Connection) -> ShanResult<()> {
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.execute_batch(
        "PRAGMA busy_timeout=5000;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;",
    )?;
    // v0 基线表（issues/events/heartbeat）。CREATE IF NOT EXISTS 对旧库无副作用。
    conn.execute_batch(super::SCHEMA)?;

    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        migrate_v1(&tx)?;
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
```

- [ ] **Step 4: 让 `Store::open` 走迁移**

在 `lib.rs` 顶部加入 `mod migration;`，把 `Store::open` 改为：

```rust
    pub fn open(path: impl AsRef<Path>) -> ShanResult<Self> {
        let mut conn = Connection::open(path.as_ref())?;
        migration::apply(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
```

删除 `Store::open` 里原来的 `conn.execute_batch(SCHEMA)?;` 与两列 `ALTER TABLE` 循环。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing migration::tests -- --nocapture`

Expected: 4 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/migration.rs crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): add user_version migrations and v1 schema (runners/runs/signal_daily)"
```

---

### Task 2: events 增列与 `record_event` 归属

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`（`EventRow`、`recent_events`、`events_for`、`record_event`、`set_state`→`set_state_as`、内部调用）
- Modify: `crates/nuwa/src/main.rs`（CI 事件 3 处）
- Modify: `crates/nuwa/src/settings.rs`（配置事件 1 处）
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `EventRow { fingerprint, ts, kind, detail, repo, actor: Option<String> }`
  - `pub fn record_event(&self, fingerprint: &str, repo: Option<&str>, actor: &str, kind: &str, detail: &str) -> ShanResult<()>`
  - `pub fn set_state_as(&self, fp: &str, to: State, now_ms: i64, detail: Option<&str>, actor: &str) -> ShanResult<()>`
  - `set_state(...)` 保持旧签名，内部转调 `set_state_as(..., "system")`。
- Consumes: v1 迁移已添加 `events.repo` / `events.actor`。

- [ ] **Step 1: 写失败测试**

在 `lib.rs` tests 模块加入：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing events_carry_repo_and_actor set_state_as_persists_actor -- --nocapture`

Expected: 编译失败（`record_event` 参数数量不符 / `set_state_as` 不存在 / `actor` 字段不存在）。

- [ ] **Step 3: 扩展 EventRow 与查询**

把 `EventRow` 改为：

```rust
#[derive(Debug, Clone, Serialize)]
pub struct EventRow {
    pub fingerprint: String,
    pub ts: i64,
    pub kind: String,
    pub detail: String,
    pub repo: String,
    pub actor: Option<String>,
}
```

`recent_events` 与 `events_for` 的 SELECT 改为：

```sql
SELECT e.fingerprint, e.ts, COALESCE(e.kind, ''), COALESCE(e.detail, ''),
       COALESCE(e.repo, i.repo, ''), e.actor
FROM events e LEFT JOIN issues i ON i.fingerprint = e.fingerprint
```

并在行映射中增加 `actor: row.get(5)?`。

- [ ] **Step 4: 扩展 `record_event` 与 `set_state_as`**

把 `record_event` 改为（六参数：fingerprint, now, repo, actor, kind, detail）：

```rust
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
```

把 `set_state` 拆成 wrapper + 带 actor 的实现：

```rust
    pub fn set_state(
        &self,
        fingerprint: &str,
        to: State,
        now_ms: i64,
        detail: Option<&str>,
    ) -> ShanResult<()> {
        self.set_state_as(fingerprint, to, now_ms, detail, "system")
    }

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
```

- [ ] **Step 5: 更新所有调用点**

按以下规则更新：

```rust
// report_signal 首次上报
self.record_event(&fp, now_ms, Some(&signal.repo), "system", "reported", "first sighting")?;

// report_signal 的 dedup（本任务保留，Task 5 删除）
self.record_event(&fp, now_ms, Some(&signal.repo), "system", "reported", "dedup")?;

// claim_ready_issue
self.record_event(
    &snapshot.fingerprint,
    now_ms,
    Some(&snapshot.repo),
    "runner",
    "state_changed",
    "claimed by runner",
)?;

// set_pr：先查 repo
let repo: String = self
    .lock()
    .query_row("SELECT repo FROM issues WHERE fingerprint = ?1", [fingerprint], |row| row.get(0))?;
self.record_event(fingerprint, now_ms, Some(&repo), "runner", "pr_opened", pr_url)?;

// main.rs CI 事件（3 处）
let _ = store.record_event(
    &fp,
    now_ms(),
    Some(&repo_config.repo),
    "runner",
    "ci_green",
    &format!("head {}", &delivered.commit[..12.min(delivered.commit.len())]),
);
let _ = store.record_event(
    &fp,
    now_ms(),
    Some(&repo_config.repo),
    "runner",
    "ci_red",
    &format!("head {} — PR #{number} closed", &delivered.commit[..12.min(delivered.commit.len())]),
);
let _ = store.record_event(&fp, now_ms(), Some(&repo_config.repo), "runner", "ci_timeout", slug);

// settings.rs 配置事件
let _ = store.record_event("__config__", now, None, "owner", "config_changed", key);

// ui.rs reopen（owner 动作）
store.set_state_as(fp, shanhaijing::State::New, crate::now_ms(), Some("reopened via web console"), "owner")

// main.rs CLI run_reopen（owner 动作）
store.set_state_as(&fp, shanhaijing::State::New, now_ms(), Some("reopened by owner"), "owner")
```

注意 `set_pr` 中查询 repo 时不要与后续 `record_event` 的锁嵌套：`query_row` 的 `MutexGuard` 必须在 `record_event` 前释放。把 repo 查询放在一个独立作用域：

```rust
        let repo: String = {
            let conn = self.lock();
            conn.query_row(
                "SELECT repo FROM issues WHERE fingerprint = ?1",
                [fingerprint],
                |row| row.get(0),
            )?
        };
```

`set_pr` 自己还有一个 `let conn = self.lock();` 用于 UPDATE；先 UPDATE、drop，再查 repo、再 record_event。

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p shanhaijing events_carry_repo_and_actor set_state_as_persists_actor -- --nocapture`

再跑全量自建 crate 测试：

Run: `cargo test -p shanhaijing -p nuwa`

Expected: 全绿。

- [ ] **Step 7: 提交**

```bash
git add crates/shanhaijing/src/lib.rs crates/nuwa/src/main.rs crates/nuwa/src/settings.rs
git commit -m "feat(shanhaijing): attribute events with repo and actor"
```

---

### Task 3: runners Store API

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`（错误类型、结构体、方法）
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `RunnerUpsert { id, repo, host, pid: Option<i64>, started_at, heartbeat_at, status, provider, model, daily_pr_limit: Option<i64>, repo_path, service_unit, config_json }`
  - `RunnerHeartbeat { heartbeat_at, status, current_fingerprint, phase, config_json: Option<String>, last_error: Option<String> }`
  - `RunnerRow { id, repo, host, pid, started_at, heartbeat_at, status, current_fingerprint, phase, provider, model, daily_pr_limit, repo_path, service_unit, config_json, last_error }`（`Serialize`）
  - `pub fn upsert_runner(&self, r: &RunnerUpsert) -> ShanResult<()>`
  - `pub fn heartbeat_runner(&self, id: &str, hb: &RunnerHeartbeat) -> ShanResult<()>`
  - `pub fn list_runners(&self) -> ShanResult<Vec<RunnerRow>>`
- Consumes: v1 `runners` 表。

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing runner_upsert_heartbeat_and_list -- --nocapture`

Expected: 编译失败，`RunnerUpsert` 等不存在。

- [ ] **Step 3: 实现结构体与错误**

在 `ShanError` 中增加：

```rust
    #[error("unknown runner {0}")]
    UnknownRunner(String),
```

在 `EventRow` 之后增加：

```rust
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
```

- [ ] **Step 4: 实现方法**

在 `record_event` 之前加入：

```rust
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing runner_upsert_heartbeat_and_list -- --nocapture`

Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): add runner registry and heartbeat store API"
```

---

### Task 4: runs Store API

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `RunStart { id, fingerprint, runner_id, repo, started_at, provider, model }`
  - `RunUpdate`（全部 `Option` 字段：phase、turns、tokens_uncached、tokens_cached、harness_run_id、root_cause、verification_ok、verification_detail、change_stat、branch、commit_sha、pr_url、pr_opened_at、ci_verdict、session_dir、error）
  - `RunFinish { finished_at, outcome, ci_verdict: Option<String>, error: Option<String> }`
  - `RunRow`（表全字段，`Serialize`）
  - `start_run` / `update_run` / `finish_run` / `mark_interrupted` / `latest_run` / `runs_for`
- Consumes: v1 `runs` 表；`issues.owner_runner_id`。

- [ ] **Step 1: 写失败测试**

```rust
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
            fingerprint: fp,
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing run_lifecycle_and_latest mark_interrupted -- --nocapture`

Expected: 编译失败，类型/方法不存在。

- [ ] **Step 3: 实现结构体**

```rust
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
```

- [ ] **Step 4: 实现方法**

```rust
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
```

在 `ShanError` 增加：

```rust
    #[error("unknown run {0}")]
    UnknownRun(String),
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing run_lifecycle_and_latest mark_interrupted -- --nocapture`

Expected: 两个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): add run lifecycle store API"
```

---

### Task 5: signal_daily 与停止 dedup 事件刷屏

**Files:**
- Modify: `crates/shanhaijing/Cargo.toml`（`time`）
- Modify: `crates/shanhaijing/src/lib.rs`（`utc_day`、`bump_signal_daily`、`report_signal`、`signal_reports_on`）
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `pub fn utc_day(now_ms: i64) -> String`
  - `pub fn signal_reports_on(&self, fingerprint: &str, day: &str) -> ShanResult<u64>`
- Consumes: v1 `signal_daily` 表；`record_event` 新签名。

- [ ] **Step 1: 加依赖**

在 `crates/shanhaijing/Cargo.toml` 的 `[dependencies]` 增加：

```toml
time = { version = "0.3", features = ["std"] }
```

- [ ] **Step 2: 写失败测试**

```rust
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p shanhaijing utc_day_formats dedup_counts -- --nocapture`

Expected: 编译失败，`utc_day` / `signal_reports_on` 不存在。

- [ ] **Step 4: 实现 `utc_day` 与 daily upsert**

在 `lib.rs`（tests 之前）加入：

```rust
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
```

在 `report_signal` 的新库插入分支，`drop(conn)` 之前加入：

```rust
            bump_signal_daily(&conn, &fp, &signal.repo, now_ms)?;
```

在既有 issue 的 `UPDATE` 之后、`occurrences` 计算附近加入：

```rust
        bump_signal_daily(&conn, &fp, &signal.repo, now_ms)?;
```

删除 dedup 分支的：

```rust
        self.record_event(&fp, now_ms, Some(&signal.repo), "system", "reported", "dedup")?;
```

（首次上报的 `reported` 事件保留。）

在回归分支 `set_state(...New...)` 之后补：

```rust
            self.set_cooldown(&fp, 0)?;
```

新增查询方法：

```rust
    pub fn signal_reports_on(&self, fingerprint: &str, day: &str) -> ShanResult<u64> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COALESCE(SUM(count), 0) FROM signal_daily WHERE fingerprint=?1 AND day=?2",
            rusqlite::params![fingerprint, day],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing utc_day_formats dedup_counts -- --nocapture`

再跑全量：

Run: `cargo test -p shanhaijing`

Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/Cargo.toml Cargo.lock crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): aggregate reports daily and stop dedup event spam"
```

---

### Task 6: state_changed_at、owner 与卡单判定

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`（`claim_ready_issue`、新增 `stuck`）
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces: `pub fn stuck(&self, fingerprint: &str, now_ms: i64, stuck_after_ms: i64) -> ShanResult<bool>`
- Consumes: `issues.state_changed_at`/`owner_runner_id`、`runners` 心跳。

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing stuck_detection_uses_owner_heartbeat -- --nocapture`

Expected: 编译失败，`stuck` 不存在。

- [ ] **Step 3: 在 claim 时写 state_changed_at**

把 `claim_ready_issue` 的 UPDATE 改为：

```sql
UPDATE issues SET state = 'triggered', cooldown_until = ?2, state_changed_at = ?1
WHERE fingerprint = (
    SELECT fingerprint FROM issues
    WHERE state = 'new' AND cooldown_until <= ?1
    ORDER BY (CASE severity
        WHEN 'critical' THEN 10 WHEN 'high' THEN 5 WHEN 'medium' THEN 2 ELSE 1 END)
        * occurrences DESC,
        last_seen ASC
    LIMIT 1
)
RETURNING fingerprint, title, language, repo, severity, occurrences, stack_sample
```

- [ ] **Step 4: 实现 stuck**

```rust
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing stuck_detection_uses_owner_heartbeat -- --nocapture`

Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): detect stuck issues from owner heartbeat"
```

---

### Task 7: requeue 与 reopen Store API

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces:
  - `pub fn requeue_stuck(&self, fingerprint: &str, force: bool, now_ms: i64, stuck_after_ms: i64) -> ShanResult<()>`
  - `pub fn reopen_issue(&self, fingerprint: &str, now_ms: i64) -> ShanResult<()>`
- Consumes: `stuck`、`issues`、`record_event`。

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing requeue_blocks reopen_issue_allows -- --nocapture`

Expected: 编译失败，方法不存在。

- [ ] **Step 3: 实现 requeue**

```rust
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
```

在 `ShanError` 增加：

```rust
    #[error("issue {0} is not stuck")]
    NotStuck(String),
```

- [ ] **Step 4: 实现 reopen**

```rust
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing requeue_blocks reopen_issue_allows -- --nocapture`

Expected: 两个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): add owner requeue and reopen store API"
```

---

### Task 8: 配额按 PR 去重统计与保留清理

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`（`prs_opened_since`、`prune`）
- Test: `crates/shanhaijing/src/lib.rs` tests

**Interfaces:**
- Produces: `pub fn prune(&self, now_ms: i64, event_days: u32, run_days: u32) -> ShanResult<u64>`
- Consumes: `runs`、`issues`、`events`、`runners`。

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p shanhaijing pr_quota_counts prune_removes -- --nocapture`

Expected: 配额计数为 1（runs 未计入）或 `prune` 不存在。

- [ ] **Step 3: 改配额查询**

把 `prs_opened_since` 的 SQL 改为：

```sql
SELECT COUNT(DISTINCT pr_url) FROM (
    SELECT pr_url FROM runs
    WHERE repo = ?1 AND pr_opened_at IS NOT NULL AND pr_opened_at >= ?2
    UNION ALL
    SELECT pr_url FROM issues
    WHERE repo = ?1 AND pr_opened_at IS NOT NULL AND pr_opened_at >= ?2
)
```

- [ ] **Step 4: 实现 prune**

```rust
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p shanhaijing pr_quota_counts prune_removes -- --nocapture`

Expected: 两个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(shanhaijing): count PR quota from runs and prune old telemetry"
```

---

### Task 9: 全量验证

**Files:**
- No code changes expected.

- [ ] **Step 1: 跑自建 crate 检查**

```bash
taskkill //F //IM nuwa.exe 2>/dev/null || true
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p shanhaijing -p nuwa
```

Expected: clippy 零 warning；测试全绿（不跑 `--workspace`，避开已知 Windows vendored 失败）。

- [ ] **Step 2: 检查迁移对现有本地库的影响（只读演练）**

```bash
cp shanhaijing.db /tmp/shanhaijing-m1a.db
C:/Users/lome/.cargo/shared-target/debug/nuwa.exe status --db /tmp/shanhaijing-m1a.db | head -5
```

Expected: 命令成功；数据仍可读，新增表不影响 `status`。

- [ ] **Step 3: 提交（若没有改动则跳过）**

```bash
git status --short
```

Expected: clean。

---

## 自审记录

- 规格覆盖：§7 迁移/表/索引/保留、§8.4 卡单、§8.5 reopen/requeue、§9 所需的 Store 查询基础均有任务。
- 占位符：无 TBD；所有新增类型/方法在测试与实现中同名。
- 类型一致性：`RunnerHeartbeat` 的字段与 `heartbeat_runner` 参数一致；`RunUpdate` 的 `turns/tokens` 使用 `i64`，M1b 负责从 `u32/u64` 转换；`record_event` 六参数签名在 Task 2 统一定义，后续任务全部按此调用。
- 已知边界：M1a 不接 `work`，所以 `runners`/`runs` 在生产库中暂时为空；M1b 完成后才会有真实遥测。
