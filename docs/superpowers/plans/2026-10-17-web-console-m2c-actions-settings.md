# Web 控制台 M2c 动作/设置/系统/转录 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成 v3 API 的写侧与治理面：reopen/requeue/sync、runner 重启、设置读写/测试/重启、system 信息、修复会话转录。完成后 API 可支撑 M3 的 SPA。

**Architecture:** 动作直接复用 M1a Store API；设置以 `nuwa.env` 为事实源，AppState 持有 env/db/unit 路径以便测试；同步/重启有互斥与活动 run 保护；转录从 `runs.session_dir` 服务端派生并解析 JSONL，永不接受客户端路径。

**Tech Stack:** axum、tokio、reqwest（现有）、serde_json、现有 Store。

**Spec:** docs/09 §9.2（actions/settings/system）、§13（配置）、§10.5（转录）；ADR-009。

## Global Constraints

- 自建 crate 零 clippy；写端点必须 CSRF + 会话（本地模式仍需 CSRF）。
- 不改变 M0/M1/M2a/M2b 行为；不触碰 vendored/guard/deliver/migration。
- 设置写回保留注释/未知键、Unix 原子替换/chmod 600，密钥永不回显。
- 动作必须有状态守卫；sync/restart 有超时/互斥/活动 run 检查。
- 测试使用 temp DB/env/会话目录；不调用真实 GitHub/systemd（成功路径手动验证）。
- 每个任务一个 commit；不 push/tag/deploy。

---

### Task 1: reopen / requeue 动作

**Files:** `crates/nuwa/src/api/actions.rs`（新）、`api/mod.rs`、`api/common.rs`（暴露 stuck 阈值）
**Interfaces:**
- `POST /api/v1/issues/{fp}/reopen`（JSON `{reason?}`）→ 204；404/409。
- `POST /api/v1/issues/{fp}/requeue`（JSON `{force?, reason?}`）→ 204；404/409/422。
- 复用 `Store::reopen_issue`、`Store::requeue_stuck`；stuck 阈值用 `common::issue_stuck_after_ms`（owner interval + `NUWA_STUCK_AFTER_SECS` floor）。
- 错误映射：UnknownIssue→404，InvalidTransition/NotStuck→409，validation→422。

**Tests:** local-mode app + `/auth/me` 取 CSRF cookie/header；seed rejected/escalated → reopen 204 + state new；seed stale fixing → requeue 204；healthy owner → 409 `NotStuck`；force → 204；unknown fp → 404.

**Commit:** `feat(api): owner reopen and requeue actions`.

---

### Task 2: sync 与 runner 重启

**Files:** `api/actions.rs`/`api/system.rs`、`ci.rs`（结构化汇总）、`serve.rs`（AppState 字段）、`api/mod.rs`
**Interfaces:**
- `POST /api/v1/sync` → 200 `{checked, merged, rejected, still_open, errors[]}`；无 token→502；30s 超时；进程内互斥→409。用 `ci::reconcile_all` 的结果行计数（或新增结构化 summary）。
- `POST /api/v1/runners/{id}/restart` → 202；未知 runner→404；无 service unit/invalid unit→422/501；用 `systemd-run --on-active=2 systemctl restart <unit>`；unit 白名单。
- AppState 增加 `serve_unit: String`、`runner_units: Vec<String>`（从 env 解析）；测试可构造。
- 测试不触发真实 systemd：覆盖未知 runner、非法 unit/缺 unit；sync 无 token→502；并发/成功路径手动。

**Commit:** `feat(api): sync and runner restart actions`.

---

### Task 3: 设置读取与写回

**Files:** `crates/nuwa/src/settings.rs`、`api/settings.rs`（新）、`serve.rs`（AppState `env_path`/`db_path`）、tests
**Interfaces:**
- AppState 增加 `env_path: PathBuf`、`db_path: PathBuf`（serve 从 `NUWA_ENV_FILE`/`--db` 设置；测试传 temp）。
- `GET /settings`：schema（§13.1 键：type/default/options/secret/scope/requires_restart/help）、values（非密钥明文，密钥 set/last4/updated）、env_path/exists/permissions/needs_migration/restart_pending/effective_diff/unknown_keys/last_changes。
- `PUT /settings` body `{values:{KEY:...}}`：空密钥保持；校验；未知 key/只读 key→422；原子写回；config_changed 审计；返回 `{changed, restart_required}`。
- `restart_pending` = 最近 `config_changed` 事件 ts > serve started_at。
- `effective_diff` 从 runner `config_json` 对比非密钥键。
- 保留旧 `/settings` 页面路径（legacy）不动。

**Tests:** temp env 文件（含注释/未知键/小写键）、GET 掩码/默认/权限、PUT 大写写回/变更 key/未知 key 422/数值校验、restart_pending、effective_diff。

**Commit:** `feat(api): settings schema, read and write`.

---

### Task 4: 设置测试/重启与 system

**Files:** `api/settings.rs`、`api/system.rs`（新）、`api/mod.rs`、tests
**Interfaces:**
- `POST /settings/test` `{target:"github"|"llm", values?}`：GitHub `GET /rate_limit`；LLM 轻量端点；无网络/无效目标→502/422；测试只覆盖无效目标/空 token，不联网。
- `POST /settings/restart` `{force?}`：活动 run (`active_runs`) 且非 force→409（body 含 active_runs）；unit 白名单；`systemd-run`；成功 202；非 systemd/无 unit→501；审计 `restart_requested`。
- `GET /system`：version、build_commit、started_at_ms、uptime_secs、db_path/size、local_mode、services（systemctl is-active，n/a 可接受）。
- 测试：restart 409 guard；非法 unit 422；system 字段/本地模式；test invalid target/empty token。

**Commit:** `feat(api): settings test/restart and system info`.

---

### Task 5: 会话转录

**Files:** `api/transcript.rs`（新）、`api/mod.rs`、tests
**Interfaces:**
- `GET /issues/{fp}/transcript?run_id=&page=1&page_size=20`。
- 选 run：指定 run_id 必须属于该指纹；否则最近有 `session_dir` 的 run；404/empty。
- 在 `session_dir` 下找最新 `.jsonl`；只读 2MB 尾部；只解析 `kind=entry,type=message`；映射 user/assistant（thinking/text/toolCall）/tool_result；单块≤16KB；分页；meta `{page,page_size,total,truncated,empty}`。
- 从不接受客户端路径；路径由 DB 派生。
- 测试：temp JSONL fixture（user/assistant thinking/text/toolCall/tool_result/未知记录）、分页、截断、未知 run、跨指纹 run 404、无 session empty。

**Commit:** `feat(api): parsed repair transcript`.

---

### Task 6: 集成、文档、验证

- 组合测试：seed temp DB/env/session；调用 reopen/requeue/sync(no token 502)/settings GET/PUT/test invalid/restart guard/system/transcript；断言 CSRF（无 403/有 204）、状态、掩码、审计、404/409/422、no-store/CSP。
- docs/07 M2c bullet（动作/设置/system/转录、测试数）；docs/08 增加动作/设置/转录/重启的操作与安全说明（UI token、CSRF、env 文件、systemd units）。
- clippy + full tests.
- Commit: `docs: record M2c actions and governance`.

## 自审记录

- Spec coverage: all M2c endpoints from docs/09 §9.2/§13/§10.5; no frontend work.
- No placeholders; interfaces/tests concrete.
- Path-parameterized settings avoid global env in tests; systemd/GitHub success paths are manual, error paths automated.
- AppState new fields require updating existing test helpers; do this in Task 3 before adding handlers.
