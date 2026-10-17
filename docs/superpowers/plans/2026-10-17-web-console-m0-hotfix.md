# Web 控制台 M0 线上热修 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不引入 SPA 的前提下，先修掉 v2 控制台的线上 P0/P1 缺陷：反射型 XSS、rejected 无法 reopen、reopen 不清冷却、设置页 env 键大小写错误、配额待命不写心跳、旧 POST 无同源校验。

**Architecture:** 只改自建 crate `nuwa` / `shanhaijing`，不碰 vendored `rpi-*`；不新增数据库表，不改变修复流水线。每个任务先写失败测试，再最小实现，单独提交。

**Tech Stack:** Rust 2021、axum 0.8、rusqlite、tokio、现有 `nuwa`/`shanhaijing` 测试设施。

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md) §14 M0；缺陷背景见 §4。

## Global Constraints

- 自建 crate 必须零 clippy warning：`cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings`。
- 行为改动必须带测试；护栏代码（`guard.rs` / `deliver.rs`）不得改动。
- `crates/rpi-*` 保持原样，不美化、不重构。
- 凭证只从环境变量读取；测试不得使用真实 token/API key。
- 本机构建前先 `taskkill //F //IM nuwa.exe 2>/dev/null || true`（运行中的进程会锁定 exe，cargo 可能静默跳过重链接）。
- 验证与执行一律使用 `C:/Users/lome/.cargo/shared-target/debug/nuwa.exe`（`target/debug/nuwa.exe` 是陈旧遗留）。
- 每个任务一个 commit；提交信息写清 what + why + 验证命令。
- M0 不部署到生产，除非所有者明确授权；计划只准备可发布产物与冒烟步骤。

---

### Task 1: 修复总览筛选说明的反射型 XSS

**Files:**
- Modify: `crates/nuwa/src/ui.rs`（`render_dashboard` 约 270-278 行；新增 `filter_note`）
- Test: `crates/nuwa/src/ui.rs` 的 `#[cfg(test)] mod tests`（文件末尾）

**Interfaces:**
- Produces: `pub fn filter_note(state: Option<&str>, repo: Option<&str>) -> String`，返回已转义的筛选说明 HTML。
- Consumes: 现有 `esc(&str) -> String`。

- [ ] **Step 1: 写失败测试**

在 `ui.rs` 的 tests 模块中，把 `use super::{basic_password_ok, esc, rel_time};` 改为：

```rust
use super::{basic_password_ok, esc, filter_note, rel_time};
```

并追加：

```rust
#[test]
fn filter_note_escapes_query_values() {
    let note = filter_note(
        Some("<img src=x onerror=alert(1)>"),
        Some("<script>alert(1)</script>"),
    );
    assert!(note.contains("&lt;img"));
    assert!(note.contains("&lt;script&gt;"));
    assert!(!note.contains("<img"));
    assert!(!note.contains("<script>"));
    assert!(filter_note(None, None).is_empty());
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa ui::tests::filter_note_escapes_query_values -- --nocapture`

Expected: 编译失败，`cannot find function filter_note`。

- [ ] **Step 3: 实现 `filter_note` 并替换拼接**

在 `esc` 之后新增：

```rust
/// 筛选说明。query 值必须转义后再拼进 HTML——这里是 v2 的反射型 XSS 入口。
pub fn filter_note(state: Option<&str>, repo: Option<&str>) -> String {
    let state = state.map(esc);
    let repo = repo.map(esc);
    match (state.as_deref(), repo.as_deref()) {
        (Some(s), Some(r)) => format!("筛选：state={s} · repo={r}（<a href=\"/ui\">清除</a>）"),
        (Some(s), None) => format!("筛选：state={s}（<a href=\"/ui\">清除</a>）"),
        (None, Some(r)) => format!("筛选：repo={r}（<a href=\"/ui\">清除</a>）"),
        _ => String::new(),
    }
}
```

在 `render_dashboard` 中删除原 `state_note` 的 `match` 块，替换为：

```rust
let state_note = filter_note(d.filter_state.as_deref(), d.filter_repo.as_deref());
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p nuwa ui::tests::filter_note_escapes_query_values -- --nocapture`

Expected: `test result: ok. 1 passed`。

- [ ] **Step 5: 提交**

```bash
git add crates/nuwa/src/ui.rs
git commit -m "fix(ui): escape filter query values in dashboard note (reflected XSS)"
```

---

### Task 2: reopen 支持 rejected，并清除冷却

**Files:**
- Modify: `crates/nuwa/src/ui.rs`（`reopen` handler 约 385-408 行；新增 `can_reopen`）
- Test: `crates/nuwa/src/ui.rs` tests 模块
- Modify: `crates/nuwa/Cargo.toml`（增加 `[dev-dependencies] tempfile`，供本任务与 Task 4 使用）

**Interfaces:**
- Produces: `pub fn can_reopen(state: &str) -> bool`，仅 `escalated` / `rejected` 为 true。
- Consumes: `shanhaijing::Store::set_state`、`set_cooldown`、`ready_issue`。

- [ ] **Step 1: 增加测试依赖**

在 `crates/nuwa/Cargo.toml` 末尾追加：

```toml
[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: 写失败测试**

在 `ui.rs` tests 模块加入：

```rust
use axum::extract::{Form, State};
use axum::http::StatusCode;
use std::collections::HashMap;
use std::sync::Arc;

fn test_signal() -> shanhaijing::Signal {
    shanhaijing::Signal {
        repo: "example.com/spike".into(),
        language: "go".into(),
        title: "panic: boom".into(),
        severity: "high".into(),
        message: None,
        stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.F()\n\t/app/a.go:1 +0x1".into(),
    }
}

fn seed_issue(store: &shanhaijing::Store, states: &[shanhaijing::State]) -> String {
    let now = crate::now_ms();
    store.report_signal(&test_signal(), now).expect("report");
    let fp = store.ready_issue(now).expect("ready").expect("issue").fingerprint;
    for state in states {
        store.set_state(&fp, *state, now, None).expect("state");
    }
    store.set_cooldown(&fp, now + 3_600_000).expect("cooldown");
    fp
}

#[test]
fn can_reopen_only_escalated_or_rejected() {
    assert!(can_reopen("escalated"));
    assert!(can_reopen("rejected"));
    assert!(!can_reopen("new"));
    assert!(!can_reopen("pr_open"));
    assert!(!can_reopen("fixing"));
}

#[tokio::test]
async fn reopen_rejected_clears_cooldown() {
    use shanhaijing::State as IssueState;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
    let fp = seed_issue(
        &store,
        &[
            IssueState::Triggered,
            IssueState::Fixing,
            IssueState::PrOpen,
            IssueState::Rejected,
        ],
    );
    let shared: crate::Shared = (Arc::clone(&store), Arc::new(None));

    let response = reopen(
        State(shared),
        Form(HashMap::from([("fp".to_string(), fp.clone())])),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(store.ready_issue(crate::now_ms()).expect("ready").is_some());
}

#[tokio::test]
async fn reopen_escalated_clears_cooldown() {
    use shanhaijing::State as IssueState;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(shanhaijing::Store::open(dir.path().join("s.db")).expect("open"));
    let fp = seed_issue(
        &store,
        &[IssueState::Triggered, IssueState::Fixing, IssueState::Escalated],
    );
    let shared: crate::Shared = (Arc::clone(&store), Arc::new(None));

    let response = reopen(
        State(shared),
        Form(HashMap::from([("fp".to_string(), fp.clone())])),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(store.ready_issue(crate::now_ms()).expect("ready").is_some());
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p nuwa ui::tests::reopen -- --nocapture`

Expected:
- `reopen_rejected_clears_cooldown` 返回 `409 CONFLICT`（rejected 不被允许）；
- `reopen_escalated_clears_cooldown` 返回 303，但 `ready_issue` 为 `None`（冷却未清）。

- [ ] **Step 4: 实现最小改动**

在 `ui.rs` 中新增：

```rust
pub fn can_reopen(state: &str) -> bool {
    matches!(state, "escalated" | "rejected")
}
```

把 `reopen` handler 中的状态检查改为：

```rust
    if !can_reopen(&row.state) {
        return (
            StatusCode::CONFLICT,
            format!("state is {}, not escalated/rejected", row.state),
        )
            .into_response();
    }
```

把 `set_state` 调用改为：

```rust
    match store.set_state(fp, shanhaijing::State::New, crate::now_ms(), Some("reopened via web console")) {
        Ok(()) => match store.set_cooldown(fp, 0) {
            Ok(()) => Redirect::to("/ui?msg=reopened").into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p nuwa ui::tests::reopen -- --nocapture`

Expected: 两个 async 测试与 `can_reopen_only_escalated_or_rejected` 全部 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/nuwa/src/ui.rs crates/nuwa/Cargo.toml Cargo.lock
git commit -m "fix(ui): allow rejected reopen and clear cooldown on web reopen"
```

---

### Task 3: 设置页 env 键大写化与历史小写键迁移

**Files:**
- Modify: `crates/nuwa/src/settings.rs`（`ENV_KEYS`/`SECRET_KEYS`、`get_settings`、`save_settings`、`apply_form`、`render_settings`、新增 `normalize_known_keys`/`migrate_env_file_at`）
- Modify: `crates/nuwa/src/main.rs`（`run_serve` 启动时执行迁移）
- Test: `crates/nuwa/src/settings.rs` 新增 tests 模块

**Interfaces:**
- Produces:
  - `pub fn normalize_known_keys(map: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>)`
  - `pub fn migrate_env_file_at(path: &std::path::Path) -> Result<Vec<String>, String>`
  - `pub fn migrate_env_file() -> Result<Vec<String>, String>`
- Consumes: 现有 `env_file_path`、`parse_env`、`render_env`。

- [ ] **Step 1: 写失败测试**

在 `settings.rs` 末尾新增：

```rust
#[cfg(test)]
mod tests {
    use super::{apply_form, migrate_env_file_at, normalize_known_keys, render_env};
    use std::collections::{BTreeMap, HashMap};

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn migration_moves_lowercase_known_keys_to_uppercase() {
        let old = map(&[("nuwa_interval", "30"), ("nuwa_provider", "zai")]);
        let (new, changed) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("30"));
        assert_eq!(new.get("NUWA_PROVIDER").map(String::as_str), Some("zai"));
        assert!(!new.contains_key("nuwa_interval"));
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));
    }

    #[test]
    fn uppercase_wins_over_lowercase_duplicate() {
        let old = map(&[("NUWA_INTERVAL", "60"), ("nuwa_interval", "30")]);
        let (new, _) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("60"));
        assert!(!new.contains_key("nuwa_interval"));
    }

    #[test]
    fn apply_form_reads_and_validates_uppercase_numbers() {
        let old = map(&[("NUWA_INTERVAL", "30")]);
        let form = HashMap::from([("NUWA_INTERVAL".to_string(), "60".to_string())]);
        let (new, changed) = apply_form(&old, &form).expect("valid");
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("60"));
        assert_eq!(changed, vec!["NUWA_INTERVAL".to_string()]);

        let bad = HashMap::from([("NUWA_INTERVAL".to_string(), "0".to_string())]);
        assert!(apply_form(&old, &bad).is_err());
    }

    #[test]
    fn render_env_writes_uppercase_keys() {
        let out = render_env(&map(&[("NUWA_INTERVAL", "60")]));
        assert!(out.contains("NUWA_INTERVAL=60"));
        assert!(!out.contains("nuwa_interval"));
    }

    #[test]
    fn migrate_env_file_is_idempotent_and_keeps_uppercase_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(&path, "NUWA_INTERVAL=60\nnuwa_interval=30\nnuwa_provider=zai\n").expect("write");

        let changed = migrate_env_file_at(&path).expect("migrate");
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("NUWA_INTERVAL=60"));
        assert!(text.contains("NUWA_PROVIDER=zai"));
        assert!(!text.contains("nuwa_interval"));
        assert!(!text.contains("nuwa_provider"));

        assert!(migrate_env_file_at(&path).expect("second").is_empty());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa settings::tests -- --nocapture`

Expected: 编译失败，`normalize_known_keys` / `migrate_env_file_at` 不存在；`apply_form` 仍按小写键工作。

- [ ] **Step 3: 把常量与表单键改为大写**

将 `ENV_KEYS` 与 `SECRET_KEYS` 全部改为大写：

```rust
pub const ENV_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "ZAI_API_KEY",
    "NUWA_INGEST_TOKEN",
    "NUWA_UI_TOKEN",
    "NUWA_PROVIDER",
    "NUWA_MODEL",
    "NUWA_BASE_URL",
    "NUWA_INTERVAL",
    "NUWA_CI_TIMEOUT",
    "NUWA_DAILY_PR_LIMIT",
    "NUWA_MAX_TURNS",
    "NUWA_MAX_TOKENS",
];

const SECRET_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "ZAI_API_KEY",
    "NUWA_INGEST_TOKEN",
    "NUWA_UI_TOKEN",
];
```

把 `apply_form` 中的 `match *key` 分支改为大写键：

```rust
        let value = match *key {
            "NUWA_PROVIDER" => {
                if !matches!(trimmed, "zai" | "anthropic" | "faux") {
                    return Err(format!("NUWA_PROVIDER 非法：{trimmed}（zai|anthropic|faux）"));
                }
                trimmed.to_string()
            }
            "NUWA_INTERVAL" | "NUWA_CI_TIMEOUT" | "NUWA_DAILY_PR_LIMIT" | "NUWA_MAX_TURNS"
            | "NUWA_MAX_TOKENS" => {
                let n: u64 = trimmed
                    .parse()
                    .map_err(|_| format!("{key} 必须是正整数，得到 {trimmed:?}"))?;
                if n == 0 {
                    return Err(format!("{key} 不能为 0"));
                }
                n.to_string()
            }
            _ => trimmed.to_string(),
        };
```

把 `render_settings` 中所有 provider 相关的小写键改为大写：

```rust
    let provider_options = ["zai", "anthropic", "faux"]
        .iter()
        .map(|p| {
            let sel = if get("NUWA_PROVIDER") == *p { " selected" } else { "" };
            format!("<option value=\"{p}\"{sel}>{p}</option>")
        })
        .collect::<String>();
```

并把表单行改为：

```html
<tr><td>provider</td><td class="mono dim">NUWA_PROVIDER</td><td><select name="NUWA_PROVIDER">{provider_options}</select></td></tr>
```

- [ ] **Step 4: 实现归一化与迁移**

在 `parse_env` 之后新增：

```rust
use std::collections::BTreeSet;

/// 把历史小写键并入规范大写键；大写优先，删除小写重复项。
pub fn normalize_known_keys(map: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut out = map.clone();
    let mut changed = BTreeSet::new();
    for key in ENV_KEYS {
        let lower = key.to_ascii_lowercase();
        if let Some(lower_value) = map.get(&lower).cloned() {
            if !map.contains_key(*key) {
                out.insert((*key).to_string(), lower_value);
            }
            out.remove(&lower);
            changed.insert((*key).to_string());
        }
    }
    (out, changed.into_iter().collect())
}

pub fn migrate_env_file_at(path: &std::path::Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let (new, changed) = normalize_known_keys(&parse_env(&text));
    if changed.is_empty() {
        return Ok(changed);
    }
    std::fs::write(path, render_env(&new)).map_err(|e| format!("write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(changed)
}

pub fn migrate_env_file() -> Result<Vec<String>, String> {
    migrate_env_file_at(&env_file_path())
}
```

在 `get_settings` 中把：

```rust
    let map = parse_env(&text);
```

改为：

```rust
    let (map, _) = normalize_known_keys(&parse_env(&text));
```

在 `save_settings` 中把：

```rust
    let old = parse_env(&text);
```

改为：

```rust
    let (old, _) = normalize_known_keys(&parse_env(&text));
```

- [ ] **Step 5: 启动时迁移**

在 `main.rs` 的 `run_serve` 里，解析参数后、打开 store 前加入：

```rust
    match settings::migrate_env_file() {
        Ok(changed) if !changed.is_empty() => {
            println!("env        : migrated lowercase keys -> {}", changed.join(", "));
        }
        Ok(_) => {}
        Err(e) => eprintln!("env migrate: {e}"),
    }
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test -p nuwa settings::tests -- --nocapture`

Expected: 5 个测试全部 PASS。

- [ ] **Step 7: 提交**

```bash
git add crates/nuwa/src/settings.rs crates/nuwa/src/main.rs
git commit -m "fix(settings): read/write uppercase NUWA_* keys and migrate legacy lowercase env"
```

---

### Task 4: 配额待命前写心跳

**Files:**
- Modify: `crates/nuwa/src/main.rs`（`work_once`：把 `store.heartbeat(now)?` 移到配额检查之前）
- Test: `crates/nuwa/src/main.rs` 末尾新增 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `work_once(...)` 现有签名；`Store::set_pr`、`last_heartbeat`、`prs_opened_since`。
- Produces: 无新 API；行为约定：任何 `work_once` 返回前至少写一次心跳。

- [ ] **Step 1: 写失败测试**

在 `main.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::RepoInstructions;
    use crate::issue::FailingCheck;

    #[tokio::test]
    async fn quota_stand_down_still_writes_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = shanhaijing::Store::open(dir.path().join("s.db")).expect("open");
        let now = now_ms();
        let signal = shanhaijing::Signal {
            repo: "example.com/spike".into(),
            language: "go".into(),
            title: "panic: boom".into(),
            severity: "high".into(),
            message: None,
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/spike.F()\n\t/app/a.go:1 +0x1".into(),
        };
        store.report_signal(&signal, now).expect("report");
        let fp = store.ready_issue(now).expect("ready").expect("issue").fingerprint;
        store.set_pr(&fp, "https://github.com/acme/spike/pull/1", now).expect("pr");

        let cfg = RepoInstructions {
            repo: "example.com/spike".into(),
            failing_check: FailingCheck { command: "true".into(), target: None },
            forbidden_paths: Vec::new(),
            notes: String::new(),
        };
        let worked = work_once(
            &store,
            &cfg,
            dir.path(),
            dir.path().join("sessions"),
            repair::ProviderKind::Faux,
            30,
            false,
            true,
            1,
            1_000_000,
            600,
        )
        .await
        .expect("work_once");

        assert!(!worked, "quota must stand down");
        let hb = store.last_heartbeat().expect("heartbeat").expect("some heartbeat");
        assert!((now_ms() - hb).abs() < 5_000, "heartbeat must be fresh");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa tests::quota_stand_down_still_writes_heartbeat -- --nocapture`

Expected: `assertion failed: hb.is_some()` 或 `some heartbeat` panic（配额分支在心跳之前返回）。

- [ ] **Step 3: 移动心跳写入**

在 `work_once` 中，把 `let now = now_ms();` 之后、配额检查之前加入：

```rust
    // 活性必须在任何早退之前可见：配额待定/预检失败都不是"进程死了"。
    store.heartbeat(now)?;
```

删除后面 `// ---- claim ... ----` 段里原有的：

```rust
    store.heartbeat(now)?;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p nuwa tests::quota_stand_down_still_writes_heartbeat -- --nocapture`

Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/nuwa/src/main.rs
git commit -m "fix(work): write heartbeat before quota stand-down so liveness is visible"
```

---

### Task 5: 旧 POST 的同源校验（过渡缓解）

**Files:**
- Modify: `crates/nuwa/src/ui.rs`（新增 `authority`、`origin_ok`、`origin_guard`）
- Modify: `crates/nuwa/src/main.rs`（`ui_routes` 挂载中间件）
- Test: `crates/nuwa/src/ui.rs` tests 模块

**Interfaces:**
- Produces:
  - `pub fn origin_ok(origin: Option<&str>, referer: Option<&str>, host: Option<&str>) -> bool`
  - `pub async fn origin_guard(headers: HeaderMap, req: Request, next: Next) -> Response`
- Consumes: 现有 `HeaderMap`、`Request`、`Next`、`StatusCode` 导入。

- [ ] **Step 1: 写失败测试**

在 `ui.rs` tests 模块加入：

```rust
#[test]
fn origin_guard_allows_same_origin_and_non_browser_clients() {
    assert!(super::origin_ok(None, None, Some("nuwa.example:7321")));
    assert!(super::origin_ok(
        Some("https://nuwa.example:7321"),
        None,
        Some("nuwa.example:7321")
    ));
    assert!(super::origin_ok(
        None,
        Some("https://nuwa.example:7321/ui"),
        Some("nuwa.example:7321")
    ));
}

#[test]
fn origin_guard_rejects_cross_origin_and_malformed_values() {
    assert!(!super::origin_ok(
        Some("https://evil.example"),
        None,
        Some("nuwa.example:7321")
    ));
    assert!(!super::origin_ok(
        Some("https://nuwa.example:7321"),
        None,
        None
    ));
    assert!(!super::origin_ok(Some("not-a-url"), None, Some("nuwa.example:7321")));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p nuwa ui::tests::origin_guard -- --nocapture`

Expected: 编译失败，`origin_ok` 不存在。

- [ ] **Step 3: 实现同源判断与中间件**

在 `ui.rs` 的 `ui_auth` 之前加入：

```rust
fn authority(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let end = rest
        .find(|c| matches!(c, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() { None } else { Some(host) }
}

/// 过渡缓解：Basic Auth 会被浏览器自动携带，跨站表单 POST 需要拦截。
/// 无 Origin/Referer 的非浏览器客户端（curl、脚本）保持可用。
pub fn origin_ok(origin: Option<&str>, referer: Option<&str>, host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let candidate = match origin {
        Some(origin) => authority(origin),
        None => referer.and_then(authority),
    };
    match candidate {
        Some(host_from_header) => host_from_header.eq_ignore_ascii_case(host),
        None => origin.is_none() && referer.is_none(),
    }
}

pub async fn origin_guard(headers: HeaderMap, req: Request, next: Next) -> Response {
    if req.method() == axum::http::Method::POST {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let referer = headers.get(header::REFERER).and_then(|v| v.to_str().ok());
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        if !origin_ok(origin, referer, host) {
            return (StatusCode::FORBIDDEN, "cross-origin POST rejected").into_response();
        }
    }
    next.run(req).await
}
```

- [ ] **Step 4: 挂载中间件**

在 `main.rs` 的 `ui_routes` 构建中，`route_layer(ui_auth)` 之前或之后追加：

```rust
        .route_layer(axum::middleware::from_fn(ui::origin_guard))
```

最终顺序建议：

```rust
    let ui_routes = axum::Router::new()
        .route("/ui", get(ui::dashboard))
        .route("/ui/reopen", post(ui::reopen))
        .route("/ui/sync", post(ui::sync_now))
        .route("/issue/{fp}", get(ui::issue_detail))
        .route("/settings", get(settings::get_settings))
        .route("/settings/save", post(settings::save_settings))
        .route("/settings/restart", post(settings::restart_services))
        .route_layer(ui_auth)
        .route_layer(axum::middleware::from_fn(ui::origin_guard))
        .with_state((Arc::clone(&store), Arc::clone(&ui_token)));
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p nuwa ui::tests::origin_guard -- --nocapture`

Expected: 2 个测试 PASS。

- [ ] **Step 6: 手工验证跨站 POST 被拒绝**

启动本地 serve（用临时 env 文件与临时 DB）：

```bash
taskkill //F //IM nuwa.exe 2>/dev/null || true
NUWA_ENV_FILE=/tmp/nuwa-m0.env \
  C:/Users/lome/.cargo/shared-target/debug/nuwa.exe serve \
  --db /tmp/nuwa-m0.db --listen 127.0.0.1:7399 > /tmp/nuwa-m0.log 2>&1 &
sleep 2
curl -s -o /dev/null -w '%{http_code}\n' \
  -X POST -H 'Origin: https://evil.example' \
  http://127.0.0.1:7399/ui/sync
```

Expected: `403`。（serve 未设置 token 时为本地模式，认证会放行，同源校验仍生效。）

- [ ] **Step 7: 提交**

```bash
git add crates/nuwa/src/ui.rs crates/nuwa/src/main.rs
git commit -m "fix(ui): reject cross-origin POSTs on legacy console (CSRF mitigation)"
```

---

### Task 6: 全量验证、文档同步与发布准备

**Files:**
- Modify: `docs/07-open-items.md`（§D 追加 M0 热修记录）
- No code changes expected.

**Interfaces:** 无。

- [ ] **Step 1: 运行自建 crate 的 clippy 与测试**

```bash
taskkill //F //IM nuwa.exe 2>/dev/null || true
cargo clippy --no-deps -p nuwa -p shanhaijing --all-targets -- -D warnings
cargo test -p nuwa -p shanhaijing
```

Expected: clippy 零 warning；nuwa + shanhaijing 全部测试 PASS。不要在本机跑 `cargo test --workspace`（`rpi-tools` 的 Windows 取消时序测试是已知上游失败）。

- [ ] **Step 2: 手工冒烟安全与设置**

```bash
NUWA_ENV_FILE=/tmp/nuwa-m0.env \
  C:/Users/lome/.cargo/shared-target/debug/nuwa.exe serve \
  --db /tmp/nuwa-m0.db --listen 127.0.0.1:7399 > /tmp/nuwa-m0.log 2>&1 &
sleep 2

# XSS 不应原样回显
curl -s 'http://127.0.0.1:7399/ui?state=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E' \
  | grep -q '<img src=x' && echo 'FAIL: XSS' || echo 'OK: escaped'

# 设置页数字项应显示值（先用大写键准备 env）
printf 'NUWA_INTERVAL=30\nNUWA_CI_TIMEOUT=600\nNUWA_PROVIDER=zai\n' > /tmp/nuwa-m0.env
curl -s http://127.0.0.1:7399/settings | grep -o 'name="NUWA_INTERVAL" value="[^"]*"'

# 跨站 POST 应 403
curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  -H 'Origin: https://evil.example' http://127.0.0.1:7399/ui/sync
```

Expected:
- `OK: escaped`
- 设置页出现 `name="NUWA_INTERVAL" value="30"`（或字段渲染为 30）
- `403`

- [ ] **Step 3: 停止本地 serve**

```bash
netstat -ano | grep ':7399' | head -1
# 记录 PID 后：
taskkill //PID <PID> //F
```

- [ ] **Step 4: 更新进度快照**

在 `docs/07-open-items.md` 的 `## D · 进度快照` 末尾、"下一步"之前追加一条（把测试数替换为实际值）：

```markdown
- **Web 控制台 M0 热修（2026-10-17）**：反射型 XSS（筛选参数转义）、rejected 无法 reopen、web reopen 不清冷却、设置页 env 键大小写（大写化 + 启动迁移历史小写键）、配额待命不写心跳、旧 POST 无同源校验五项修复。自建 crate clippy 零 warning、nuwa/shanhaijing 测试全绿（新增 N 个测试）；本地冒烟通过。
```

- [ ] **Step 5: 提交文档**

```bash
git add docs/07-open-items.md
git commit -m "docs: record M0 web console hotfix and verification evidence"
```

- [ ] **Step 6: 发布准备（需所有者批准后执行）**

发布流程不变：合并 main → 由所有者决定 tag `v0.1.6` → GitHub Actions 构建双平台产物 → 按 `docs/08-onboarding.md` 部署。**不要在本计划执行过程中自行 tag、推送或部署生产。**

---

## 自审记录

- 规格覆盖：M0 的六项（XSS、reopen+冷却、env 大写迁移、心跳顺序、同源校验、验证/文档）均有对应任务。
- 占位符：无 TBD/TODO；所有测试与实现代码均为可编译片段。
- 类型一致性：`can_reopen(&str) -> bool`、`filter_note(Option<&str>, Option<&str>) -> String`、`origin_ok(Option<&str>, Option<&str>, Option<&str>) -> bool` 在测试与实现中一致；测试复用的 `crate::Shared` 是 `(Arc<Store>, Arc<Option<String>>)`。
- 已知边界：同源校验只是过渡缓解，不是完整 CSRF；v3 的会话 + double-submit 在 M2 实现。
