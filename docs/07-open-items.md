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
- **ingest HTTP 服务端（`nuwa serve`）**：axum，`POST /ingest`（单信封或 JSONL）+ `GET /health`；与文件 ingest 共享解析。实测：青鸟 `HTTPSink`（application/json）直连入库 3/3、curl dedup、status 可见。**修复 axum String extractor 强制 text/plain 导致 415 的坑**（改用 Bytes）。v0 仅绑 localhost（采集器同信任域）。
- **接入手册落地**：docs/08（B6 接入指南部分）——四步接入 + 命令速查 + 护栏代码位置表。
- **真实 PR 闭环达成（2026-10-16）**：`nuwa work --pr` 全自动——领单 a7ca566f（escalated→reopen→claimed）→ faux 修复 → 验证门 → push `agent/fix/a7ca566fcf76d91a` → REST API 开出 [nuwa-spike-target#1](https://github.com/lomehong/nuwa-spike-target/pull/1)（state=open，标题/正文含指纹与验证证据）→ `pr_open` + `pr_url` 回写（`nuwa status` 可见）。细粒度 PAT 权限实操结论：**Pull requests: RW + Contents: R**（缺 Contents:Read 时 API 422 "not all refs are readable"；push 走本机 Git 凭证不需要 token 写内容）。接入手册凭证表同步。
- **真实 LLM 闭环达成（2026-10-16，智谱 coding plan）**：`--provider zai`（OpenAI-completions 兼容，base `…/paas/v4` 自动拼 `/chat/completions`）+ `glm-5.3-flash` 全自动：领单 `f2144b83` → 读代码定位 → 编辑（首次绝对路径被护栏否决后**自行改正为相对路径**）→ 修复 → `go test -count=1 -v` 自验 → 验证门 → **[PR #3](https://github.com/lomehong/nuwa-spike-target/pull/3)**（state=open）→ 回写。修复耗时 ~65s 全程无人干预。
- **usage 接线修复**：上游 rpi 的 `AgentLane::record_usage` 无生产调用方（移植缺口），session 统计恒 0、预算闸失明。nuwa 侧在事件汇把每 `TurnEnd` 的 assistant usage 喂回 harness；实测 glm-5.3-flash 单问题单 **17475 tokens** 真实计费入账（智谱流式 usage 含 cached_tokens 细分，parse_usage 直接兼容）。
- **GitHub CI 编排（2026-10-16）**：①主仓 Actions（`.github/workflows/ci.yml`）——rust：自建 crate `clippy --no-deps -D warnings` 零警告 + 全量测试；go：qingniao vet+test。首跑踩坑：clippy 命令行 `-D` 会**波及 vendored workspace 成员**（rpi-telemetry 的 unwrap 在新工具链升 error）→ `--no-deps` 圈定范围；附带清掉跨平台/`--all-targets` 下自建代码的 6 个 lint（含 workspace MSRV 升 1.79）。两 job 双绿（`06e7da0`）。②target 仓 ci.yml（go test）就绪，待 PR #3 人工合并落 main 后生效——PR 的 workflow 文件取自 base 分支。③`nuwa ci.rs` + `work --ci-timeout`：交付后轮询 check-runs，绿→留人工合并；**红→自动关 PR + 问题单 rejected**（回归信号自动重开）；超时→保持 open 报警。aggregate 纯函数可测。
- **CI 闸门消费 e2e 达成（2026-10-16）**：`work --pr --ci-timeout` 全自动——领单 `9287e35a` → glm-5.3-flash 修复（17099 tokens）→ 验证门 → [PR #5](https://github.com/lomehong/nuwa-spike-target/pull/5)（state=open）→ **触发 target Actions（ci.yml go test）→ nuwa ci gate 轮询 actions/runs → all checks green → 留人工合并**。99 秒无人干预。**权限实操修正**：细粒度 PAT 下拉里**没有 "Checks" 权限项**（此前记错目录）；check-runs 端点对私有仓需 Checks 权限（403），而 `Actions: Read-only` + `actions/runs?head_sha=` 端点即等价数据源（workflow_runs 的 status/conclusion）——已切换，不再依赖 check-runs。另：push 网络瞬断（CN→GitHub）加 3 次重试；`nuwa sync` 对账命令落地（pr_open ↔ GitHub 实态：merged→merged、closed→rejected、open 不动，实测三态全对）。
- **Release 流水线达成（2026-10-16）**：`release.yml`（tag `v*` 触发）→ 双平台（win/linux x64）`cargo build --release` → `gh release create` 自动发布。首跑踩坑：publish job 无 checkout 时 gh 报 "not a git repository"（gh 靠 git 上下文定位 repo）→ 补 checkout 修复。**v0.1.1 实测发布成功**：[Release 页](https://github.com/lomehong/Nuwa/releases/tag/v0.1.1) 双平台二进制（nuwa-linux-x64 / nuwa-windows-x64.exe）。发版流程 = 改版本号 → `git tag -a vX.Y.Z && git push origin vX.Y.Z`。
- **生产部署达成（2026-10-16，腾讯云深圳 OpenCloudOS 9.4）**：v0.1.2 tag → Actions 构建产物（CN→GitHub 大文件下载被掐在 ~12MB，改走 **actions artifact 通道**（api 域名）完整拿到）→ SSH 部署 /opt/nuwa（bin + repo + systemd 双服务）。**serve 鉴权三态实测**（401/401/200）、work 领单循环 active、**预检护栏首次在生产拦截**（checkout 停在旧 agent 分支拒绝服务）。服务器 main 已注入回归（revert 提交），待安全组放行 7321 后从采集器发真实信号完成生产首单。**环境坑记录**（AGENTS.md 硬规则 6）：全局 target-dir 重定向到 C 盘 shared-target → E 盘 exe 陈旧遗留 + 运行中进程锁 exe + E 卷 mtime 停滞——本机"构建-执行"链路需按该规则操作。
- **生产形态首单达成（2026-10-16，腾讯云服务器）**：真实信号公网 POST（token 鉴权）→ 服务器领单 `63ad88c4` → glm-5.3-flash 修复（17272 tokens，自验含 GOCACHE 自适应）→ 验证门 → push（credential store + Contents:write）→ **[PR #6](https://github.com/lomehong/nuwa-spike-target/pull/6)**（state=open）→ target Actions 绿 → **服务器 ci gate 35 秒确认 all checks green → 留人工合并**。生产部署中实战暴露并修复：① systemd 无 HOME/GOCACHE（验证门 go test 失败）→ unit 显式注入；② 服务器无 git 凭证（clone 用的内嵌 token 不进 push）→ credential store；③ 交付失败残留工作树 → 人工清理（**待办：deliver 失败自动回滚 + 交付后自动 checkout main**）。服务器 main 保持回归态（revert 提交），作为生产闭环的常驻验证靶。
- **Web 控制台 v2 上线（2026-10-16，v0.1.4 部署至生产）**：四问题优先级设计（需要我/机器在干什么/健康/全量清单）。状态机漏斗（计数+点击过滤）、**五色石等级石**（docs/05 映射落地）、needs-attention 区（escalated/rejected 一键 reopen）、相对时间、repo 过滤、活动流（events 表）、flash 消息、全字段 HTML 转义、零 JS 框架。公网实测：无凭证 401、Basic 鉴权渲染真实数据。shanhaijing 新增 `recent_events`/`EventRow`。23 测试全绿、clippy -D 零警告。dev/生产双视图教训再确认：**所有本机修改必须经 git 提交 + 云端构建交付**（AGENTS.md 规则 6 已记录 shared-target 陷阱）。
- **设计实施落地（2026-10-16，v0.1.5 部署至生产）**：docs/09 定稿设计全量实现——**设置页** `/settings`（① 凭据掩码编辑 ② LLM provider/model/base_url ③ Runner 五参数 ④ 服务重启编排 `systemd-run`；保存写 env 文件 chmod 600 + config_changed 事件；work/serve env 默认链路打通 `NUWA_PROVIDER/MODEL/BASE_URL/INTERVAL/CI_TIMEOUT/DAILY_PR_LIMIT/MAX_TURNS/MAX_TOKENS`）；**详情页** `/issue/{fp}`（时间线/堆栈样本/PR 卡/reopen）；**G1-G5 数据层**（ci_green/ci_red/ci_timeout 事件、escalated detail 规范化前缀 repair_failed/delivery_failed/ci_red、merged 事件、work 心跳表+每轮 upsert、config_changed 审计）；**D2 独立 UI 令牌**（NUWA_UI_TOKEN fallback 上报令牌）；**交付加固**（push 耗尽回滚 main + 成功后自动归位）。39 测试全绿、clippy -D 零警告、v0.1.5 云构建部署冒烟通过。
- **Web 控制台 M0 热修（2026-10-17）**：五个任务修复六处症状——反射型 XSS（筛选参数转义）、rejected 无法 reopen、web reopen 不清冷却、设置页 env 键大小写（大写化 + 启动迁移 + `nuwa migrate-env` 部署命令）、配额待命不写心跳、旧 POST 无同源校验。自建 crate clippy 零 warning、56 测试全绿（nuwa 40 / shanhaijing 16，新增 17）；本地冒烟通过，生产冒烟待发布。
- 下一步：接入第一个真实系统（产品化）→ 并发池/镜像矩阵（A-3 后半）→ 验证探针协议（A-4）→ musl 静态构建消除 GLIBC 噪音。

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
