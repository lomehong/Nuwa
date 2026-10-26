# AGENTS.md — Nuwa 仓库 agent 操作规程

给在本仓库工作的 AI coding agent。目标：不重踩已踩过的坑，不破坏已建立的约定。拿不准的事，答案通常在 `docs/`（设计：01/03，决策：04，进度与陷阱：07，接入操作：08）。

## 仓库是什么（30 秒）

Nuwa = 自愈式修复 agent：运行时问题信号 → 山海经问题单（SQLite）→ 应龙 runner 修复（vendored rpi-harness）→ 独立验证门 → `agent/fix/*` 分支 → GitHub REST API 开 PR。**注意区分**：本文件的读者是"修 Nuwa 的 agent"；Nuwa 修复目标仓库时读的是目标仓的 `nuwa.repo.json`，两者不可混淆。

当前版本 `0.15.0`：控制台是 `nuwa serve` 内嵌的 SPA（单二进制），旧服务端渲染控制台已移除。

## 布局

| 路径 | 是什么 |
|---|---|
| `crates/rpi-{telemetry,ai,agent,tools,harness}` | **vendored**，源自 bigfish1913/pi-rust @ 0.3.24（tag `vendored/pi-rust-0.3.24`） |
| `crates/nuwa` | 自建主 bin：`ingest` / `work` / `fix` / `reopen` |
| `crates/shanhaijing` | 自建问题单库（指纹/状态机/冷却） |
| `sdk/qingniao-go` | （已迁出）青鸟 Go 采集 SDK 独立为公开仓 [lomehong/qingniao-go](https://github.com/lomehong/qingniao-go)，接入目标仓用 `go get github.com/lomehong/qingniao-go@vX.Y.Z` |
| `docs/` | 设计、ADR（04）、进度快照与陷阱（07 §D）、接入手册（08） |
| `.upstream/` | 上游完整 clone，cherry-pick 参考；gitignored，永不提交 |
| `spike/` | 现场实验区：toy 仓库、信号样例、session 转录；gitignored |
| `shanhaijing.db*` | 运行时产物；gitignored |

## 命令

```bash
cargo clippy --workspace --all-targets   # 自建 crate 必须零 warning
cargo test --workspace                   # 上游 1662 测试是安全网
# CI 用 nextest（每测试独立进程 + 120s 硬超时）：
#   cargo nextest run --workspace --profile ci && cargo test --workspace --doc
# 青鸟 Go SDK 的测试在独立仓 github.com/lomehong/qingniao-go（已迁出）
cd web && npm ci && npm run typecheck && npm run lint && npm test && npm run build
```

冒烟/演示命令见 docs/08 与 README 快速上手。

## 发版

升版要同时改 **4 处**，漏任何一处都会让 `cargo metadata --locked`（`scripts/check-arch.py` 与 release.yml 都会调）直接失败：

1. `crates/nuwa/Cargo.toml` 的 `version`
2. `crates/shanhaijing/Cargo.toml` 的 `version`
3. 根 `Cargo.toml` 的 `[workspace.dependencies] shanhaijing = { path = ..., version = ... }`（最容易漏）
4. `Cargo.lock` 里 `nuwa` / `shanhaijing` 两个 package 的 `version`

然后：

```bash
git tag -a vX.Y.Z -m "vX.Y.Z — <一句话>"
git push origin main && git push origin vX.Y.Z   # tag 触发 release.yml
```

tag 打错（指向坏提交、未产出 Release）时：`git tag -d vX.Y.Z` → 在正确提交上重打 → `git push origin :refs/tags/vX.Y.Z` 删除远端旧 tag → 再推送新 tag。

`v*` tag 必须指向 main 上 **ci.yml 已绿**的提交：release 不重跑 Rust 测试，`gate` job 会用 Actions API 校验该提交的 ci.yml 结论；未绿（或只打 tag、没走过 main CI）会在 gate 失败。

## 硬规则

1. **vendored 纪律**：`crates/rpi-*` 保持原名、原风格、原注释。不美化、不重构、不"顺手清理"；需要上游修复 → `git remote add upstream https://github.com/bigfish1913/pi-rust` + cherry-pick。vendored 代码里 `unwrap_used=warn` 是削减中的可见性手段（基线见 docs/07 §E），不是放行——不要批量改掉。
2. **自建 crate 硬标准**：`nuwa` / `shanhaijing` 零 clippy warning；行为改动必须带测试；护栏代码（`nuwa` 的 `guard.rs` / `deliver.rs`）改动必须同步改护栏测试——五件套护栏是产品存在前提（ADR-003），不是可选优化。
3. **凭证红线**：token/key 只从环境变量读（`NUWA_GITHUB_TOKEN`、`ANTHROPIC_API_KEY`）；永不入库、永不写进文档或示例的真实值。
4. **交付面契约**：staging 与变更判定都用 `:(exclude)nuwa.repo.json` pathspec——这是修过的真实缺陷（runner 曾把仓库配置提交上 agent 分支），别"简化"掉。
5. **已知环境问题，不是你的 bug，别"修"**：`rpi-tools` 的 `execution_env_conformance::os_cancel_interrupts_even_when_timeout_is_set` 在 Windows 失败——上游原树同样失败（预存问题）。
6. **本机 cargo 环境专有坑**：全局 `~/.cargo/config.toml` 把 `target-dir` 重定向到 `C:\Users\lome\.cargo\shared-target`（为省 E 盘空间）。后果：① `E:\...\target\debug\nuwa.exe` 是陈旧遗留，**验证/执行一律用 shared-target 下的产物**；② 运行中的 serve/work 会锁定 exe 导致 cargo 重链接静默跳过——**构建/验证前先 `taskkill /F /IM nuwa.exe`**；③ E 盘写入 mtime 停滞（`touch` 无效），改码后增量构建可能空转——用 `cargo clean -p <crate>` 强制，或直接跑 shared-target 产物看行为。
7. **Go 代码**：Go 1.22+，整数循环用 `for i := range n`；唯一例外是 `spike/target-repo` 的 seeded bug（那是被修的对象，改了它 spike 就废了）。
8. **提交前**：clippy + test 全绿；一次提交一个完整变更，message 写清 what + why + 验证证据。
9. **文档同步**：行为改动更新 docs/07 §D 进度快照；接入操作变化更新 docs/08；新决策走 docs/04 ADR。

## 领土

`.upstream/`、`spike/`、`*.db` 属于本地工作区，gitignored。不要提交它们的内容，也不要把它们当作用户无关的垃圾清掉——spike 是验证流水线的现场。
