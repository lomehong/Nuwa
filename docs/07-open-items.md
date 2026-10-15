# 07 · 待详细设计与待办

状态：动态维护（2026-10-15 初版）

## A · 详细设计（按优先级）

1. **问题单状态机与指纹算法**（山海经）
   - 状态：new → triggered → fixing → pr-open → merged / rejected / escalated
   - 指纹：归一化堆栈 + 模块/错误类；同语言 parser 策略（Go goroutine trace / Java 异常链 / Rust panic）
   - 冷却窗口、优先级（频次 × 严重度）、升级（escalate 给所有者）规则
   - 存储选型（SQLite 起步即可）
2. **PR 工具接口**（五色石交付面）
   - `open_pull_request(repo, branch, title, body)` AgentTool 签名与实现（git + GitHub API / gh CLI）
   - 分支命名 `agent/fix/<issue-fingerprint>`；PR 正文模板（问题单链接、根因摘要、改动清单、验证证据）
   - deploy key 管理、防越权仓库访问
3. **运行器池调度**（应龙）
   - 按技术栈选 runner；并发与队列；预算闸（每问题单 token 上限 + 每仓库每日 PR 上限）接入 rpi-harness budget 钩子
   - runner 镜像矩阵（per-stack 工具链 + 离线依赖缓存）
4. **验证探针协议**（现场验证，命名待定）
   - 补丁下发 → 现场执行预定义脚本 → 回报 pass/fail + 新日志 的安全协议；触发条件（中心 runner 无法复现的环境特定 bug）
5. **青鸟采集 SDK 事件 schema**
   - 统一事件信封（语言无关）+ 各语言实现（Go 起步？按你的系统实际语言定）
   - 上下文包字段与体积上限（草案：指纹、日志窗口前后 N 行、环境元数据、复现输入、频次；脱敏规则）

## B · 工程待办

- [ ] fork pi-rust → 裁剪出 rpi-agent / rpi-ai / rpi-tools / rpi-harness 四 crate 的 Nuwa 工作区骨架
- [ ] 保留 crates 补 `[lints]` + clippy 进 CI；盘点并削减生产 unwrap（参考热点：上游 rpi-harness/src/agent_harness.rs 5771 行 / 107 unwrap）
- [ ] 锁定 vendor 版本，建 upstream remote 与 cherry-pick 流程
- [ ] 自定义 AgentTool 骨架：`open_pull_request`、`read_issue`（问题单上下文注入）
- [ ] 第一个试点系统选型（建议：选一个 Go 或 Java 的自有小服务，闭环跑通"问题单 → PR"）
- [ ] 文档：runner 镜像构建手册、接入指南（接入清单落地为 checklist）

## C · 已明确不做

- 通用对话 / 交互式 TUI（上游 rpi-tui、rpi-cli 直接裁掉）
- 现场生产进程内运行 LLM 循环或 shell 执行
- 面向第三方的多租户 SaaS
- 监控指标采集与告警路由（消费既有可观测性设施的问题信号）

## D · 进度快照（2026-10-15 · 阶段 0+1 完成）

**骨架（B1-B3 落地）**
- vendored 五 crate：rpi-telemetry/ai/agent/tools/harness @ pi-rust 0.3.24（7210550），commit `3446a82`，tag `vendored/pi-rust-0.3.24`。ADR-008 清单两处修正：rpi-telemetry 是三个保留 crate 的硬依赖（593 行，一并保留）；rpi-tools 硬依赖 rpi-plugin-sdk 的两个 env 常量，SDK 整体砍除、常量内联进 `crates/rpi-tools/src/os_env.rs`（`tests/host_private_env.rs` 钉住拼写）。
- `[workspace.lints]` + clippy 全绿（0 error）；上游 ≈730 unwrap 以 `unwrap_used=warn` 保持可见，削减未动。
- **上游已知问题**：`rpi-tools` `execution_env_conformance::os_cancel_interrupts_even_when_timeout_is_set` 在本机 Windows 失败（进程取消时序），上游原树同样失败——预存问题，非裁剪引入。

**脊柱 spike（B4/B5 部分落地）**
- `crates/nuwa` bin：问题单 JSON → rpi-harness（JSONL session 每 fingerprint 一目录，crash-resumable）→ 工具带（read/edit/write/bash/grep/find/ls）→ 修复 → 独立验证门 → `agent/fix/<fp>` 分支 → 提交。
- 离线 faux 路径全链路已验证：真实 edit 工具落盘、验证门 `go test ./...` 通过、分支/提交/护栏（空改动拒绝、重复分支待验）全部工作。真实 LLM 路径待 `ANTHROPIC_API_KEY`；真实 PR 待 origin remote + `NUWA_GITHUB_TOKEN`（细粒度，仅 `pulls:write`）。
- 护栏（ADR-003 五件套其三）：禁改路径 + git/gh 命令否决（`before_tool_call` 钩子，9 个单元测试）；分支命名空间 `agent/fix/*`；turn 级预算（`RPI_MAX_TURNS_PER_RUN`）。token 级预算随运行器池调度做（A-3）。
- **山海经 A-1 落地（`crates/shanhaijing`）**：SQLite 存储 + 指纹算法（Go goroutine trace / Java 异常链 / Rust panic 三种 parser + 归一化：数字/地址折叠，栈帧剥行号，跳过 runtime/JDK 噪音帧）+ 去重/冷却/回归重开 + 状态机（new→triggered→fixing→pr_open→merged/rejected/escalated，非法迁移拒绝）+ 优先级（severity × occurrences）。11 个单元测试 + 真实 panic 信号 ingest 实测（首报 new、二报 dedup）。入口 `nuwa ingest --signal <json>`。
- **设计演进（相对 A-2）**：`open_pull_request` 不做成 AgentTool，也不用 gh CLI——PR 由 run 后的交付步骤（`deliver.rs`）经 GitHub REST API + 细粒度 token（`NUWA_GITHUB_TOKEN`，仅 `pulls:write`）创建；push 走 git/deploy key。护栏禁止 agent 运行 `gh`/`git push`。理由：自主开 PR 的闸门不应在同一 agent 循环内，凭证最小化符合 ADR-004。
- **运行器调度 v0（A-3 首半）**：`nuwa work` 支撑三道闸——①每问题单 token 上限（`NUWA_MAX_TOKENS_PER_ISSUE`/`--max-tokens`，交付前硬闸，超限升级；turn 级上限继续在 run 内兜底）②每仓库每日 PR 上限（`--daily-pr-limit`，UTC 窗口，配额耗尽 runner **不领单**原地待命，问题单保持 `new` 次日自愈）③turn 级预算（run 内）。`--loop --interval N` 常驻轮询（串行 runner；并发池与 runner 镜像矩阵是 A-3 后半）。实测：配额闸 standing-down、循环轮询、修复分支已排除指令文件（修复过 `git add -A` 吞 `nuwa.repo.json` 的交付面缺陷）。
- **并发安全领单 + 现场预检**：`claim_ready_issue()` 用单条 `UPDATE…RETURNING` 原子翻转 triggered+冷却（多 runner 进程永不同领一单，双进程实测互斥）；领单前预检目标仓库（必须在 main/master、工作区净）——实测抓到"工作区停在旧 agent 分支导致空跑"的缺陷并修复。`nuwa reopen --fingerprint` 补上 escalated 的人工复核入口（escalated→new，清冷却；报告永不自动重开 escalated）。
- **青鸟 Go SDK 端到端**：真实 panic → `runtime.Stack()` 全 goroutine 捕获 → FileSink JSONL → ingest 指纹 `7acff4a2…` → work 领单修复 → 验证门 → 分支。采集到修复无人干预贯通（faux provider）。
- **接入手册落地**：docs/08（B6 接入指南部分）——四步接入 + 命令速查 + 护栏代码位置表。
- 下一步：真实 LLM + 真实 PR 闭环（等凭证）→ 并发池/runner 镜像矩阵（A-3 后半）→ ingest HTTP 服务端 → 验证探针协议（A-4，等第一个环境特定 bug 出现再设计）。

## E · clippy / unwrap 基线（2026-10-15 盘点，`clippy -p <crate> --lib`）

| crate | warnings | 其中 unwrap_used | 说明 |
|---|---|---|---|
| rpi-harness | 226 | 148 | **削减主战场**；热点 agent_harness.rs（5771 行） |
| rpi-tools | 69 | 23 | os_env.rs 已改动区保持干净 |
| rpi-agent | 47 | 20 | 核心循环，改动需行为级测试护航 |
| rpi-ai | 38 | 18 | provider 适配层 |
| rpi-telemetry | 5 | 3 | 最小 |
| shanhaijing / nuwa | 0 | 0 | 自有代码零 warning 基线，CI 门槛按此执行 |

削减策略（B2 待办执行时用）：自建 crate `deny`；vendored crate 按 crate 分批把 `unwrap_used` 从 warn 收紧为 deny，热点先行（harness），每批必须过上游测试套（1662 测试是安全网）。
