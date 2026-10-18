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
- **Web 控制台 M1a 数据与 Store（2026-10-17）**：`PRAGMA user_version` v1 迁移 + WAL/busy_timeout；新增 `runners`/`runs`/`signal_daily`；events 增加 repo/actor 归属；上报按日聚合、停止 dedup 事件刷屏；runner 注册/心跳与 run 生命周期 Store API（work 接线在 M1b）；卡单判定 + requeue/reopen、telemetry 保留清理仅 Store API（M1a 无调用方；prune 已由 M1b 启动时接线；requeue/reopen 调用方在 M2）；配额按 PR URL 去重。自建 crate clippy 零 warning、74 测试全绿（nuwa 40 / shanhaijing 34）；本地库副本 `user_version 0→1` 迁移后仍可读。
- **Web 控制台 M1b Runner 遥测接入（2026-10-17）——M1 完成（faux e2e + run_work 集成；未做真实 provider 实跑）**：Store 硬化（`start_run` 事务化、缺失问题单报错、`update_run` 拒写已结束 run、`mark_interrupted` 保留原错误、重启复位）；work 接线——runner 身份（`host:repo` 净化 + `--runner-id` 覆盖）与启动注册、后台心跳（`spawn_blocking`，5–15s，携 status/当前指纹/phase/last_error，旧 heartbeat 表双写，首次失败只记一次）、run 生命周期 start/update/finish/interrupted（启动复位残留 run；phase claim→repair→verify→deliver→ci；outcome 区分 repair_failed/budget_exceeded/delivery_failed/ci_red/success；修复/交付/CI 各失败路径均收尾 run）；遥测不阻塞修复/交付热路径——live_update 纯内存，心跳/进度/证据写经 `spawn_blocking`，进度回调 fire-and-forget；修复进度实时落 turns/uncached+cached tokens，进入修复即落确定性 session_dir，修复结束以累计值覆盖，交付后落验证证据（有界）与 diff stat，run 错误文本限 ~16KB；faux 端到端测试断言 run 阶段与 outcome、runner_id、turns≥1、session_dir、harness_run_id、验证证据（verification_detail/change_stat）、`agent/fix/*` 分支内容；新增 run_work 进程级集成测试（启动注册 runner、run 落库、重启把 pre-existing open run 标记为 interrupted）。自建 crate clippy 零 warning、87 测试全绿（nuwa 48 / shanhaijing 39，M1b 新增 13）；下一步 M2 API 与会话，随后 M3–M6（前端骨架/只读页面/动作与设置/加固切流）。
- **Web 控制台 M2a API 鉴权与路由地基（2026-10-17）**：单端口 `/api/v1` 与旧控制台共存——HMAC 会话 cookie（`nuwa_session` HttpOnly + `nuwa_csrf` 双提交，SameSite=Strict，TLS 反代自动 Secure）、`POST /api/v1/auth/login|logout` + `GET /api/v1/auth/me`、登录限速（5 次失败/10 分钟 → 429 + `Retry-After`）；统一 JSON 错误契约（401/403/404/429，`{"error":{"code","message"}}`）；全局安全响应头（nosniff / Referrer-Policy / X-Frame-Options DENY / Permissions-Policy；API 与 SPA 严格 CSP，旧控制台过渡期保留 `style-src 'unsafe-inline'`）；旧路由（`/ui`、`/settings`、`/issue/`、`/ingest`、`/health`）行为不变，Basic 鉴权与旧 POST 同源校验保留；`rust-embed` 静态占位 + SPA fallback（仅 GET/HEAD，缺失资源 404，debug 读 `web/dist`）。静态测试与 dist 内容解耦（M3 产出 `index.html` 后仍绿）。**最终评审修复（同日）**：API 响应统一 `Cache-Control: no-store`；登录限流在 `check`/`failure` 时清理所有过期 IP 桶；空 `NUWA_UI_TOKEN` 统一按本地模式处理；`/ingest` 与 `/health/*` 不再落入 SPA；空 CSRF cookie 由 `/auth/me` 重发；dotfile 与静态缺失测试硬化；`NUWA_SESSION_TTL_SECS` 收敛到 300–2592000 且本地模式非回环监听启动告警；登录 JSON 提取失败走 `invalid_request`/415 契约错误。自建 crate clippy 零 warning、136 测试全绿（nuwa 97 / shanhaijing 39，M2a 累计新增 49）。下一步 M2b 问题单只读 API、M2c 动作/系统 API 与前端对接。
- **Web 控制台 M2b 只读 API（2026-10-18）**：六个 GET 端点全部落地——`/api/v1/overview`（状态漏斗/需要处理/活跃 run/可领队列/runner 健康/24h CI）、`/api/v1/issues`（state/severity/repo/language/q/stuck/attention 过滤 + 五种排序 + 分页）、`/api/v1/issues/{fp}`（堆栈样本/最近 200 事件/最近 10 次 run/同名相似）、`/api/v1/runners`（心跳新鲜度/每仓当日 PR/effective 配置）、`/api/v1/events`（kind/repo/fingerprint/时间窗过滤、`aggregate=1` 按 fingerprint+UTC 小时聚合 `reported`、分组后分页）、`/api/v1/metrics/trends`（7/14/30/90d UTC 日桶 + 区间合计 + 中位开 PR 时长）。卡单窗口按 owner runner 的 `interval_secs` 推导（`interval_secs` 默认 30s；窗口 = max(3×interval, floor)，floor 默认 300s 且可用 `NUWA_STUCK_AFTER_SECS` 覆盖为 60–86400s，上限 72h），总览/列表/详情共用同一判定。补齐 carry-over：`sync` 与 CI-red 闸门写 `merged`/`rejected` 终态事件（趋势桶有真实数据）；趋势 `prs_opened` 对 `runs.pr_opened_at` 与 `issues.pr_opened_at` 按日/仓/PR URL 去重回退（迁移前 PR 不断崖）；详情 `similar.count` 为同名问题单总数（items 仍限 5）。新增六端点合一 local-mode 集成冒烟（temp DB + 种子数据，覆盖过滤/分页/404/no-store+严格 CSP/跨端点一致性）。自建 crate clippy 零 warning、182 测试全绿（nuwa 125 / shanhaijing 57，M2b 累计新增 46）。**最终评审修复（同日）**：总览 `?repo=` 的 `reports_today`/`ci_24h` 改为按仓聚合（新增 `reports_on_repo`/`count_events_since_repo`，旧 NULL-repo 事件回退 issue repo）；`NUWA_STUCK_AFTER_SECS` 作为卡单窗口下限覆盖（默认 300s，范围 60–86400s）真正生效；新单 `state_changed_at` 回落到 `first_seen`；`latest_run`/`runs_for` 以 `id DESC` 决胜；`sync`/CI-red 终态事件统一走 `journal_terminal` 并补 temp-DB 测试。最终 clippy 零 warning、186 测试全绿（nuwa 127 / shanhaijing 59）。下一步 M2c 动作/系统 API 与前端对接。
- **Web 控制台 M2c 动作、设置与系统 API（2026-10-18）**：写操作与治理端点全部落地——`POST /api/v1/issues/{fp}/reopen|requeue`（204；state 守卫 409、未知单 404、body 422；`reason` 接受但不落库，审计行固定为 `reopened by owner`/`stuck requeued by owner`）、`POST /api/v1/sync`（200 `{checked,merged,rejected,still_open,errors[]}`；进程内互斥 409、30s 超时/缺 token 502）、`POST /api/v1/runners/{id}/restart`（202；runner 存储的 `service_unit` 白名单校验，缺失/非法 422、systemd-run 不可用 501）、`GET/PUT /api/v1/settings`（22 键 schema、密钥只回显 `set/last4/updated_at_ms`、注释/顺序/未知键保留、原子写 + `.bak`、Unix 0600、逐键 `config_changed` 审计、小写已知键自动迁移）、`POST /api/v1/settings/test`（github `rate_limit` / zai / anthropic，`max_tokens=1`，空 model/base 视为未设置，anthropic 固定 base 且模型取自 vendored catalog，faux 离线）、`POST /api/v1/settings/restart`（活动 run 409 + `active_runs`、`force` 放行、units = serve + runner units、先审计后编排、非 systemd 501）、`GET /api/v1/system`（版本/构建/uptime/DB 路径与大小/本地模式/systemd 活性）、`GET /api/v1/issues/{fp}/transcript`（最新有 session 的 run 或指定 `run_id`；只解析 message 条目；逐块 16KB UTF-8 上限、tool_call 参数超限转预览串、2MB 尾部窗口、默认 20/最大 100 分页、无会话空页、跨指纹 404）。所有非 GET 仍强制双提交 CSRF（本地模式不豁免），API 读响应保持 `no-store` + 严格 CSP。**评审 carry 修复**：`sync` 对单个 PR 的 GitHub 查询失败改为 `error      : <fp> — <message>` 结果行并进入 200 响应的 `errors[]`（不再伪装成 `still open`）；`reconcile_all` 的 `Err` 现在只代表本地存储失败，映射 500 `internal`；缺 token 仍 502。新增 M2c 动作/设置/系统/转录合一 local-mode 集成测试（temp DB/env/session，覆盖 CSRF 403、reopen/requeue 204、sync 无 token 502、设置掩码/写回/审计、test 422、restart guard 409、system、转录分页/404/headers）。**最终评审修复（同日）**：`work` 启动即把 `--sessions` 根解析为绝对路径（修复失败/中断的 run 也保留可读 `session_dir`，不再被后续失败 run 遮蔽；docs/08 的 unit 示例改传 `/opt/nuwa/sessions`）；`/settings/restart` 与 `/runners/{id}/restart` 共享进程内重启互斥（并发 409，审计与编排都在锁内）且 `systemd-run` / `systemctl is-active` 有 3s 预算（超时 kill → restart 501、status `n/a`；无 shell）；`NUWA_COOKIE_SECURE=never` 优先于 `always` 与 `X-Forwarded-Proto`；systemd unit 白名单拒绝 `-` 前缀；`sync` 对 `NUWA_GITHUB_TOKEN`/`GITHUB_TOKEN` 去空白（纯空白视为缺失 → 502）；`/settings/test` 对每个已知键校验标量类型（字符串/数字/布尔；对象/数组/null → 422）；`.bak` 在 Unix chmod 0600（尽力而为）。最终 clippy 零 warning、257 测试全绿（nuwa 197 / shanhaijing 60，修复波新增 7）。
- **Web 控制台 M3 SPA 前端核心（2026-10-18）**：前端骨架落地——Vite 8 + React 19 + TypeScript strict + Tailwind 4（`tokens.css` 深/浅主题、自托管 Inter/JetBrains Mono、严格 CSP 无内联样式/脚本）+ TanStack Query + React Router。页面：`/login`（令牌登录/限速/错误文案）、`/` 总览（需要处理/运行中/健康/趋势/漏斗/最近活动）、`/issues`（URL 即状态：筛选/排序/分页，<768px 卡片化）、`/issues/:fp`（时间线/证据/运行记录/堆栈/相似问题/会话转录）；AppShell 桌面 232px 侧栏 + sticky 顶栏与健康条，<1024px 导航抽屉（焦点陷阱、Esc/遮罩关闭、导航后关闭、背景 `inert` + 滚动锁）+ 四项底部导航。错误与状态：可重试错误卡、传输失败离线横幅（自动恢复）、`/auth/me` 非 401 启动错误态（重试而非误判未登录）、401 清空 Query 缓存并回登录、logout-401 视为会话已过期、App 级错误边界、空态/skeleton/只读提示。无障碍：skip link 聚焦 `<main tabIndex={-1}>`、`aria-current` 导航、对话框背景 inert、转录分页保留旧页并聚焦页码标题、复制 toast 精简、`prefers-reduced-motion`；触控控件在粗指针下 ≥44px（`Button` 尺寸类加 `min-w-11`，页面内操作链接/折叠项同步 `min-h-11 pointer-fine:min-h-9`；细指针保留 36px 紧凑尺寸），390/1024/1440 三档布局、双主题与键盘流程已由 Task 8 的 Chrome/Playwright 实测覆盖（无横向溢出、无内联样式、0 console error）。构建：`npm run build` = typecheck + Vite，产物 `web/dist` 被 `rust-embed` 嵌入，`serve` 在 `/` 提供 SPA（旧 `/ui` 保留至 M6）；Vite dev 代理 `/api` 与 `/ingest` 到 7321 且保留 Host/Origin。前端 **136 测试全绿（28 个测试文件）**、typecheck/lint/build 零错误；本次无后端改动，Rust clippy 基线不变。docs/09 §12 中未纳入 M3 的全局 `/` 聚焦搜索快捷键、轮询失败指数退避、7/14/30/90 天趋势切换（当前固定 14 天）与逐 runner 健康卡推迟到 M4–M6。
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
