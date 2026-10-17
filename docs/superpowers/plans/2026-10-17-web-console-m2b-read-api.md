# Web 控制台 M2b 只读 API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 M2a 的会话/路由地基上提供只读 API：`/overview`、`/issues`、`/issues/{fp}`、`/runners`、`/events`、`/metrics/trends`，并补齐 Store 查询层（分页/过滤/卡单/趋势/事件聚合）。

**Architecture:** Store 负责 SQL 与分页，API 负责 DTO、权限、错误映射与 stuck/attention 组合。读端点全部挂在 M2a 的受保护路由下；测试用 local-mode `AppState` + 临时 DB + `oneshot`，不需要登录。

**Tech Stack:** axum 0.8、rusqlite、serde、现有 `shanhaijing` Store。

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md) §9.2（overview/issues/detail/runners/events/trends）、§10；ADR-009。

## Global Constraints

- 自建 crate 零 clippy；只读端点不产生副作用。
- 所有时间字段 epoch ms；分页默认 50、上限 200。
- 不改变 M0/M1/M2a 行为；不触碰 vendored/guard/deliver。
- API 响应 `Cache-Control: no-store`（M2a 已加）。
- 测试使用临时 DB；不依赖本机 `shanhaijing.db`。
- 每个任务一个 commit；不 push/tag/deploy。

---

### Task 1: Issue 查询视图与 Store 只读辅助

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: `lib.rs` tests

**Interfaces:**
- Extend `IssueRow` with `state_changed_at: i64`, `owner_runner_id: Option<String>`, `ci_verdict: Option<String>`; serialization names `first_seen_ms`/`last_seen_ms`/`state_changed_at_ms`.
- `latest_event(fp) -> Option<EventRow>`
- `active_runs() -> Vec<RunRow>` (`finished_at IS NULL`, newest first)
- `ready_issues(now, limit) -> Vec<IssueSnapshot>` (same ordering as claim, no update)
- `count_ready(now) -> u64`
- `count_events_since(kinds: &[&str], since_ms) -> u64`
- `reports_on(day) -> u64` (sum signal_daily)
- `similar_issues(title, exclude_fp, limit) -> Vec<IssueRow>`

- [ ] **Step 1: 扩展 `list_issues`**

SQL:

```sql
SELECT i.fingerprint, i.title, i.language, i.repo, i.severity, i.state,
       i.occurrences, i.pr_url, i.first_seen, i.last_seen,
       i.state_changed_at, i.owner_runner_id,
       (SELECT r.ci_verdict FROM runs r
        WHERE r.fingerprint = i.fingerprint
        ORDER BY r.started_at DESC, r.id DESC LIMIT 1) AS ci_verdict
FROM issues i ORDER BY i.last_seen DESC
```

`IssueRow` fields and serde renames:

```rust
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
    pub ci_verdict: Option<String>,
}
```

- [ ] **Step 2: 辅助方法**

```rust
pub fn latest_event(&self, fingerprint: &str) -> ShanResult<Option<EventRow>>
pub fn active_runs(&self) -> ShanResult<Vec<RunRow>>
pub fn ready_issues(&self, now_ms: i64, limit: u32) -> ShanResult<Vec<IssueSnapshot>>
pub fn count_ready(&self, now_ms: i64) -> ShanResult<u64>
pub fn count_events_since(&self, kinds: &[&str], since_ms: i64) -> ShanResult<u64>
pub fn reports_on(&self, day: &str) -> ShanResult<u64>
pub fn similar_issues(&self, title: &str, exclude_fp: &str, limit: u32) -> ShanResult<Vec<IssueRow>>
```

Use the existing `ready_issue` ordering for `ready_issues` (without UPDATE), `events` `kind IN (...)` with a dynamically built placeholder list (static kinds from API; validate), `signal_daily SUM(count) WHERE day=?`, and `issues WHERE title=? AND fingerprint<>? ORDER BY last_seen DESC LIMIT ?`.

- [ ] **Step 3: Tests**

- Extend an issue row test: after start_run/update_run with `ci_verdict=success`, `list_issues` shows owner/state_changed/ci.
- `active_runs` excludes finished runs.
- `ready_issues` returns the same priority order as claim but leaves state new.
- `similar_issues` returns same-title rows excluding self.
- `count_events_since`/`reports_on` with seeded timestamps.

- [ ] **Step 4: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(store): issue views, active runs and read helpers"
```

---

### Task 2: Events 分页/聚合与 Trends Store

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: `lib.rs` tests

**Interfaces:**
- `EventQuery { kinds: Vec<String>, repo: Option<String>, fingerprint: Option<String>, from_ms: Option<i64>, to_ms: Option<i64> }`
- `list_events(&EventQuery, limit) -> Vec<EventRow>` (newest first; cap 5000)
- `TrendBucket { day, reports, new_issues, prs_opened, merged, rejected, tokens, ci_green, ci_red }`
- `trends(days: u32, repo: Option<&str>, now_ms: i64) -> Vec<TrendBucket>`

- [ ] **Step 1: Implement**

- `list_events`: SQL filters; `kind IN (...)`; order `ts DESC, id DESC`; limit.
- `trends`: build `BTreeMap<String, TrendBucket>` for the last `days` UTC days using `utc_day`; fill from:
  - `signal_daily` (reports, by day/repo),
  - `issues.first_seen` (new issues; bucket in Rust),
  - `runs.pr_opened_at` (PRs opened; bucket),
  - `events` merged/rejected/ci_green/ci_red (bucket),
  - `runs.started_at` + tokens (tokens by run start day).
  Missing days are zero-filled.
- Tests with seeded issues/runs/events/signal_daily at fixed timestamps; assert bucket counts and zero days.

- [ ] **Step 2: 提交**

```bash
git add crates/shanhaijing/src/lib.rs
git commit -m "feat(store): events filtering and daily trends"
```

---

### Task 3: API DTO 与 Overview

**Files:**
- Create: `crates/nuwa/src/api/overview.rs`
- Modify: `crates/nuwa/src/api/mod.rs`（routes/DTOs）
- Test: `api/overview.rs` tests + `serve.rs` integration

**Interfaces:**
- `GET /api/v1/overview?repo=` returns counts, attention, active_runs, queue, health, ci_24h.
- DTOs:
  - `OverviewResponse { counts: BTreeMap<String, usize>, attention: Vec<AttentionItem>, active_runs: Vec<ActiveRun>, queue: Queue, health: Health, ci_24h: CiCounts }`
  - `AttentionItem { fingerprint,title,state,severity,repo,last_seen_ms,stuck,owner_runner_id,reason_code,reason_text,pr_url }`
  - `ActiveRun { id,fingerprint,title,repo,runner_id,phase,started_at_ms,turns,tokens }`
  - `Queue { ready_count: u64, next: Vec<QueueItem> }`
  - `Health { runners_total, runners_online, working, idle, standing_down, error, reports_today, prs_today }`
  - `CiCounts { green, red, timeout, pending }`
- Attention:
  - escalated/rejected from issues; reason from latest event detail prefix (`repair_failed`/`delivery_failed`/`budget_exceeded`/`ci_red` → code/text; rejected without prefix → `pr_closed`).
  - stuck: `state in triggered/fixing`, owner healthy check using list_runners; use `now - max(state_changed_at, owner heartbeat) > max(3*interval, 300s)` with interval from config_json or 30.
  - pr_open with `ci_timeout` event in last 24h.
  - Sort escalated > stuck > rejected > ci_timeout; same-level last_seen asc.
- Active runs: `started_at_ms`, turns/tokens from runs.
- Queue: `count_ready`, next 3 `ready_issues`.
- Health: runners offline (90s), status counts, reports today (UTC day), prs today (sum `prs_opened_since` per repo or all runners' repos).
- CI 24h: `count_events_since` for ci_green/red/timeout; pending = active_runs with phase `ci`.

- [ ] **Step 1: Route + handler + test**

Use local-mode `AppState` (ui_token None) in tests; seed DB; `oneshot GET /api/v1/overview`; assert counts, attention reason, active runs, queue.

- [ ] **Step 2: 提交**

```bash
git add crates/nuwa/src/api/overview.rs crates/nuwa/src/api/mod.rs
git commit -m "feat(api): overview endpoint with attention and runner health"
```

---

### Task 4: Issues 列表与详情

**Files:**
- Create: `crates/nuwa/src/api/issues.rs`
- Modify: `crates/nuwa/src/api/mod.rs`
- Test: `api/issues.rs` + integration

**Interfaces:**
- `GET /api/v1/issues?state=&severity=&repo=&language=&q=&stuck=1&attention=1&sort=&order=&page=&page_size=`
- `GET /api/v1/issues/{fp}`
- List item: `IssueRow` + `stuck`.
- Detail: issue + `stack_sample` (from `store.get`), `timeline` (events_for 200), `runs` (runs_for 10), `similar`.
- Filters/sort/pagination in Rust over `list_issues` + runner map; `q` case-insensitive title/fingerprint contains; `attention=1` uses the same attention predicate; `state` comma list.
- Sort: last_seen/first_seen/occurrences/severity weight/state lifecycle; order.
- Pagination: page 1-based, page_size default 50 max 200; meta total.
- 404 for unknown fingerprint.
- Tests: seeded 3 issues; filter state/severity/q; sort; pagination; stuck flag; detail timeline/runs/similar/404.

- [ ] **Step 1: Implement + tests**

- [ ] **Step 2: 提交**

```bash
git add crates/nuwa/src/api/issues.rs crates/nuwa/src/api/mod.rs
git commit -m "feat(api): issue list and detail endpoints"
```

---

### Task 5: Runners / Events / Trends 端点

**Files:**
- Create: `crates/nuwa/src/api/runners.rs`, `events.rs`, `trends.rs`
- Modify: `crates/nuwa/src/api/mod.rs`
- Test: integration

**Interfaces:**
- `GET /api/v1/runners` → `RunnerDto` list with `offline`, `prs_today`, `effective` parsed from `config_json`.
- `GET /api/v1/events?kind=&repo=&fingerprint=&from_ms=&to_ms=&aggregate=1&page=&page_size=` → event rows; `aggregate=1` groups consecutive `reported` rows by fingerprint+hour in Rust (count/first/last), others raw; paginate after grouping.
- `GET /api/v1/metrics/trends?range=7d|14d|30d|90d&repo=` → buckets/totals from Store::trends.
- Offline: `now - heartbeat_at > 90_000`.
- `prs_today`: `prs_opened_since(repo, UTC day start)`.
- Tests: seeded runner (working/stale), events, signal_daily/runs; assert fields, aggregate, range buckets.

- [ ] **Step 1: Implement + tests**

- [ ] **Step 2: 提交**

```bash
git add crates/nuwa/src/api/runners.rs crates/nuwa/src/api/events.rs crates/nuwa/src/api/trends.rs crates/nuwa/src/api/mod.rs
git commit -m "feat(api): runners, events and trends endpoints"
```

---

### Task 6: 集成、文档与验证

**Files:**
- Tests: `serve.rs` integration
- Docs: `docs/07-open-items.md`, `docs/08-onboarding.md`（API 只读端点摘要）

- [ ] **Step 1: 全链路**

- local-mode app + temp DB + seeded data; login not needed for GET.
- Call all six endpoints; assert 200 JSON shapes, filters/pagination, 404.
- API no-store/CSP headers already covered; add one assertion for a read endpoint.

- [ ] **Step 2: Docs + full verification**

- docs/07 M2b bullet with counts; docs/08 add a short "只读 API" section listing endpoints.
- `cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings`
- `cargo test -p nuwa -p shanhaijing`
- Commit: `docs: record M2b read API`.
- Do not push/tag/deploy.

## 自审记录

- Spec coverage: overview/issues/detail/runners/events/trends endpoints and required fields; settings/system/transcript/actions are M2c.
- No placeholders; interfaces and test expectations are concrete.
- Store extension keeps legacy `IssueRow` consumers working (new fields only); API local-mode tests avoid auth.
- Known scale note: list/attention filtering is in Rust over `list_issues`; acceptable for the single-owner console, revisit with pagination SQL if issue counts grow.
