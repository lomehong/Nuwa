# Web 控制台 M1b Runner 遥测接入 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 M1a 的 `runners`/`runs`/`signal_daily` Store API 接到 `nuwa work`：注册 runner、后台心跳、run 生命周期与实时 turns/tokens，让控制台的"机器在干什么/是否卡单"有真实数据。

**Architecture:** `work` 启动时注册 runner 并派生心跳任务；`work_once` 在领单时创建 run，按修复/验证/交付/CI 阶段更新，所有遥测写入 best-effort，不阻断修复主链。修复与护栏行为不变。

**Tech Stack:** Rust 2021、tokio、uuid、现有 rpi harness/faux provider、临时 git 仓库测试。

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md) §8、§14 M1。

## Global Constraints

- 自建 crate 零 clippy warning；每个任务先失败测试再实现。
- 遥测写入（runner/run/事件）一律 best-effort：失败只写 stderr，不阻断 repair/deliver。
- 修复、交付、护栏逻辑不变；`guard.rs`/`deliver.rs` 的验证门与分支规则不动。
- 不碰 `crates/rpi-*`。
- 本机构建前 `taskkill //F //IM nuwa.exe 2>/dev/null || true`。
- 测试用临时目录/临时 DB；不依赖 `spike/target-repo` 或本机 `shanhaijing.db`。
- 每个任务一个 commit。

---

### Task 1: Repair 进度回调与 session_dir

**Files:**
- Modify: `crates/nuwa/src/repair.rs`
- Modify: `crates/nuwa/src/main.rs`（`run_fix` 与 `work_once` 的 `RepairJob` 构造）
- Test: `crates/nuwa/src/repair.rs` 新增 tests 模块

**Interfaces:**
- Produces:
  - `pub struct RepairProgress { pub turns: u32, pub uncached_tokens: u64, pub cached_tokens: u64 }`
  - `RepairJob.progress: Option<Arc<dyn Fn(RepairProgress) + Send + Sync>>`
  - `RepairReport.session_dir: String`
- Consumes: `AgentEvent::TurnEnd` 的 assistant usage（`input + cache_write` 为 uncached，`cache_read` 为 cached）。

- [ ] **Step 1: 写失败测试**

在 `repair.rs` 末尾新增：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn issue() -> Issue {
        Issue {
            fingerprint: "fp".into(),
            title: "panic in Average".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic: boom".into(),
            log_window: Vec::new(),
            failing_check: crate::issue::FailingCheck {
                command: "echo ok".into(),
                target: None,
            },
            forbidden_paths: Vec::new(),
        }
    }

    #[tokio::test]
    async fn faux_run_reports_progress_and_session_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let source = repo.join("calc.go");
        std::fs::write(
            &source,
            "package main\n\nfunc Average(xs []float64) float64 {\n\
             \tvar sum float64\n\
             \tfor i := 0; i <= len(xs); i++ {\n\
             \t\tsum += xs[i]\n\
             \t}\n\
             \treturn sum / float64(len(xs))\n\
             }\n",
        )
        .expect("write");

        let seen = Arc::new(Mutex::new(Vec::<RepairProgress>::new()));
        let seen_cb = Arc::clone(&seen);
        let job = RepairJob {
            repo: repo.clone(),
            issue: issue(),
            sessions_dir: dir.path().join("sessions"),
            provider: ProviderKind::Faux,
            max_turns: 5,
            guards: Arc::new(Guards::new(Vec::new())),
            notes: String::new(),
            progress: Some(Arc::new(move |p| {
                seen_cb.lock().expect("lock").push(p);
            })),
        };

        let report = run(&job).await.expect("run");
        assert!(!report.session_dir.is_empty());
        let events = seen.lock().expect("lock");
        assert!(!events.is_empty(), "progress callback must fire at least once");
        assert!(events.last().expect("last").turns >= 1);

        let fixed = std::fs::read_to_string(&source).expect("read");
        assert!(fixed.contains("i < len(xs)"), "faux edit must land");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa repair::tests::faux_run_reports_progress_and_session_dir -- --nocapture`

Expected: 编译失败，`RepairJob` 没有 `progress` 字段 / `RepairReport` 没有 `session_dir`。

- [ ] **Step 3: 实现进度与字段**

在 `repair.rs` 中：

```rust
pub struct RepairJob {
    pub repo: PathBuf,
    pub issue: Issue,
    pub sessions_dir: PathBuf,
    pub provider: ProviderKind,
    pub max_turns: u32,
    pub guards: Arc<Guards>,
    pub notes: String,
    pub progress: Option<Arc<dyn Fn(RepairProgress) + Send + Sync>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RepairProgress {
    pub turns: u32,
    pub uncached_tokens: u64,
    pub cached_tokens: u64,
}

pub struct RepairReport {
    pub run_id: String,
    pub root_cause_text: String,
    pub turns: u32,
    pub uncached_tokens: u64,
    pub cached_tokens: u64,
    pub session_dir: String,
}
```

在 `run` 开头：

```rust
    let progress = job.progress.clone();
    let uncached = Arc::new(AtomicU64::new(0));
    let cached = Arc::new(AtomicU64::new(0));
```

把 `use std::sync::atomic::{AtomicU32, Ordering};` 改为：

```rust
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
```

在事件 drain 的 `AgentEvent::TurnEnd` 分支，把原来的：

```rust
                    AgentEvent::TurnEnd { ref message, .. } => {
                        eprintln!("  -- turn end --");
                        turn_counter.fetch_add(1, Ordering::Relaxed);
                        if let rpi_agent::AgentMessage::Assistant(assistant) = message {
                            let usage = assistant.usage.clone();
                            let _ = drain_harness.record_usage(usage, None, None).await;
                        }
                    }
```

改为：

```rust
                    AgentEvent::TurnEnd { ref message, .. } => {
                        eprintln!("  -- turn end --");
                        let turns = turn_counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if let rpi_agent::AgentMessage::Assistant(assistant) = message {
                            let usage = assistant.usage.clone();
                            let uncached_total = uncached.fetch_add(
                                (usage.input + usage.cache_write).max(0) as u64,
                                Ordering::Relaxed,
                            ) + (usage.input + usage.cache_write).max(0) as u64;
                            let cached_total = cached.fetch_add(
                                usage.cache_read.max(0) as u64,
                                Ordering::Relaxed,
                            ) + usage.cache_read.max(0) as u64;
                            let _ = drain_harness.record_usage(usage, None, None).await;
                            if let Some(cb) = &progress {
                                cb(RepairProgress {
                                    turns,
                                    uncached_tokens: uncached_total,
                                    cached_tokens: cached_total,
                                });
                            }
                        }
                    }
```

在 drain 的 `move` 闭包前，`let turns = Arc::clone(&turns);` 已经存在；`uncached`/`cached` 直接 move 进闭包（它们是 Arc）。确保 `progress` 在闭包中可用。

在构造 `RepairReport` 时加入：

```rust
        session_dir: sessions_dir.to_string_lossy().into_owned(),
```

在 `main.rs` 的 `run_fix` 与 `work_once` 的 `RepairJob { ... }` 构造中先加：

```rust
        progress: None,
```

（Task 4 再把 `work_once` 改为 `Some(...)`。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p nuwa repair::tests::faux_run_reports_progress_and_session_dir -- --nocapture`

Expected: PASS；`calc.go` 被 faux 工具真实修改。

- [ ] **Step 5: 提交**

```bash
git add crates/nuwa/src/repair.rs crates/nuwa/src/main.rs
git commit -m "feat(repair): report live turn/token progress and session dir"
```

---

### Task 2: Deliver 证据字段

**Files:**
- Modify: `crates/nuwa/src/deliver.rs`
- Test: `crates/nuwa/src/deliver.rs` tests

**Interfaces:**
- Produces: `DeliverReport.verification_detail: String`、`DeliverReport.change_stat: String`。
- Consumes: 现有验证门 `check.output`、`git diff --stat`。

- [ ] **Step 1: 写失败测试**

在 `deliver.rs` tests 模块加入：

```rust
#[test]
fn evidence_truncation_keeps_bounded_text() {
    let long = "x".repeat(20_000);
    let out = truncate_evidence(&long, 100);
    assert!(out.ends_with('…'));
    assert_eq!(out.chars().count(), 101);
    assert_eq!(truncate_evidence("short", 100), "short");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa deliver::tests::evidence_truncation -- --nocapture`

Expected: 编译失败，`truncate_evidence` 不存在。

- [ ] **Step 3: 实现字段与截断**

把现有 `root_cause_excerpt` 保留，并新增：

```rust
fn truncate_evidence(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max_chars).collect();
    format!("{cut}…")
}
```

`DeliverReport` 增加：

```rust
    pub verification_detail: String,
    pub change_stat: String,
```

在验证门通过、写 evidence 之后：

```rust
    let verification_detail = truncate_evidence(
        &format!("$ {}\n\n{}", issue.failing_check.command, check.output),
        16_000,
    );
```

在 `stat` 计算之后：

```rust
    let change_stat = truncate_evidence(&stat, 4_000);
```

返回 `DeliverReport` 时加上：

```rust
        verification_detail,
        change_stat,
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p nuwa deliver::tests::evidence_truncation -- --nocapture`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/nuwa/src/deliver.rs
git commit -m "feat(deliver): expose bounded verification evidence and diff stat"
```

---

### Task 3: work 的 runner 身份与后台心跳

**Files:**
- Modify: `crates/nuwa/src/main.rs`
- Modify: `crates/nuwa/Cargo.toml`（`uuid`）
- Test: `crates/nuwa/src/main.rs` tests

**Interfaces:**
- Produces:
  - `fn sanitize_runner_id(raw: &str) -> String`
  - `fn default_runner_id(repo: &str) -> String`
  - `fn provider_meta(provider: &repair::ProviderKind) -> (String, String)`
  - `struct RunnerLive` / `type SharedLive`
  - `fn spawn_heartbeat(...) -> tokio::task::JoinHandle<()>`
  - `async fn live_update(...)` / `async fn beat_now(...)`
- Consumes: M1a `upsert_runner`/`heartbeat_runner`/`mark_interrupted`、旧 `heartbeat`（回滚双写）。

- [ ] **Step 1: 加 uuid 依赖**

在 `crates/nuwa/Cargo.toml` 的 `[dependencies]` 增加：

```toml
uuid = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

在 `main.rs` tests 模块加入：

```rust
#[test]
fn runner_id_is_sanitized_and_stable() {
    let id = sanitize_runner_id("host/repo name:日本");
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')));
    assert!(id.len() <= 128);
    assert!(!id.is_empty());
    assert_eq!(sanitize_runner_id("---"), "runner");
}

#[test]
fn default_runner_id_contains_repo() {
    let id = default_runner_id("example.com/spike-target");
    assert!(id.contains("example.com") || id.contains("spike-target"));
}

#[test]
fn provider_labels_are_stable() {
    assert_eq!(provider_meta(&repair::ProviderKind::Faux), ("faux".into(), "faux".into()));
    assert_eq!(
        provider_meta(&repair::ProviderKind::Zai { base_url: None, model: None }),
        ("zai".into(), "glm-5.3-flash".into())
    );
    assert_eq!(
        provider_meta(&repair::ProviderKind::Anthropic { model: None }),
        ("anthropic".into(), "<default>".into())
    );
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p nuwa tests::runner_id tests::provider_labels -- --nocapture`

Expected: 编译失败，函数不存在。

- [ ] **Step 4: 实现身份/心跳辅助**

在 `main.rs` 的 `work_once` 之前加入：

```rust
#[derive(Debug)]
struct RunnerLive {
    status: String,
    current_fingerprint: Option<String>,
    phase: Option<String>,
    last_error: Option<String>,
}

impl RunnerLive {
    fn new() -> Self {
        Self {
            status: "starting".into(),
            current_fingerprint: None,
            phase: None,
            last_error: None,
        }
    }
}

type SharedLive = Arc<tokio::sync::Mutex<RunnerLive>>;

fn sanitize_runner_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let value = if trimmed.is_empty() { "runner" } else { trimmed };
    value.chars().take(128).collect()
}

fn default_runner_id(repo: &str) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "host".to_string());
    sanitize_runner_id(&format!("{host}:{repo}"))
}

fn provider_meta(provider: &repair::ProviderKind) -> (String, String) {
    match provider {
        repair::ProviderKind::Faux => ("faux".into(), "faux".into()),
        repair::ProviderKind::Anthropic { model } => (
            "anthropic".into(),
            model.clone().unwrap_or_else(|| "<default>".into()),
        ),
        repair::ProviderKind::Zai { model, .. } => (
            "zai".into(),
            model.clone().unwrap_or_else(|| "glm-5.3-flash".into()),
        ),
    }
}

fn effective_config_json(
    repo_config: &instructions::RepoInstructions,
    interval_secs: u64,
    ci_timeout_secs: u64,
    max_turns: u32,
    max_tokens: u64,
) -> String {
    serde_json::json!({
        "interval_secs": interval_secs,
        "ci_timeout_secs": ci_timeout_secs,
        "max_turns": max_turns,
        "max_tokens": max_tokens,
        "failing_check": repo_config.failing_check.command,
        "forbidden_paths_count": repo_config.forbidden_paths.len(),
    })
    .to_string()
}

async fn live_snapshot(live: &SharedLive) -> (String, Option<String>, Option<String>, Option<String>) {
    let state = live.lock().await;
    (
        state.status.clone(),
        state.current_fingerprint.clone(),
        state.phase.clone(),
        state.last_error.clone(),
    )
}

async fn beat_now(store: &Arc<shanhaijing::Store>, runner_id: &str, live: &SharedLive) {
    let (status, fp, phase, error) = live_snapshot(live).await;
    let _ = store.heartbeat_runner(
        runner_id,
        &shanhaijing::RunnerHeartbeat {
            heartbeat_at: now_ms(),
            status,
            current_fingerprint: fp,
            phase,
            config_json: None,
            last_error: error,
        },
    );
    // v0.2.x 回滚保护：旧控制台仍读全局 heartbeat 表。
    let _ = store.heartbeat(now_ms());
}

async fn live_update(
    store: &Arc<shanhaijing::Store>,
    runner_id: &str,
    live: &SharedLive,
    status: &str,
    fp: Option<&str>,
    phase: Option<&str>,
    error: Option<String>,
) {
    {
        let mut state = live.lock().await;
        state.status = status.to_string();
        state.current_fingerprint = fp.map(str::to_string);
        state.phase = phase.map(str::to_string);
        state.last_error = error;
    }
    beat_now(store, runner_id, live).await;
}

fn spawn_heartbeat(
    store: Arc<shanhaijing::Store>,
    runner_id: String,
    interval: std::time::Duration,
    live: SharedLive,
    config_json: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (status, fp, phase, error) = live_snapshot(&live).await;
            let _ = store.heartbeat_runner(
                &runner_id,
                &shanhaijing::RunnerHeartbeat {
                    heartbeat_at: now_ms(),
                    status,
                    current_fingerprint: fp,
                    phase,
                    config_json: Some(config_json.clone()),
                    last_error: error,
                },
            );
            let _ = store.heartbeat(now_ms());
            tokio::time::sleep(interval).await;
        }
    })
}
```

- [ ] **Step 5: 修改 run_work 启动**

在 `run_work` 的参数解析中加入：

```rust
    let mut runner_id_given: Option<String> = std::env::var("NUWA_RUNNER_ID").ok().filter(|v| !v.is_empty());
    let mut service_unit = std::env::var("NUWA_SERVICE_UNIT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "nuwa-work".to_string());
    let mut event_retention_days: u32 = env_d("NUWA_EVENT_RETENTION_DAYS", "90")
        .parse()
        .unwrap_or(90);
```

在 flag 匹配中增加：

```rust
            "--runner-id" => runner_id_given = Some(value("--runner-id")?),
            "--service-unit" => service_unit = value("--service-unit")?,
```

把 store 创建改为 Arc：

```rust
    let store = Arc::new(
        shanhaijing::Store::open(&db_path)
            .with_context(|| format!("open db {}", db_path.display()))?,
    );
    let (repo_config, repo_dir) = instructions::RepoInstructions::load(&instructions_path)?;

    let runner_id = runner_id_given
        .map(|id| sanitize_runner_id(&id))
        .unwrap_or_else(|| default_runner_id(&repo_config.repo));
    let (provider_name, model_name) = provider_meta(&provider);
    let config_json = effective_config_json(
        &repo_config,
        interval_secs,
        ci_timeout_secs,
        max_turns,
        max_tokens,
    );
    let now = now_ms();
    let _ = store.mark_interrupted(&runner_id, now);
    let _ = store.prune(now, event_retention_days, 365);
    if let Err(e) = store.upsert_runner(&shanhaijing::RunnerUpsert {
        id: runner_id.clone(),
        repo: repo_config.repo.clone(),
        host: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "host".to_string()),
        pid: Some(std::process::id() as i64),
        started_at: now,
        heartbeat_at: now,
        status: "starting".into(),
        provider: Some(provider_name),
        model: Some(model_name),
        daily_pr_limit: Some(daily_pr_limit as i64),
        repo_path: Some(repo_dir.display().to_string()),
        service_unit: Some(service_unit),
        config_json: config_json.clone(),
    }) {
        eprintln!("runner register failed (telemetry only): {e}");
    }
    let live = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
    let heartbeat_interval = std::time::Duration::from_secs(interval_secs.clamp(5, 15));
    let heartbeat = spawn_heartbeat(
        Arc::clone(&store),
        runner_id.clone(),
        heartbeat_interval,
        Arc::clone(&live),
        config_json,
    );
```

把原 `loop { ... }` 替换为：

```rust
    let result: anyhow::Result<()> = async {
        loop {
            let worked = work_once(
                &store,
                &repo_config,
                &repo_dir,
                sessions_dir.clone(),
                provider.clone(),
                max_turns,
                push,
                create_pr,
                daily_pr_limit,
                max_tokens,
                ci_timeout_secs,
                &runner_id,
                Arc::clone(&live),
            )
            .await?;
            if !loop_run {
                break;
            }
            if !worked {
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            }
        }
        Ok(())
    }
    .await;
    heartbeat.abort();
    result
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p nuwa tests::runner_id tests::provider_labels -- --nocapture`

Expected: 3 个测试 PASS。

- [ ] **Step 7: 提交**

```bash
git add crates/nuwa/src/main.rs crates/nuwa/Cargo.toml Cargo.lock
git commit -m "feat(work): register runner identity and heartbeat in background"
```

---

### Task 4: run 生命周期接入 work_once

**Files:**
- Modify: `crates/nuwa/src/main.rs`
- Test: `crates/nuwa/src/main.rs` tests（faux 端到端）

**Interfaces:**
- Consumes: `RunStart`/`RunUpdate`/`RunFinish`、`RepairProgress`、`DeliverReport` 新字段、`live_update`/`beat_now`。
- Produces: 每次领单产生一条 `runs` 记录；阶段/结果/tokens/证据落库。

- [ ] **Step 1: 写失败测试（faux 全链路）**

在 `main.rs` tests 模块加入：

```rust
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git");
    assert!(status.success(), "git {:?} failed", args);
}

fn git_output(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {:?} failed", args);
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[tokio::test]
async fn faux_run_writes_run_telemetry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    std::fs::write(
        repo.join("calc.go"),
        "package main\n\nfunc Average(xs []float64) float64 {\n\
         \tvar sum float64\n\
         \tfor i := 0; i <= len(xs); i++ {\n\
         \t\tsum += xs[i]\n\
         \t}\n\
         \treturn sum / float64(len(xs))\n\
         }\n",
    )
    .expect("write");
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["add", "."]);
    git(
        &repo,
        &["-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-m", "init"],
    );

    let store = Arc::new(
        shanhaijing::Store::open(dir.path().join("s.db")).expect("open"),
    );
    let now = now_ms();
    let signal = shanhaijing::Signal {
        repo: "example.com/spike".into(),
        language: "go".into(),
        title: "panic in Average".into(),
        severity: "high".into(),
        message: None,
        stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.Average()\n\t/app/calc.go:17 +0x1".into(),
    };
    store.report_signal(&signal, now).expect("report");

    let cfg = RepoInstructions {
        repo: "example.com/spike".into(),
        failing_check: FailingCheck { command: "echo ok".into(), target: None },
        forbidden_paths: Vec::new(),
        notes: String::new(),
    };
    let live: SharedLive = Arc::new(tokio::sync::Mutex::new(RunnerLive::new()));
    let worked = work_once(
        &store,
        &cfg,
        &repo,
        dir.path().join("sessions"),
        repair::ProviderKind::Faux,
        5,
        false,
        false,
        5,
        1_000_000,
        600,
        "test:runner",
        Arc::clone(&live),
    )
    .await
    .expect("work");
    assert!(worked);

    let issue = store.list_issues().expect("issues").pop().expect("issue");
    let run = store.latest_run(&issue.fingerprint).expect("latest").expect("run");
    assert_eq!(run.phase, "done");
    assert_eq!(run.outcome.as_deref(), Some("success"));
    assert!(run.turns >= 1, "faux run must record at least one turn");
    assert!(run.session_dir.is_some());
    assert!(run.verification_ok == Some(true));
    assert!(run.branch.as_deref().unwrap_or("").starts_with("agent/fix/"));
    assert_eq!(run.runner_id, "test:runner");
    // 交付后工作树已切回 main；修复内容在 agent 分支上取证。
    let branch = run.branch.clone().expect("branch");
    let fixed = git_output(&repo, &["show", &format!("{branch}:calc.go")]);
    assert!(fixed.contains("i < len(xs)"));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa tests::faux_run_writes_run_telemetry -- --nocapture`

Expected: 编译失败（`work_once` 参数数量不符）；或 run 表为空。

- [ ] **Step 3: 修改 work_once 签名与顶部**

签名末尾增加：

```rust
    runner_id: &str,
    live: SharedLive,
```

在 `let now = now_ms();` 之后、配额检查之前保留 M0 的 `store.heartbeat(now)?;`，并把配额分支改为：

```rust
        if opened >= daily_pr_limit {
            println!(
                "quota      : {opened}/{daily_pr_limit} PRs today for {} — standing down until the window rolls",
                repo_config.repo
            );
            live_update(store, runner_id, &live, "standing_down", None, None, None).await;
            return Ok(false);
        }
```

- [ ] **Step 4: 预检失败上报**

把 HEAD 检查改为：

```rust
    let head = match deliver::git(repo_dir, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(head) => head,
        Err(e) => {
            live_update(store, runner_id, &live, "error", None, None, Some(format!("{e:#}"))).await;
            return Err(e);
        }
    };
    if head != "main" && head != "master" {
        let message = format!(
            "repo {} is on branch `{}` — runner only works from main/master; reset the checkout",
            repo_dir.display(),
            head
        );
        live_update(store, runner_id, &live, "error", None, None, Some(message.clone())).await;
        bail!("{message}");
    }
    let dirty = match deliver::git(
        repo_dir,
        &["status", "--porcelain", "--", ":(exclude)nuwa.repo.json"],
    ) {
        Ok(dirty) => dirty,
        Err(e) => {
            live_update(store, runner_id, &live, "error", None, None, Some(format!("{e:#}"))).await;
            return Err(e);
        }
    };
    if !dirty.trim().is_empty() {
        let message = format!(
            "repo {} has an unclean working tree — reset before running:\n{}",
            repo_dir.display(),
            dirty.trim()
        );
        live_update(store, runner_id, &live, "error", None, None, Some(message.clone())).await;
        bail!("{message}");
    }
```

- [ ] **Step 5: 领单与 run 创建**

把 `let Some(snapshot) = store.claim_ready_issue(...) else { ... }` 的 else 分支改为：

```rust
    else {
        println!("nothing ready for pickup");
        live_update(store, runner_id, &live, "idle", None, None, None).await;
        return Ok(false);
    };
```

在 repo mismatch 分支前加入：

```rust
    let run_id = uuid::Uuid::new_v4().to_string();
    let (provider_name, model_name) = provider_meta(&provider);
```

repo mismatch 分支改为：

```rust
    if snapshot.repo != repo_config.repo {
        let message = format!(
            "repo mismatch: issue `{}` belongs to `{}`, instructions are for `{}` — refusing (ADR-004)",
            snapshot.fingerprint,
            snapshot.repo,
            repo_config.repo
        );
        live_update(store, runner_id, &live, "error", Some(&snapshot.fingerprint), None, Some(message.clone())).await;
        bail!("{message}");
    }
```

在 `let fp = snapshot.fingerprint.clone();` 之后加入：

```rust
    let _ = store.start_run(&shanhaijing::RunStart {
        id: run_id.clone(),
        fingerprint: fp.clone(),
        runner_id: runner_id.to_string(),
        repo: snapshot.repo.clone(),
        started_at: now,
        provider: Some(provider_name.clone()),
        model: Some(model_name.clone()),
    });
    live_update(store, runner_id, &live, "working", Some(&fp), Some("claim"), None).await;
```

删掉原来 `claim_ready_issue` 分支外重复的 `store.heartbeat(now)?;`（如果 M0 已把心跳前移，这里不再需要）。

- [ ] **Step 6: 修复阶段与进度回调**

在 `store.set_state(&fp, Fixing, ...)` 之前加入进度回调，并把 `RepairJob` 的 `progress` 改为 `Some(...)`：

```rust
    let progress_store = Arc::clone(store);
    let progress_run = run_id.clone();
    let progress: Option<Arc<dyn Fn(repair::RepairProgress) + Send + Sync>> =
        Some(Arc::new(move |p| {
            let _ = progress_store.update_run(
                &progress_run,
                &shanhaijing::RunUpdate {
                    turns: Some(i64::from(p.turns)),
                    tokens_uncached: Some(p.uncached_tokens as i64),
                    tokens_cached: Some(p.cached_tokens as i64),
                    ..Default::default()
                },
            );
        }));
```

`RepairJob` 构造中的 `progress: None` 改为 `progress`。在调用 `repair::run` 前：

```rust
    live_update(store, runner_id, &live, "working", Some(&fp), Some("repair"), None).await;
```

修复失败分支改为：

```rust
    let report = match repair::run(&job).await {
        Ok(r) => r,
        Err(e) => {
            let detail = format!("repair_failed: {e:#}");
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "repair_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(store, runner_id, &live, "error", Some(&fp), None, Some(detail.clone())).await;
            store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
            bail!("repair failed — issue escalated: {e:#}");
        }
    };
```

修复成功后，在 budget gate 之前落库：

```rust
    let _ = store.update_run(
        &run_id,
        &shanhaijing::RunUpdate {
            phase: Some("verify".into()),
            harness_run_id: Some(report.run_id.clone()),
            turns: Some(i64::from(report.turns)),
            tokens_uncached: Some(report.uncached_tokens as i64),
            tokens_cached: Some(report.cached_tokens as i64),
            root_cause: Some(report.root_cause_text.clone()),
            session_dir: Some(report.session_dir.clone()),
            ..Default::default()
        },
    );
```

- [ ] **Step 7: 预算/交付/CI 的 run 更新**

预算超限分支改为：

```rust
    if spent > max_tokens {
        let detail = format!("budget_exceeded: {spent} > {max_tokens}");
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "budget_exceeded".into(),
                ci_verdict: None,
                error: Some(detail.clone()),
            },
        );
        live_update(store, runner_id, &live, "error", Some(&fp), None, Some(detail.clone())).await;
        store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
        bail!("{detail} — issue escalated");
    }
```

交付前：

```rust
    live_update(store, runner_id, &live, "working", Some(&fp), Some("deliver"), None).await;
```

交付失败分支改为：

```rust
        Err(e) => {
            let detail = format!("delivery_failed: {e:#}");
            let _ = store.finish_run(
                &run_id,
                &shanhaijing::RunFinish {
                    finished_at: now_ms(),
                    outcome: "delivery_failed".into(),
                    ci_verdict: None,
                    error: Some(detail.clone()),
                },
            );
            live_update(store, runner_id, &live, "error", Some(&fp), None, Some(detail.clone())).await;
            store.set_state(&fp, shanhaijing::State::Escalated, now_ms(), Some(detail.as_str()))?;
            bail!("delivery failed — issue escalated: {e:#}");
        }
```

交付成功后，在 `set_state(PrOpen)` 附近更新 run：

```rust
    let pr_opened_at = delivered.pr_url.as_ref().map(|_| now_ms());
    let _ = store.update_run(
        &run_id,
        &shanhaijing::RunUpdate {
            phase: Some("ci".into()),
            branch: Some(delivered.branch.clone()),
            commit_sha: Some(delivered.commit.clone()),
            pr_url: delivered.pr_url.clone(),
            pr_opened_at,
            verification_ok: Some(true),
            verification_detail: Some(delivered.verification_detail.clone()),
            change_stat: Some(delivered.change_stat.clone()),
            session_dir: Some(report.session_dir.clone()),
            ..Default::default()
        },
    );
```

在 `if create_pr {` 之前先判断不建 PR 的路径：

```rust
    if !create_pr {
        let _ = store.finish_run(
            &run_id,
            &shanhaijing::RunFinish {
                finished_at: now_ms(),
                outcome: "success".into(),
                ci_verdict: None,
                error: None,
            },
        );
        live_update(store, runner_id, &live, "idle", None, None, None).await;
        println!(
            "delivered  : branch {} commit {} pr {}",
            delivered.branch,
            &delivered.commit[..12.min(delivered.commit.len())],
            delivered.pr_url.as_deref().unwrap_or("-"),
        );
        return Ok(true);
    }
```

在 CI 分支前：

```rust
    live_update(store, runner_id, &live, "working", Some(&fp), Some("ci"), None).await;
```

CI Success 分支加入：

```rust
                    let _ = store.update_run(
                        &run_id,
                        &shanhaijing::RunUpdate {
                            ci_verdict: Some("success".into()),
                            ..Default::default()
                        },
                    );
                    let _ = store.finish_run(
                        &run_id,
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "success".into(),
                            ci_verdict: Some("success".into()),
                            error: None,
                        },
                    );
```

CI Failure 分支：在 `close_pr` 成功后加入：

```rust
                    let _ = store.update_run(
                        &run_id,
                        &shanhaijing::RunUpdate {
                            ci_verdict: Some("failure".into()),
                            ..Default::default()
                        },
                    );
                    let _ = store.finish_run(
                        &run_id,
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "ci_red".into(),
                            ci_verdict: Some("failure".into()),
                            error: Some(format!("CI red — PR #{number} closed")),
                        },
                    );
```

在原有 `bail!("CI red — PR #{number} closed ...")` 之前加入：

```rust
                    live_update(
                        store,
                        runner_id,
                        &live,
                        "error",
                        Some(&fp),
                        None,
                        Some(format!("CI red — PR #{number} closed")),
                    )
                    .await;
```

若 `close_pr` 失败（`?` 提前返回），在调用前改为：

```rust
                    if let Err(e) = ci::close_pr(&token, slug, number).await {
                        let detail = format!("ci_red: close PR failed: {e:#}");
                        let _ = store.finish_run(
                            &run_id,
                            &shanhaijing::RunFinish {
                                finished_at: now_ms(),
                                outcome: "ci_red".into(),
                                ci_verdict: Some("failure".into()),
                                error: Some(detail.clone()),
                            },
                        );
                        live_update(store, runner_id, &live, "error", Some(&fp), None, Some(detail)).await;
                        return Err(e);
                    }
```

CI Timeout 分支加入：

```rust
                    let _ = store.update_run(
                        &run_id,
                        &shanhaijing::RunUpdate {
                            ci_verdict: Some("timeout".into()),
                            ..Default::default()
                        },
                    );
                    let _ = store.finish_run(
                        &run_id,
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "success".into(),
                            ci_verdict: Some("timeout".into()),
                            error: None,
                        },
                    );
```

CI 不可读分支加入：

```rust
                    let _ = store.update_run(
                        &run_id,
                        &shanhaijing::RunUpdate {
                            ci_verdict: Some("unavailable".into()),
                            ..Default::default()
                        },
                    );
                    let _ = store.finish_run(
                        &run_id,
                        &shanhaijing::RunFinish {
                            finished_at: now_ms(),
                            outcome: "success".into(),
                            ci_verdict: Some("unavailable".into()),
                            error: None,
                        },
                    );
```

函数末尾在打印投递结果前：

```rust
    live_update(store, runner_id, &live, "idle", None, None, None).await;
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test -p nuwa tests::faux_run_writes_run_telemetry -- --nocapture`

Expected: PASS。若失败，先检查 `calc.go` 的 oldText 是否与 faux 脚本完全一致。

- [ ] **Step 9: 提交**

```bash
git add crates/nuwa/src/main.rs
git commit -m "feat(work): persist run lifecycle, progress and evidence telemetry"
```

---

### Task 5: 验证、文档与 M1 完成标志

**Files:**
- Modify: `docs/07-open-items.md`（§D）

- [ ] **Step 1: 全量检查**

```bash
taskkill //F //IM nuwa.exe 2>/dev/null || true
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p shanhaijing -p nuwa
```

Expected: clippy 零 warning；测试全绿。

- [ ] **Step 2: 单独重跑 faux 遥测测试**

```bash
cargo test -p nuwa tests::faux_run_writes_run_telemetry -- --nocapture
```

Expected: PASS；测试已用临时 git 仓库断言 run 阶段、turns、session_dir、验证证据、agent 分支内容。不需要手工 CLI 步骤。

- [ ] **Step 3: 更新进度快照**

在 `docs/07-open-items.md` §D "下一步"之前追加（替换实际测试数）：

```markdown
- **Web 控制台 M1 数据与遥测（2026-10-17）**：`PRAGMA user_version` v1 迁移 + WAL；新增 `runners`/`runs`/`signal_daily`；events 增加 repo/actor；上报按日聚合、停止 dedup 事件刷屏；runner 身份 + 后台心跳 + run 生命周期/实时 turns/tokens/证据；卡单判定与 requeue/reopen Store API；配额改为按 PR URL 去重。自建 crate clippy 零 warning、测试全绿（新增 N 个）。
```

- [ ] **Step 4: 提交文档**

```bash
git add docs/07-open-items.md
git commit -m "docs: record M1 telemetry foundation and verification"
```

---

## 自审记录

- 规格覆盖：§8.1 身份、§8.2 心跳、§8.3 run 生命周期/进度/证据、§8.6 配额待命、§8.7 多仓库所需遥测、§7 数据表均有任务。
- 占位符：无 TBD；所有 helper 与测试均为可编译片段。
- 类型一致性：`RepairProgress { turns: u32, uncached_tokens: u64, cached_tokens: u64 }`；`RunUpdate` 数值字段为 `i64`，Task 4 显式转换；`work_once` 新参数顺序与 Task 3 的调用一致（runner_id, live 在最后）。
- 已知边界：faux 运行只证明遥测接线与流水线，不证明真实 LLM 修复质量；真实 runner 配置由 M2 API 展示。
