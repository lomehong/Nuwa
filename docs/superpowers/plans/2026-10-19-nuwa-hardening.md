# Nuwa 全量加固实施计划

> **Status (2026-10-20):** 六批 A–F 已实施并自测；工作树未提交/未推送，等待所有者确认。
>
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Nuwa 自愈闭环的失败模式（基线/分支/指令文件/超时/密钥）和人的闭环节点（证据/忽略/重试记忆/通知/成本）系统性补齐，使回归修复、失败重试和无人值守运行不再依赖人工救火。

**Architecture:** 新增 `git_ops`、`secrets`、`verify`、`webhook` 四个聚焦模块；`deliver` 改为“先验证、再交付、失败回滚”，`work` 改为“先 fetch/reset、再校验指令文件、再领自己仓库的单”；`shanhaijing` schema 升 v3，新增 `ignored` 终态、`attempts`、`progress_at`。所有交付证据、PR 正文、转录先脱敏再落库/外发。

**Tech Stack:** Rust 2021, tokio, axum, rusqlite, globset, sha2, hmac, reqwest; React 19 + TanStack Query; Go 1.24 collector SDK.

**Spec:** 本计划直接实现对话中已批准的六批设计（A-F）；设计决策记录在 `docs/04-decisions.md` 的新 ADR 与 `docs/07-open-items.md` §D。

## Global Constraints

- `crates/rpi-*` 是 vendored：不改名、不重构、不美化；bash 隔离只使用现有 `BashToolOptions.prepare`。
- `nuwa` / `shanhaijing` 零 clippy warning；行为改动必须带测试；`guard.rs` / `deliver.rs` 改动必须同步护栏测试。
- 不自动 commit/push；全部完成并自测后等所有者确认。
- 本机构建前先 `taskkill /F /IM nuwa.exe`；验证用 `C:\Users\lome\.cargo\shared-target` 下的产物，不用 `E:\...\target\debug`。
- `rpi-tools` 的 `execution_env_conformance::os_cancel_interrupts_even_when_timeout_is_set` 在 Windows 失败是上游预存问题，不作为本计划失败。
- Go 代码：Go 1.22+，整数循环用 `for i := range n`（spike/target-repo seeded bug 除外）。
- Web 验证：`npm run typecheck && npm run lint && npm test && npm run build`。
- 每批完成后更新 `docs/07-open-items.md` §D；接入变化更新 `docs/08-onboarding.md`；新决策走 `docs/04-decisions.md` ADR。

---

## 文件结构

**新增：**

- `crates/nuwa/src/git_ops.rs` — 带超时的 git 网络操作、fetch/reset、分支存在/删除、当前分支、干净树检查。
- `crates/nuwa/src/secrets.rs` — 启动时读入并清除进程 env 中的密钥、agent 子进程 env 白名单、文本/会话脱敏。
- `crates/nuwa/src/verify.rs` — base/branch/reproducer 验证协议、交付面 diff 终检、风险画像。
- `crates/nuwa/src/webhook.rs` — HMAC webhook 发送器、事件 JSON、重试/超时。

**主要修改：**

- `crates/nuwa/src/deliver.rs` — 使用 `git_ops`/`verify`/`secrets`，`DeliveryGuard` 回滚。
- `crates/nuwa/src/main.rs` — preflight、指令文件校验、分支预检、失败分类、成本闸、prompt 历史、repo 过滤领单。
- `crates/nuwa/src/instructions.rs` — `reproduce_check`/`max_attempts`/`draft_first_n`，可信副本与哈希校验。
- `crates/nuwa/src/repair.rs` — 显式 secrets、bash env 白名单、prior context、run 结束脱敏。
- `crates/nuwa/src/issue.rs` — prompt 增加 `PRIOR ATTEMPTS`/`OWNER NOTE`；`FailingCheck.timeout_secs`。
- `crates/nuwa/src/guard.rs` — 隐式禁改 `nuwa.repo.json`。
- `crates/nuwa/src/ci.rs` — reconcile 结果事件化，供 webhook 消费。
- `crates/nuwa/src/serve.rs` — 自动 reconcile 任务、webhook 初始化、进度心跳。
- `crates/nuwa/src/api/actions.rs` — ignore、reopen reason、共享 sync lock。
- `crates/nuwa/src/api/issues.rs` — `ignored` 状态、attempts 字段。
- `crates/nuwa/src/api/trends.rs` / `overview.rs` / `runners.rs` — 交付质量指标、last_signal_at、progress。
- `crates/nuwa/src/settings.rs` — 新 env 键 schema。
- `crates/shanhaijing/src/lib.rs` / `migration.rs` — schema v3、Ignored、attempts、progress_at、查询。
- `sdk/qingniao-go/qingniao.go` — HTTPSink timeout/retry/fallback。
- `web/src/features/issues/*`、`issue-detail/*`、`components/ui/StateBadge.tsx`、`features/settings/*`、`features/overview/*` — ignored/attempts/指标。
- `README.md`、`docs/01`、`docs/03`、`docs/04`、`docs/07`、`docs/08` — 状态与决策同步。

---

## 批次 A：基线、分支与交付回滚

### Task A1: `git_ops` 带超时的 git 操作

**Files:**
- Create: `crates/nuwa/src/git_ops.rs`
- Modify: `crates/nuwa/src/main.rs` (`mod git_ops;`)
- Test: `crates/nuwa/src/git_ops.rs` 内联 tests

**Interfaces:**
- Produces:
  - `pub struct GitOutput { pub success: bool, pub stdout: String, pub stderr: String }`
  - `pub fn git_with_timeout(repo: &Path, args: &[&str], timeout: Duration) -> Result<GitOutput>`
  - `pub fn git_ok(repo: &Path, args: &[&str], timeout: Duration) -> Result<String>`
  - `pub fn current_branch(repo: &Path) -> Result<String>`
  - `pub fn branch_exists(repo: &Path, branch: &str) -> Result<bool>`
  - `pub fn delete_local_branch(repo: &Path, branch: &str) -> Result<()>`
  - `pub fn delete_remote_branch(repo: &Path, branch: &str, timeout: Duration) -> Result<()>`
  - `pub fn fetch_base(repo: &Path, base: &str, timeout: Duration) -> Result<()>`
  - `pub fn reset_to_base(repo: &Path, base: &str) -> Result<()>`

- [ ] **Step 1: Write failing tests** in `git_ops.rs`:
  - `git_with_timeout_kills_a_hanging_command`: run `sh -c 'sleep 30'` (Windows: `cmd /C ping -n 30 127.0.0.1`) with 200ms timeout; assert error contains `timed out` and elapsed < 5s.
  - `fetch_and_reset_moves_to_remote_main`: create temp bare origin + clone, commit in a second clone, push; in first clone call `fetch_base("main")`, `reset_to_base("main")`, assert HEAD equals origin/main.
  - `branch_delete_removes_local_and_remote`: create branch, push, delete local/remote, assert `branch_exists` false and `ls-remote` empty.
  - `current_branch_reports_head`.

- [ ] **Step 2: Run tests, expect failure**:
  `cargo test -p nuwa git_ops -- --nocapture`

- [ ] **Step 3: Implement**:
  - Spawn with `tokio::process::Command`; `stdout/stderr` piped; `kill_on_drop(true)`.
  - On Unix: `process_group(0)` and on timeout `kill -KILL -<pid>`; on Windows: `taskkill /T /F /PID <pid>`.
  - `fetch_base`: `git fetch --prune origin <base>`; retry 3x with 2s backoff on failure? Keep single call + caller retry; tests use local remotes.
  - `reset_to_base`: `git reset --hard origin/<base>`.
  - `delete_remote_branch`: `git push origin --delete <branch>`; tolerate "remote ref does not exist" as success.

- [ ] **Step 4: Run tests, expect pass**.

### Task A2: preflight fetch/reset、错误分支自愈、分支预检

**Files:**
- Modify: `crates/nuwa/src/main.rs` (`work_once`)
- Modify: `crates/nuwa/src/deliver.rs` (expose base detection helper if needed)
- Test: `crates/nuwa/src/main.rs` tests

**Interfaces:**
- Consumes: `git_ops::*`.
- Produces: `fn preflight_repo(repo_dir: &Path, base: &str) -> anyhow::Result<()>`; `fn stale_branch_for(fp: &str) -> String`.

- [ ] **Step 1: Write failing tests**:
  - `preflight_recovers_from_agent_branch`: temp repo on `agent/fix/abc`, clean; call `preflight_repo(repo, "main")`; assert HEAD main.
  - `preflight_fetches_remote_main`: local main behind origin; call; assert HEAD == origin/main.
  - `stale_branch_is_deleted_before_repair`: create `agent/fix/<fp>`; call `prepare_branch(repo, fp)`; assert branch gone.
  - `fetch_failure_does_not_claim`: use a repo with a nonexistent origin; call `preflight_repo`; assert error; no claim (test at work_once level with a seeded DB and a fake repo).

- [ ] **Step 2: Run tests, expect failure**.

- [ ] **Step 3: Implement**:
  - In `work_once`, after quota gate:
    ```
    let base = if head == "master" { "master" } else { "main" };
    preflight_repo(repo_dir, base)?;
    ```
  - `preflight_repo`:
    - `current_branch`; if `agent/fix/*` and clean (excluding `nuwa.repo.json` later) → `git checkout <base>`;
    - if not base → bail with actionable message;
    - `fetch_base`; `reset_to_base`;
    - re-check dirty; if dirty, existing auto-reset logic (now includes `nuwa.repo.json` in batch B).
  - After claim, before `start_run`/repair:
    - `let branch = format!("agent/fix/{}", snapshot.short_fingerprint())` (need a helper on `IssueSnapshot` or compute from fingerprint with same sanitization);
    - if `branch_exists` → `delete_local_branch`; `delete_remote_branch` best-effort; if remote delete fails, `set_state(Escalated)` with message and return `Ok(true)` before LLM.
  - Use a helper `short_fingerprint(fp: &str)` shared with `issue.rs` to avoid duplication; move to `shanhaijing` or `nuwa::issue`.

- [ ] **Step 4: Run tests, expect pass**.

### Task A3: `DeliveryGuard` 回滚

**Files:**
- Modify: `crates/nuwa/src/deliver.rs`
- Test: `deliver.rs` tests + `main.rs` integration test

**Interfaces:**
- Produces: `struct DeliveryGuard<'a> { repo: &'a Path, base: String, branch: Option<String>, committed: bool }` with `Drop` restoring `base` and deleting `branch` on failure; `disarm()` on success.
- `pub async fn deliver(..., base: &str, ...)` — caller passes the freshly fetched base.

- [ ] **Step 1: Write failing test**:
  - `delivery_failure_restores_base`: temp repo; call `deliver` with `push=false` but force a failure after `checkout -b` (e.g., make repo's `.git/objects` read-only? Simpler: call an internal `run_delivery_steps` with a `FailPoint::Commit` test hook, or create a pre-existing file conflict). Prefer a test-only `#[cfg(test)] fail_after_branch: Option<FailPoint>` parameter in a private `deliver_inner`; public `deliver` passes `None`. Assert HEAD is main after error.
  - `delivery_success_keeps_branch_and_returns_to_base`: faux path; assert HEAD main and branch exists.

- [ ] **Step 2: Run test, expect failure**.

- [ ] **Step 3: Implement**:
  - Split `deliver` into `deliver_inner(..., fail_point: Option<FailPoint>)`.
  - Create guard immediately after `base` is known; `committed` false until commit; on Drop: `git checkout base`; if `!committed` and branch set, `git branch -D branch`.
  - On success: guard.disarm() after the final checkout; keep branch.
  - Ensure push failure path also goes through guard (remove the manual checkout).
  - `deliver` uses `git_ops::git_with_timeout` for push.

- [ ] **Step 4: Run tests, expect pass**.

### Task A4: 终端分支清理

**Files:**
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/shanhaijing/src/lib.rs` (query by branch/state)
- Test: `main.rs` tests

**Interfaces:**
- Produces: `fn cleanup_terminal_branches(store: &Store, repo: &Path) -> Result<usize>`; `Store::terminal_fingerprints() -> ShanResult<Vec<(String, String)>>` (short fp, state) or `Store::issue_state(fp)`.
- Use `runs.branch` to map local branches to fingerprints; fall back to `agent/fix/<short_fp>` naming.

- [ ] **Step 1: Write failing test**:
  - Seed store with issues in merged/rejected/ignored/escalated; create local branches; call cleanup; assert only terminal branches deleted.
- [ ] **Step 2: Implement**:
  - List local branches via `git for-each-ref --format=%(refname:short) refs/heads/agent/fix/`.
  - For each, parse short fp; find issue row by matching `short_fingerprint`; if state terminal, delete local + remote best-effort.
  - Call at `run_work` startup and every 30 cycles (or after each successful delivery).
- [ ] **Step 3: Tests pass**.

---

## 批次 B：指令文件完整性

### Task B1: `TrustedInstructions` 与哈希校验

**Files:**
- Modify: `crates/nuwa/src/instructions.rs`
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub struct TrustedInstructions { pub instructions: RepoInstructions, pub bytes: Vec<u8>, pub sha256: String, pub source: TrustSource }`
  - `pub enum TrustSource { Tracked, PrivateCopy }`
  - `pub fn load_trusted(repo_dir: &Path, path: &Path, base: &str, store_dir: &Path) -> Result<TrustedInstructions>`
  - `pub fn verify_and_restore(&self, path: &Path) -> Result<bool>` (true = restored)
- `RepoInstructions` gains:
  - `#[serde(default)] pub reproduce_check: Option<FailingCheck>`
  - `#[serde(default = "default_max_attempts")] pub max_attempts: u32` (default 3)
  - `#[serde(default)] pub draft_first_n: u32` (default 0)

- [ ] **Step 1: Tests**:
  - `tracked_config_is_read_from_base_not_worktree`: temp repo with committed `nuwa.repo.json`; modify worktree; load; assert parsed value from commit.
  - `untracked_config_is_copied_to_private_store`: temp repo without tracked config; load; assert private copy exists and source PrivateCopy.
  - `tampered_config_is_restored`: after load, modify worktree; `verify_and_restore` returns true and bytes match.
  - `invalid_json_is_an_error_with_path`.
- [ ] **Step 2: Implement** using `sha2::Sha256`, `git show origin/<base>:nuwa.repo.json` via `git_ops::git_ok`.
  - Private store path: `store_dir.join(format!("instructions-{}.json", sha256(repo canonical path)[..12]))`; create dirs.
  - If tracked config missing in base but worktree exists, fall back to private copy / first copy.
- [ ] **Step 3: Tests pass**.

### Task B2: guard 隐式禁改 + preflight 恢复

**Files:**
- Modify: `crates/nuwa/src/guard.rs`
- Modify: `crates/nuwa/src/main.rs`
- Test: `guard.rs` tests + `main.rs` tests

**Interfaces:**
- `Guards::new(forbidden)` appends `nuwa.repo.json` if absent.
- `work_once` takes `trusted: &TrustedInstructions` and calls `verify_and_restore` before every claim; if restored, record `config_tampered` event.
- `main.rs` loads `TrustedInstructions` at startup.

- [ ] **Step 1: Tests**:
  - `config_file_edit_is_blocked` for edit/write/bash.
  - `config_tamper_is_restored_before_claim`: seed DB; modify file; run preflight; assert file restored and event recorded.
  - `tracked_config_cannot_be_weakened_across_restart`: simulate load twice; modified worktree ignored.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task B3: 脏树/clean/deliver 的排除语义

**Files:**
- Modify: `crates/nuwa/src/main.rs` (dirty check + clean)
- Modify: `crates/nuwa/src/deliver.rs` (status/add)
- Test: `main.rs` / `deliver.rs`

- [ ] **Step 1: Tests**:
  - config-only modification is visible to preflight dirty check and restored/cleaned.
  - config modification is never committed to the agent branch.
- [ ] **Step 2: Implement**:
  - Remove `:(exclude)nuwa.repo.json` from preflight status/clean.
  - Keep `:(exclude)nuwa.repo.json` in deliver `status`/`add` (so it isn't delivered) but restore trusted copy before delivery.
- [ ] **Step 3: Tests pass**.

---

## 批次 C：密钥、环境与脱敏

### Task C1: `secrets.rs`

**Files:**
- Create: `crates/nuwa/src/secrets.rs`
- Modify: `crates/nuwa/src/main.rs`
- Test: inline tests

**Interfaces:**
- `pub struct Secrets { pub github_token: Option<String>, pub zai_key: Option<String>, pub anthropic_key: Option<String> }`
- `impl Secrets { pub fn load_and_scrub_env() -> Self; pub fn redact(&self, text: &str) -> String; pub fn redact_session_dir(&self, dir: &Path) -> Result<()>; }`
- `pub fn agent_env_allowlist() -> BTreeMap<String, String>`
- Known secret keys: `["NUWA_GITHUB_TOKEN","GITHUB_TOKEN","ZAI_API_KEY","ANTHROPIC_API_KEY","NUWA_UI_TOKEN","NUWA_INGEST_TOKEN"]`.

- [ ] **Step 1: Tests**:
  - `load_and_scrub_removes_known_keys`: set env vars, load, assert `std::env::var` missing.
  - `redact_replaces_exact_and_patterns`: `ghp_xxx`, `sk-ant-...`, `Bearer ...`.
  - `redact_session_dir_rewrites_jsonl`: temp dir with JSONL containing a secret; assert replaced.
  - `allowlist_excludes_secrets_and_includes_path`.
- [ ] **Step 2: Implement**.
  - Use `sha2` not needed; simple replace.
  - `redact_session_dir`: walk files ending `.jsonl`, read line by line, replace, write `.tmp`, rename.
- [ ] **Step 3: Tests pass**.

### Task C2: provider/deliver 显式 token

**Files:**
- Modify: `crates/nuwa/src/repair.rs`
- Modify: `crates/nuwa/src/deliver.rs`
- Modify: `crates/nuwa/src/main.rs`
- Test: `repair.rs` / `deliver.rs`

**Interfaces:**
- `RepairJob` gains `pub secrets: Arc<Secrets>`.
- `repair::run` uses `job.secrets.zai_key/anthropic_key`; no `env::var` for keys.
- `deliver(..., github_token: Option<&str>, secrets: &Secrets, ...)`; `create_pr_via_api(token, ...)`.
- `ci::wait_for_ci` call in `main.rs` uses `secrets.github_token`.

- [ ] **Step 1: Tests**:
  - `provider_uses_injected_key` (faux can't test; unit-test a helper `provider_for` with keys and assert error message when missing).
  - `deliver_without_token_fails_before_network_with_clear_message`.
  - `env_scrubbed_process_cannot_read_token_after_load`.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task C3: bash/验证子进程 env 白名单

**Files:**
- Modify: `crates/nuwa/src/repair.rs`
- Modify: `crates/nuwa/src/deliver.rs`
- Test: `repair.rs` / `deliver.rs`

**Interfaces:**
- `repair.rs` builds `BashToolOptions { prepare: Some(Arc::new(|exec| { exec.inherit_env=false; exec.env=allowlist(); ... })), default_timeout: Some(300.0) }`.
- `verify::run_check` uses `Command::env_clear()` + allowlist.

- [ ] **Step 1: Tests**:
  - `bash_child_does_not_see_process_secret`: use a faux provider script that runs `bash` with `env` and writes output; assert secret absent. Or unit-test the prepare closure directly.
  - `check_command_env_is_whitelisted`: temp repo; run `env` as failing check; assert no secret.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task C4: 证据/PR/转录脱敏

**Files:**
- Modify: `crates/nuwa/src/deliver.rs`
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/nuwa/src/repair.rs`
- Test: inline tests

- [ ] **Step 1: Tests**:
  - `verification_detail_redacts_secret`.
  - `pr_body_does_not_contain_secret`.
  - `run_error_is_redacted`.
  - `session_dir_is_redacted_after_run` (integration with temp JSONL).
- [ ] **Step 2: Implement**:
  - Redact `check.output`, `report.root_cause_text`, `format!` errors before `bounded_text`.
  - After `repair::run`, call `secrets.redact_session_dir(&report.session_dir)`.
- [ ] **Step 3: Tests pass**.

---

## 批次 D：验证协议

### Task D1: 配置字段 + `verify` 骨架

**Files:**
- Modify: `crates/nuwa/src/issue.rs` (`FailingCheck.timeout_secs`)
- Modify: `crates/nuwa/src/instructions.rs`
- Create: `crates/nuwa/src/verify.rs`
- Modify: `crates/nuwa/src/main.rs` (`mod verify;`)
- Test: inline tests

**Interfaces:**
- `FailingCheck { command: String, target: Option<String>, #[serde(default = "default_check_timeout")] timeout_secs: u64 }` default 600.
- `verify::CheckOutput { pub ok: bool, pub timed_out: bool, pub output: String }`
- `verify::VerificationEvidence { pub mode: String, pub base: Option<CheckOutput>, pub branch: CheckOutput, pub reproduce: Option<CheckOutput>, pub diff: DiffSummary }`
- `verify::verify(repo, issue, config, base, env_allowlist, redactor) -> Result<VerificationEvidence>`

- [ ] **Step 1: Tests**:
  - config parse defaults.
  - `verify` returns mode `configured`, `failing_check`, or `reproducer`.
- [ ] **Step 2: Implement skeleton with `run_check` timeout**.
- [ ] **Step 3: Tests pass**.

### Task D2: red-green 三模式 + base worktree

**Files:**
- Modify: `crates/nuwa/src/verify.rs`
- Test: inline tests

**Interfaces:**
- `fn run_in_worktree(repo, base, command, timeout, env, copy_reproducer) -> Result<CheckOutput>`
- Reproducer path: `.nuwa/repro.sh` (Unix) / `.nuwa/repro.cmd` (Windows); if both absent, no reproducer.

- [ ] **Step 1: Tests** with a temp repo:
  - `configured_reproduce_must_fail_on_base_and_pass_on_branch`.
  - `failing_check_red_on_base_is_used_as_reproducer`.
  - `base_green_without_reproducer_is_rejected`.
  - `base_green_with_reproducer_fails_on_base_passes_on_branch`.
  - `branch_failing_check_must_pass`.
- [ ] **Step 2: Implement**:
  - Create temp worktree: `git worktree add --detach <tmp> origin/<base>`; remove in a guard.
  - Copy `.nuwa/repro.*` from branch to base worktree for the fallback.
  - Run commands with timeout and env allowlist.
- [ ] **Step 3: Tests pass**.

### Task D3: diff 终检 + glob + 风险画像

**Files:**
- Modify: `crates/nuwa/src/verify.rs`
- Modify: `crates/nuwa/src/deliver.rs`
- Test: inline tests

**Interfaces:**
- `pub struct DiffSummary { pub files: Vec<DiffFile>, pub touches_tests: bool, pub touches_ci: bool, pub forbidden_hits: Vec<String>, pub added: u64, pub removed: u64 }`
- `pub struct DiffFile { pub status: char, pub path: String }`
- `pub fn diff_summary(repo, base, branch, forbidden: &[String]) -> Result<DiffSummary>`
- Glob matching via `globset::GlobBuilder` with literal separator false; normalize `\` to `/`; directory patterns (`deploy/`) become `deploy/**`.

- [ ] **Step 1: Tests**:
  - `star_pem_matches_real_file`.
  - `directory_pattern_matches_nested`.
  - `forbidden_hit_rejects_delivery`.
  - `test_change_sets_risk_flag`.
- [ ] **Step 2: Implement**.
  - In `deliver`, after verification and before commit, call diff_summary on the working tree vs base; use `git diff --name-status base` (working tree + untracked? use `git add -N`? Simpler: after `git add -A` excluding config, `git diff --cached --name-status base`).
  - Reject forbidden hits; include risk in `DeliverReport`.
- [ ] **Step 3: Tests pass**.

### Task D4: `run_check` 超时/杀进程树 + 证据格式

**Files:**
- Modify: `crates/nuwa/src/verify.rs`
- Modify: `crates/nuwa/src/deliver.rs`
- Test: inline tests

- [ ] **Step 1: Tests**:
  - `check_timeout_kills_process_tree` (a shell that spawns a child sleeper; assert no leftover).
  - `evidence_contains_base_branch_reproduce_sections`.
- [ ] **Step 2: Implement** using `git_ops`-style process runner; share a `run_process` helper (move to `git_ops` or a new `proc.rs`).
- [ ] **Step 3: Tests pass**.

### Task D5: docs/08 模板

**Files:**
- Modify: `docs/08-onboarding.md`

- [ ] Add `reproduce_check` example, `.nuwa/repro.sh` template, explanation of red-green and escalate behavior.
- [ ] Add non-Go raw envelope example.
- [ ] Update known boundaries.

---

## 批次 E1：ignored / attempts / reason / 优先级

### Task E1.1: schema v3 + State::Ignored + attempts

**Files:**
- Modify: `crates/shanhaijing/src/migration.rs`
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: inline tests

**Interfaces:**
- `SCHEMA_VERSION = 3`.
- `migrate_v3`: `ALTER TABLE issues ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0; ALTER TABLE runners ADD COLUMN progress_at INTEGER NOT NULL DEFAULT 0; CREATE INDEX ...`.
- `State::Ignored`; `is_terminal` includes it; `can_transition` adds `(New|Escalated|Rejected -> Ignored)` and `(Ignored -> New)`.
- `IssueRow.attempts`, `IssueSnapshot.attempts`; update selects/from_row.
- `Store::increment_attempts(fp) -> u64`; `Store::ignore_issue(fp, reason, actor, now)`; `reopen_issue` accepts Ignored.
- `report_signal`: Ignored does not auto-reopen; add `ReportOutcome::Muted` or reuse Deduped.

- [ ] **Step 1: Tests**:
  - migration v2 -> v3 adds columns, preserves data.
  - ignored transition guards.
  - report on ignored is suppressed.
  - attempts increment/read.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E1.2: ignore/reopen API/CLI + reason 持久化

**Files:**
- Modify: `crates/nuwa/src/api/actions.rs`
- Modify: `crates/nuwa/src/main.rs` (CLI)
- Test: `actions.rs` tests

**Interfaces:**
- `POST /api/v1/issues/{fp}/ignore { reason }`.
- `ReopenRequest { reason, #[serde(default)] reset_attempts: bool }`; store reason in event detail `reopened by owner: <reason>`; reset attempts if requested.
- CLI `nuwa ignore --db <path> --fingerprint <fp> [--reason <text>]`; `nuwa reopen` gains `--reason`, `--reset-attempts`.
- `Store::reopen_issue(fp, now, reason, reset_attempts)`; `Store::ignore_issue`.

- [ ] **Step 1: Tests**:
  - ignore returns 204 and state ignored; event detail contains reason.
  - reopen ignored works and resets attempts.
  - invalid states return 409.
  - CLI parser tests.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E1.3: attempts cap + 优先级

**Files:**
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/shanhaijing/src/lib.rs`
- Test: inline tests

**Interfaces:**
- `work_once`: after claim, if `snapshot.attempts >= repo_config.max_attempts` → set Ignored with reason `attempt cap reached (N)` and return `Ok(true)` without LLM.
- Increment attempts on issue-level failure only: in repair failure/delivery failure/verification failure/budget exceeded paths; `Store::increment_attempts`.
- `READY_ORDER` becomes severity-first:
  ```sql
  ORDER BY CASE severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3 WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END DESC,
           occurrences DESC, last_seen ASC
  ```
- Optionally cap occurrences in display/order: `MIN(occurrences, 100)`; keep raw for display.

- [ ] **Step 1: Tests**:
  - `attempt_cap_ignores_issue_before_llm`.
  - `critical_single_beats_medium_seven`.
  - `issue_level_failure_increments_attempts`; `provider_transient_does_not`.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E1.4: prompt 历史回灌

**Files:**
- Modify: `crates/nuwa/src/issue.rs`
- Modify: `crates/nuwa/src/repair.rs`
- Modify: `crates/nuwa/src/main.rs`
- Test: `issue.rs` / `main.rs`

**Interfaces:**
- `Issue` gains `pub prior_context: String` (default empty via `#[serde(default)]`).
- `user_prompt()` appends:
  ```
  PRIOR ATTEMPTS:
  - 2026-... outcome=verification_failed ci=- root_cause=... error=...
  OWNER NOTE:
  - <latest owner event detail>
  ```
  bounded to 4KB.
- `main.rs` builds `prior_context` from `Store::recent_runs(fp, 3)` + `Store::latest_owner_event(fp)`.
- `Store::recent_runs(fp, limit) -> Vec<RunRow>`; `Store::latest_owner_event(fp)`.

- [ ] **Step 1: Tests**:
  - prompt includes prior root cause/error and owner reason; no history → no section.
  - bounded length.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E1.5: 前端 ignored/attempts

**Files:**
- Modify: `web/src/components/ui/StateBadge.tsx`
- Modify: `web/src/features/issues/actions.ts`
- Modify: `web/src/features/issues/IssueActionButton.tsx`
- Modify: `web/src/features/issues/filters.ts`
- Modify: `web/src/features/issues/types.ts`
- Modify: `web/src/features/issue-detail/*`
- Test: existing `.test.tsx` + new tests

- [ ] Add `ignored` label/color, filter, action `ignore` with reason dialog (reuse ConfirmDialog pattern), attempts display in table/detail.
- [ ] Update API types with `attempts`.
- [ ] Tests: ignore action posts reason; ignored state renders; filters include ignored; attempts visible.

---

## 批次 E2：进度心跳 / 自动对账 / 通知 / 失败分类

### Task E2.1: progress_at + stuck + heartbeat_stale

**Files:**
- Modify: `crates/shanhaijing/src/migration.rs` (already in E1.1)
- Modify: `crates/shanhaijing/src/lib.rs` (`RunnerUpsert`, `RunnerHeartbeat`, `RunnerRow`, `upsert/heartbeat`, `stuck`)
- Modify: `crates/nuwa/src/main.rs` (`RunnerLive`, `live_update`, `spawn_heartbeat`)
- Modify: `crates/nuwa/src/api/runners.rs`
- Test: inline tests

**Interfaces:**
- `RunnerLive.progress_at_ms`.
- `RunnerHeartbeat.progress_at`.
- `stuck()` healthy_owner uses `progress_at` freshness; fallback to heartbeat when progress_at=0.
- `work_once` calls `live.progress()` on phase transitions and turn progress.
- Webhook event `heartbeat_stale` emitted by a monitor: serve task or work heartbeat when `now - progress_at > threshold` while state fixing.

- [ ] **Step 1: Tests**:
  - `fresh_heartbeat_stale_progress_is_stuck`.
  - `working_progress_is_not_stuck`.
  - `progress_at_persisted`.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E2.2: repo 过滤领单 + orphan revive

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs`
- Modify: `crates/nuwa/src/main.rs`
- Test: inline tests

**Interfaces:**
- `claim_ready_issue_for_repo(now, cooldown, repo) -> ...` with `AND repo = ?`.
- Keep unfiltered `claim_ready_issue` for console/tests.
- `revive_orphaned_fixing(now, runner_id)` filters `owner_runner_id = ?1 OR owner_runner_id IS NULL`; update tests that expected all.
- `work_once` uses repo-filtered claim.

- [ ] **Step 1: Tests**:
  - two repos in DB; runner A claims only A.
  - runner A restart does not revive B's fixing issue.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E2.3: failure classification + standing-down

**Files:**
- Modify: `crates/nuwa/src/main.rs`
- Maybe create `crates/nuwa/src/failure.rs`
- Test: inline tests

**Interfaces:**
- `enum FailureClass { ProviderTransient, ProviderConfig, Environment, Issue, Delivery }`
- `fn classify_failure(err: &anyhow::Error) -> FailureClass` using keyword matching (`429`, `rate limit`, `timeout`, `connection`, `401`, `403`, `api key`, `dirty`, `toolchain`, etc.).
- `work_once`:
  - provider transient → `store.release_issue(fp, "provider transient: ...", now + 5min)` (new method; does not increment attempts), `stand_down` counter; after 3 consecutive, sleep/skip claims for 15min.
  - provider config → escalate + stand down.
  - environment → return Err/stand down; no issue escalation.
  - issue/delivery → escalate + increment attempts.
- `RunnerLive` gets `consecutive_provider_failures`, `stand_down_until_ms`.
- New store `release_issue` (triggered/fixing -> new, cooldown, reason) with transition allowed.

- [ ] **Step 1: Tests**:
  - classifier table test.
  - transient failure releases issue and doesn't increment attempts.
  - 3 transient failures set stand_down.
  - auth failure escalates and stands down.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E2.4: serve 自动 reconcile

**Files:**
- Modify: `crates/nuwa/src/serve.rs`
- Modify: `crates/nuwa/src/api/actions.rs` (move sync lock)
- Modify: `crates/nuwa/src/ci.rs` (return event data)
- Test: `serve.rs` tests

**Interfaces:**
- `api::common::sync_lock()` shared.
- `spawn_reconciler(store, token, interval)`; first tick after interval; skip if no token.
- `NUWA_SYNC_INTERVAL` default 600, range 60..86400.
- On changes/errors, emit webhook events via `webhook::emit`.

- [ ] **Step 1: Tests**:
  - short interval task reconciles a seeded pr_open issue (mock GitHub? `reconcile_all` calls network; factor a `reconcile_once` that accepts a `PrState` trait or use a fake token/URL? Simpler: test the scheduler function calls a provided closure; keep network path covered by existing `ci` tests? There are no network tests. Refactor `reconcile_all` to accept a `GithubApi` trait, with a real implementation and a fake in tests. This is a bigger refactor but improves testability.)
  - `sync_lock` is shared with manual sync.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E2.5: webhook + settings

**Files:**
- Create: `crates/nuwa/src/webhook.rs`
- Modify: `crates/nuwa/src/main.rs`/`serve.rs`
- Modify: `crates/nuwa/src/settings.rs`
- Test: inline tests

**Interfaces:**
- `WebhookConfig { url: String, secret: Option<String>, events: BTreeSet<String> }`
- `pub struct WebhookEvent { event: String, fingerprint: Option<String>, repo: Option<String>, detail: String, at_ms: i64 }`
- `pub fn spawn(config: Option<WebhookConfig>) -> WebhookHandle`; `handle.emit(event)` non-blocking.
- HMAC-SHA256 signature `sha256=<hex>` in `X-Nuwa-Signature`.
- Retry 3x with 1s/2s/4s backoff; 10s request timeout.
- Settings keys: `NUWA_WEBHOOK_URL` (url, serve), `NUWA_WEBHOOK_SECRET` (secret, serve), `NUWA_WEBHOOK_EVENTS` (csv, serve), `NUWA_SYNC_INTERVAL` (int, serve).
- Emit from: main escalation/ignore/ci red, serve reconcile errors, progress monitor.

- [ ] **Step 1: Tests**:
  - signature matches HMAC of body.
  - event filter skips unsubscribed.
  - retry on 500 then success.
  - no URL => disabled, no panic.
- [ ] **Step 2: Implement**.
- [ ] **Step 3: Tests pass**.

### Task E2.6: docs/ADR for E

**Files:**
- Modify: `docs/01-vision.md` (narrow webhook exception)
- Modify: `docs/04-decisions.md` (ADR-010)
- Modify: `docs/07-open-items.md` (snapshot)

---

## 批次 F：产品项与接入

### Task F1: 成本闸 + Anthropic model 硬错误

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs` (`tokens_used_since`)
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/nuwa/src/repair.rs`
- Modify: `crates/nuwa/src/settings.rs`
- Test: inline tests

**Interfaces:**
- `NUWA_DAILY_TOKEN_LIMIT` default 5_000_000; `NUWA_MONTHLY_TOKEN_LIMIT` default 50_000_000.
- `tokens_used_since(repo: Option<&str>, since_ms: i64) -> u64`.
- Before claim: check both; if exceeded, stand down and emit webhook.
- Anthropic: unknown model → `bail!("unknown Anthropic model ...; available: ...")`.
- Tests: cap blocks claim; limit resets by window; unknown model errors.

### Task F2: PR 上下文/风险画像/draft

**Files:**
- Modify: `crates/nuwa/src/deliver.rs`
- Modify: `crates/nuwa/src/instructions.rs` (`draft_first_n`)
- Modify: `crates/nuwa/src/main.rs` (public base URL, merged count)
- Modify: `crates/shanhaijing/src/lib.rs` (`merged_pr_count`, `first_pr_for_repo`)
- Test: inline tests

**Interfaces:**
- `NUWA_PUBLIC_BASE_URL` setting.
- PR body: link `[/issues/<fp>](<base>/issues/<fp>)`; risk profile block; prior attempts; verification evidence.
- `create_pr_via_api(..., draft: bool)`.
- `draft = draft_first_n > 0 && merged_pr_count(repo) < draft_first_n`.
- Tests: draft flag; body contains link/risk; no local path.

### Task F3: 交付质量指标

**Files:**
- Modify: `crates/nuwa/src/api/trends.rs`
- Modify: `crates/nuwa/src/api/overview.rs`
- Modify: `web/src/features/overview/*`
- Test: Rust API tests + web tests

**Interfaces:**
- Trends response adds `delivery: { false_positive_rate, review_latency_p50_ms, ci_red_rate }` per window.
- Compute from issues/events/runs.
- Overview card "交付质量".
- Tests: seeded data produces expected rates; empty data nulls.

### Task F4: 保留策略

**Files:**
- Modify: `crates/shanhaijing/src/lib.rs` (session dirs? maybe `Store::expired_session_dirs`)
- Modify: `crates/nuwa/src/serve.rs`
- Test: `serve.rs`

**Interfaces:**
- `cleanup_sessions(sessions_root, store, now, run_days)`:
  - delete `sessions/<fp>` dirs whose latest run is older than run_days;
  - ignore paths outside root; remove empty dirs.
- Call in daily prune.
- Tests: old dir removed, recent kept, traversal rejected.

### Task F5: SDK 超时/重试/fallback

**Files:**
- Modify: `sdk/qingniao-go/qingniao.go`
- Modify: `sdk/qingniao-go/qingniao_test.go`

**Interfaces:**
- `HTTPSink` gains `Timeout time.Duration`, `MaxAttempts int`, `FallbackFile string`.
- Defaults: Timeout 3s, MaxAttempts 2.
- Retry on network/5xx; append to FallbackFile on final failure.
- `Capture` unchanged (Send is bounded).
- Tests: retry then success; fallback written; timeout doesn't hang; no data race.

### Task F6: `nuwa check` + 非 Go 接入文档

**Files:**
- Modify: `crates/nuwa/src/main.rs` (`check` command)
- Create maybe `crates/nuwa/src/check.rs`
- Modify: `docs/08-onboarding.md`
- Test: `main.rs`/`check.rs`

**Interfaces:**
- `nuwa check --instructions <path> [--repo <path>] [--json]`
- Validate config parse, repo identity vs git remote slug, glob compile, commands non-empty, max_attempts > 0.
- If repo provided: check git repo, base branch, fetch? Do not mutate; run `failing_check` in a temp worktree at `origin/<base>` and report red/green.
- Exit code 0/1; JSON summary.
- Docs: raw envelope example, non-Go collector.

### Task F7: 文档/ADR 总同步

**Files:**
- Modify: `README.md`, `docs/01`, `docs/03`, `docs/04`, `docs/07`, `docs/08`
- Add ADR-010 (webhook), ADR-011 (verification protocol), amend ADR-003.

---

## 最终验证

- [ ] `taskkill /F /IM nuwa.exe` (ignore if not running)
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `cargo test --workspace` (expect known Windows failure in rpi-tools; document)
- [ ] `cd sdk/qingniao-go && go test ./...`
- [ ] `cd web && npm ci && npm run typecheck && npm run lint && npm test && npm run build`
- [ ] Manual smoke: `nuwa check` on `spike/target-repo/nuwa.repo.json`; `nuwa work --provider faux` against a temp DB; verify branch/base recovery and evidence.
- [ ] Update `docs/07-open-items.md` with final snapshot and test counts.
- [ ] Report to user; wait for confirmation before commit/push.
