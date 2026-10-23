# 04 · 决策记录（ADR）

格式：编号 / 状态 / 背景 / 决策 / 理由 / 被否决的备选。日期均为 2026-10-15。

---

## ADR-001 · 基于 pi-rust fork 构建自用修复 Agent

- 状态：已接受
- 背景：需要一个嵌入自家系统、收集运行时问题并自动提 PR 的修复 Agent。对 pi-rust 做了五维成熟度评估（见 [02-upstream-assessment.md](02-upstream-assessment.md)）。
- 决策：fork pi-rust，裁剪为自用运行时。
- 理由：library-first + 薄核心（rpi-agent 3.4k 行）+ crash-resumable loop 恰好匹配嵌入修复场景；MIT 无授权风险；agent loop / provider 适配器 / 编码工具带 / 可恢复运行循环全部白拿，自建工作量收敛到采集层与 PR 工具链。
- 被否决备选：直接依赖 crates.io 发布的 rpi-* crate（上游 2 个月 51 版 API 不稳定，长期追平成本高；且 Nuwa 需要改运行时本身，如注入预算/护栏钩子）。
- 义务：保留 crates 补 `[lints]` + clippy 进 CI；上游留 remote 选择性 cherry-pick 修复。

## ADR-002 · 触发粒度：先聚合成问题单，再触发

- 状态：已接受
- 背景：原始设想"每个异常触发一次 agent"会制造 PR 噪音与成本失控。
- 决策：信号先指纹化聚合成问题单（同指纹去重、冷却窗口），问题单才触发 agent。
- 理由：同指纹 bug 修一次即可；聚合层天然承担"真 bug vs 噪音"的分辨职责。

## ADR-003 · 自主度：直接开 PR（非 draft）

- 状态：已接受
- 决策：验证通过后直接开 PR，CI 与所有者人工合并为闸门。
- 伴随义务（不可选）：分支命名空间隔离、频控/预算上限、禁改路径清单、复现验证证据五件套护栏。
- **补充（2026-10-20）**：默认仍为非 draft；新增每仓 `draft_first_n`（前 N 个 PR 可 draft）与 PR 风险画像，并记录误报 PR 率/review 延迟/CI 红率。未改为自动渐进放权——当前历史数据不足以支撑信任状态机。

## ADR-004 · 目标仓库全部为所有者私有仓库，源码可达

- 状态：已接受
- 决策：服务端统一持有 deploy key（权限仅覆盖这些仓库）；不涉及第三方仓库与外部贡献流程。

## ADR-005 · 运行位置：现场永远只采集，修复恒在服务端

- 状态：已接受
- 背景：初版设想 agent 嵌入各系统进程现场修复；后提出"现场具备条件则现场修、否则上传服务端"的两模式折中。
- 决策：两模式收敛为单模式——现场 = 薄采集 SDK（青鸟，按系统语言编写，不含 Rust 运行时）；修复恒在服务端运行器池（应龙）；现场仅保留下发**预定义验证脚本**的角色（应对依赖现场状态的复现场景），不运行自由 agent。
- 理由：生产现场通常只有构建产物没有源码；"具备源码+工具链的现场"本质就是 runner；集中部署让密钥面、预算闸、审计、升级只有一份。
- 被否决备选：现场内嵌 agent 运行时（资源/密钥/安全三重代价）；两模式并存（双倍部署形态成本）。
- 注：此决策同时回答了原"运行位置待定"问题（ADR 前置讨论中挂起的第 3 点）。

## ADR-006 · 语言无关：agent 本体零语言假设

- 状态：已接受
- 依据：已核实 pi-rust 工具面（read/write/edit/bash/powershell/grep/find/ls）为纯文件/文本/shell 操作，rpi-agent/rpi-harness/rpi-tools 无任何 cargo/rustc/maven/gradle 假设；edit 为精确文本替换 + 模糊回退。
- 决策：可修 Go / Java / Rust 等任何语言仓库。语言差异由外围承担：采集层按语言写堆栈指纹 parser 与事件采集 SDK；运行器按语言备工具链；每个目标仓库放指令文件（构建/测试命令、规范、禁改路径）自教 agent。
- 推论：新接入一个系统 = 装采集 SDK + 放指令文件 + 备 runner 工具链，agent 本体零改动。

## ADR-007 · 项目命名：女娲（Nuwa）

- 状态：已接受
- 决策：整体名**女娲 / Nuwa**（"补天"与"生成补丁修复系统裂缝"字面同构）；组件宇宙：采集 SDK = 青鸟（Qingniao），问题单库 = 山海经（Shanhaijing），补丁单元 = 五色石（Wuse Stone），修复 runner = 应龙（Yinglong）。
- 理由：中文神话中唯一的"修补创世神"，且能一次性命名完整个组件体系。
- 被否决备选：谛听 Diting（只诊不修，组件宇宙不完整）、扁鹊 Bianque（四诊=探针映射工整但为历史人物、拼写不友好）、大禹 Dayu（治水偏"控制"而非"修复"）。
- 项目文件夹：`E:\Development\Code\Rust\Nuwa`；Logo 见 [05-naming.md](05-naming.md)。

## ADR-008 · 从 pi-rust 保留/砍除清单

- 状态：已接受
- 保留：rpi-agent（agent loop，`#![forbid(unsafe_code)]`）、rpi-ai（8 个 provider 适配器，BYOK）、rpi-tools（bash/powershell/edit/read/grep/find/ls/write 工具带）、rpi-harness（crash-resumable 循环 + budget 钩子）。
- 砍除：rpi-tui（27k）、rpi-cli（48.5k）、rpi-extensions、rpi-plugin-sdk、rpi-telemetry——约 10 万行终端产品面。
- 依据：Nuwa 是"问题单进、PR 出"的无界面流水线；扩展机制用自定义 `AgentTool`（`pub trait AgentTool` + `AgentBuilder::tools`）而非 cdylib 插件 ABI。

---

## ADR-009 · Web 控制台 v3：SPA + JSON API + 会话鉴权 + runner 遥测

- 状态：已接受（2026-10-17）
- 背景：v2 三页 SSR 控制台已上线（v0.1.5），但实测暴露设置页 env 键大小写错误、反射型 XSS、无 CSRF、rejected 无法 reopen、配额待命被误判为死亡、全局单行心跳无法支撑多仓库、卡单无恢复路径等缺陷；同时需要会话转录、趋势与更好的运维交互。所有者决定采用 SPA 形态。
- 决策：
  1. 用 React 19 + TypeScript + Vite 单页应用替换 v2 SSR 三页；`nuwa serve` 托管静态资源与 `/api/v1` JSON API，前端产物 `rust-embed` 编入单二进制。
  2. 鉴权改为登录页 + HMAC 签名 HttpOnly 会话 Cookie；写操作使用 double-submit CSRF + SameSite=Strict + 同源校验；不再使用 Basic Auth。
  3. 一仓一进程不变；`nuwa work` 增加 runner 身份、后台心跳、run 生命周期与实时 turns/tokens 遥测；新增 `runners`/`runs`/`signal_daily` 表与 `events` 审计列。
  4. 卡单只识别（owner 心跳 + state_changed_at），由所有者一键 requeue；不自动重排。
  5. 先以 M0 独立热修线上 P0，再用一次大版本（v0.2.0）完成控制台替换；保留旧二进制/DB 备份与回滚演练。
  6. 设置页继续以 `nuwa.env` 为唯一事实源；规范化为大写键并在启动时迁移历史小写键；重启由 systemd 编排并有活动 run 保护。
- 理由：详情取证、会话转录、趋势与实时运行状态需要真正的客户端状态与组件化；JSON API 使前后端契约可测试；会话 Cookie 提供可退出、可轮换、可限速的鉴权模型；runner 遥测是"多仓库可观测 + 卡单处置"的前提。
- 伴随义务（不可选）：安全头与严格 CSP、CSRF、登录限速、密钥不回显；遥测写入 best-effort 不阻断修复；数据库迁移幂等并可回滚；Node 构建进入 CI/release；发布前完成生产库副本演练与回滚演练。
- 被否决备选：
  - SSR + HTMX：可减少 JS，但转录查看器、趋势交互与跨页状态仍会堆叠大量原生脚本，长期维护性不如组件化 SPA。
  - API 先行、UI 后置：后端契约更稳，但可见运维价值延迟，且旧 UI 可能被改两遍。
  - 保留 Basic + CSRF：无法优雅退出/限速会话，浏览器弹窗体验差。
  - 保留全局单行心跳：无法判断多仓库中哪个 runner 失效。
  - 自动 requeue 卡单：无法证明其他 runner 不在工作，存在双开分支/PR 的竞争风险。
  - 本期直接做多仓库并发池：属于 docs/07 A-3 后半，是独立的运行器架构项目，会稀释控制台交付。
- 取代关系：取代 docs/09 v2 的 D1（serve 内服务端渲染、零 JS 框架）；保留 D2（UI 令牌与上报令牌分离）与 D3（env 文件 + 重启编排）。

### 实施状态（Implementation status，2026-10-18）

- **已实现**：M0–M2（热修、数据层/遥测、`/api/v1` 与会话鉴权）、M3–M5（SPA 骨架、只读页面、动作与设置）、M6（命令面板/轮询退避/无障碍打磨、启动遥测保留清理、切流）。
- **旧控制台已移除**：`ui.rs` 与 Basic 鉴权删除；`/` 是唯一控制台，`/ui*` 308 到 `/`、`/issue/{fp}` 308 到 `/issues/{fp}`、`/settings` 为 SPA 设置页；全站严格 CSP。
- **版本 0.2.0**：`crates/nuwa` 与 `Cargo.lock` 已更新；未打 tag、未 push、未部署。
- **验证已完成（2026-10-18）**：axe WCAG 2.0/2.1 A+AA 26 个组合 0 violation；5k issues / 17k events 性能基准达标；回滚演练通过。生产库副本未提供，按计划以 5k 种子库替代（schema v1 兼容）。
- **发布门（仅待所有者授权）**：tag `v0.2.0`、push 与部署须由所有者明确批准后执行。

---

## ADR-010 · 交付验证：base 红 / 分支绿

- 状态：已接受（2026-10-20）
- 背景：v0 验证门只在 agent 改过的同一棵树上重跑 `failing_check`，不能证明修复前失败；测试文件可被改写；运行时 panic 往往不被单测覆盖。
- 决策：
  1. `nuwa.repo.json` 支持可选 `reproduce_check`，必须在干净 base worktree 上失败、在分支上通过。
  2. 未配置时，先跑 `failing_check`：base 红则作为复现；base 绿则要求分支提供自包含 `.nuwa/repro.sh`（Windows `.repro.cmd`），复制到 base worktree 验证红→绿；否则 escalate，不开 PR。
  3. `failing_check` 始终必须在分支上通过。
  4. 交付面用 `git diff --name-status` + glob 终检 `forbidden_paths`，并把测试/CI 变更写入 PR 风险画像。
- 被否决备选：
  - 只保留“分支绿”：零信息量，无法区分“修好了”与“测试本来就不覆盖”。
  - 一律要求 base 红：会让运行时 panic 全部升级，除非 agent 先提交复现测试；因此保留 reproducer 回退。
  - 直接拒绝所有测试变更：会阻止合法回归测试；改为风险画像 + 复现门。

---

## ADR-011 · 状态变更 webhook（窄范围）

- 状态：已接受（2026-10-20）
- 背景：无人值守系统没有门铃；escalated/pr_open/CI 红/runner 挂死只能靠所有者主动打开控制台。
- 决策：提供一个 owner-configured HTTP webhook，发送 Nuwa 自己的生命周期事件（escalated/ignored/provider_stand_down/heartbeat_stale/ci_red/sync_error/pr_opened），JSON + 可选 HMAC-SHA256，10s 超时、3 次退避重试。serve 与 work 都可发送。
- 理由：这是产品闭环的“呼叫所有者”，不是监控指标采集/通用告警路由；事件集合固定、无第三方 SDK、无路由/抑制/分级。
- 被否决备选：Slack/飞书专用集成（耦合渠道）；通用告警路由（违反 docs/01 非目标）；只靠控制台轮询（发现时机取决于所有者）。

---

## ADR-012 · `ignored` 终态与 attempt cap

- 状态：已接受（2026-10-20）
- 背景：rejected 同时承载“PR 被拒”和“owner 判定不是 bug”；噪音指纹会走 rejected → 回归重开 → 再修；无 attempt 上限。
- 决策：
  1. 新增 `ignored` 终态；owner 可对 new/escalated/rejected 执行 ignore（带原因），reports 只计数不自动重开。
  2. issue-level 失败累计 attempts；达到 `max_attempts`（默认 3）自动 ignored；provider 瞬时故障和环境故障不计数。
  3. reopen 由 owner 显式执行，重置 attempts 并把 reason 写入事件，下一轮 prompt 作为 OWNER NOTE。
  4. 优先级改为 severity 字典序优先，occurrences 只做同级排序。
- 被否决备选：复用 rejected + muted 标记（状态语义混淆）；无上限自动重试（成本不可控）；把 provider 故障计入 attempts（会让基础设施抖动消耗业务单）。

## ADR-013 · 问题单身份按仓库限定（fingerprint 纳入 repo）

- 状态：已接受（2026-10-21）
- 背景：`fingerprint = sha256(lang|error_class|top3 frames)`，issues 表以 fingerprint 为主键。两个仓库若共享同一全限定符号路径与错误消息（fork、vendored 副本、同一服务两个登记身份），同栈信号会合并成一行：后报的仓库被去重、无法被自己的 runner 领取，且 ignored/escalated/attempts/stack_sample 互相污染。实测 A、B 两仓同栈 → B 永远 claim 不到。
- 决策：把 **repo 作为 fingerprint 哈希的第一段输入**（`sha256(repo|lang|error_class|frames…)`）。fingerprint 仍是单一主键，URL/runs/events/schema 全不变，值变为按仓库限定。`claim_ready_issue_for_repo` 的 repo 过滤因此从“必需”变为“冗余但保留”。
- 跨仓契约：青鸟 SDK 的 `panicFingerprint` 必须同步把 `Meta.Repo` 作为首段输入并重钉 golden 常量（shanhaijing 侧 golden 已更新为 `5cc9dae18baa7dac8582fda50dedec8d`，repo=`example.com/spike-target`）。**运行时不受影响**：服务端始终重算指纹、Signal 不携带客户端指纹，客户端指纹只用于其自身节流；SDK 未跟进前只是两端 golden 互验暂时失效，不影响聚合正确性。
- 被否决备选：① 复合主键 `(repo, fingerprint)`——需迁移 + 改 `/issues/{fp}` URL 方案 + runs/events 归属 + web 路由，blast radius 过大；② 仅按 repo 过滤 claim 不改身份——问题单仍合并，状态/样本污染未解决。

## ADR-014 · 授权对 vendored rpi-* 就地打补丁（突破 AGENTS.md 硬规则 1）

- 状态：已接受（2026-10-21，所有者明确授权）
- 背景：全仓审查发现 5 个真实缺陷位于 vendored 的 `crates/rpi-*`，其中 os_env 大输出管道阻塞在正常运行即可触发（Q6 同类挂死）。AGENTS.md 硬规则 1 要求 vendored 保持原样、走上游 cherry-pick。
- 决策：所有者授权**就地修补**这 5 处（os_env 并发驱动 stdout/stderr、settle 关键持久化失败传播、agent_harness 预算耗尽明确 outcome、compaction 继承 stream options、read 有预算流式读取）。每处改动加 `// Nuwa 本地补丁(2026-10-21): <原因>` 注释，保持公开签名稳定，带回归测试。
- 约束：① 仅限这 5 处真实 bug，不借机重构/美化 vendored；② 与 upstream 对账时这些本地补丁是已知差异，未来 cherry-pick 上游修复时须保留本地语义；③ docs/07 §D 记录补丁清单。
- 被否决备选：全部走上游 PR（周期长，os_env 挂死在生产持续发生）；仅 Nuwa 层规避（bash 工具在 vendored 内，无法完全规避）。

## ADR-015 · 修复流水线单一入口、决策可审计与上下文预算

- 状态：已接受（2026-10-22）
- 背景：常驻 runner（`work_once`）与一次性 `nuwa fix` 各自拼装修复任务与交付参数，已出现真实漂移——CLI 路径不注入仓库 notes、不脱敏外部输入。claim→repair→deliver→CI 多步流程只留结果事件，没有“为什么”的结构化记录；成功修复的经验不成体系；token 数字看不到上下文窗口与压缩次数。
- 决策：
  1. `pipeline.rs` 是唯一修复/交付实现；两条入口只负责寻址、记账与失败分类。问题单构建、脱敏、技能选择、会话目录、交付参数都从这里走。
  2. 版本化修复技能：内置技能随二进制分发（rust-embed），仓库 `.nuwa/skills/*.md` 同名覆盖；选择是确定性的（语言 + 失败命令打分，或 `skills` 白名单），有数量/长度上限，注入 runner 系统提示。
  3. 决策 trace：kind=`decision` 的事件，detail 为 `{stage, decision, reason, details}` JSON，覆盖 claim/preflight/repair/budget/deliver/ci；控制台渲染为一行可读文本，原始 JSON 保留。
  4. 修复记忆：仅在独立验证门通过且 CI 绿灯（或明确 branch-only 已推送）后写入 `repair_memory`；带 commit/PR/时间来源；同一 (repo, fingerprint) 只保留最新，每仓上限 50；注入下一轮 `PRIOR ATTEMPTS` 之后，仅限同仓且排除当前问题单。
  5. 交付后验证：推送后用 `git ls-remote` 校验远端 tip == 已验证提交；PR 基线在验证后移动时，合并一次并重新过红绿验证后再推；合并冲突则中止交付、交人工 rebase。
  6. 上下文预算：runs 表记录 `compactions/context_window/messages`，repair 报告同一组数字；压缩次数来自 harness 的 CompactionProgress 事件（vendored `events.rs` 补类型化订阅实现，属 ADR-014 授权范围）。
- 已知盲区（诚实记录）：harness 未对外暴露逐条消息淘汰、压缩摘要长度与“当前上下文 token 占用”；`estimate_context_tokens` 也不对调用方开放。因此控制台只承诺“压缩发生了几次、模型窗口多大、结束时多少条消息”，不承诺实时上下文占用百分比。
- 被否决备选：两条入口各写一套（漂移已经发生）；把技能选择交给模型（不确定、不可回放）；把全部历史修复注入提示（污染上下文且无来源）；无条件合并基线（会把未验证的树推给 PR）。

## ADR-016 · 修复基准：执行在 CLI/CI，控制台只读结果，指标必须包含误报

- 状态：已接受（2026-10-23）
- 背景：技能、记忆、决策 trace、上下文预算都让修复过程可审计，但仍没有可比较的质量数字——“改完之后到底修得更好还是更差”无法回答。只看通过率又会自欺：模型声称修好、检查仍红的情况必须被单列。
- 决策：
  1. **固定场景内嵌**：`assets/benchmarks/<scenario>/` 含 `scenario.json` 与种子仓文件；场景声明 `failing_check`、预期是否应修复、确定性 faux 写入与最终消息。内置 4 个正场景 + 1 个负对照（只写说明但声称修好）。
  2. **执行面与展示面分离**：`nuwa benchmark run` 在独立进程运行（`faux` 默认，`live` 需真实 provider 密钥）；serve 不启动模型循环，控制台只读结果。原因：provider 密钥、全局 turn 预算环境变量、进程隔离都不适合放在 HTTP 服务里。
  3. **指标**：逐场景记录 `repaired`、`claimed_fixed`、`delivery_ok`、turns/tokens/耗时；误报定义为 `claimed_fixed && !repaired`。基准必须报告误报率与负对照结果，不能只报通过率。
  4. **隔离**：每个场景复制到独立临时 git 仓；Windows 场景可覆盖启动器命令；Rust fixture 自带 `target-dir`，避免开发机的全局 cargo 配置污染基线。基准不触碰用户仓库。
  5. **持久化与展示**：结果写 `benchmark_runs`/`benchmark_results`（schema v6）；控制台 `/benchmarks` 展示趋势、运行表、场景目录与逐场景证据，`/benchmarks/:id` 显示误报、预期偏差与门禁拦截。
  6. **顺带修复**：基准首次运行暴露 Windows 子进程环境白名单按大小写精确查找 `SystemRoot` 的缺陷（Git Bash 导出 `SYSTEMROOT` 时被丢弃，Node 测试子进程在 crypto 初始化崩溃）；白名单改为大小写不敏感并回填规范键名。
- 已知边界：内置场景只有 5 个，覆盖四类工具链的典型失败与一个负对照；它证明“流水线与门禁不退化”，不能替代真实仓库的多样化故障。`live` 结果受模型随机性与成本影响，比较时应固定 provider/model/场景集。
- 被否决备选：控制台按钮直接跑基准（密钥/进程/预算边界不清）；只报通过率（无法发现误报）；接入外部基准服务（超出单所有者部署边界）；把 faux 结果当模型质量（faux 只证明流水线，不证明 LLM）。

---

## ADR-017 · dev 构建关闭调试信息（磁盘 IO 止血）

- 状态：已接受（2026-10-21，2026-10-23 复核并入场）
- 背景：本工作区 7 个 crate、约 40 个集成测试文件，dev profile 全量 debuginfo 下 MSVC 链接器为**每个测试二进制**写一个 PDB。实测 `target/` 里曾有 33 个 >50 MB 的 PDB、合计 5.84 GB，单文件最大 295 MB。编辑 rpi-tools / rpi-telemetry 这类底层 crate 会触发**全部 ~46 个测试二进制重链接**，一次落盘以 GB 计；而构建盘与系统盘是同一块 NVMe，写入突发直接挤占系统 IO → 整机冻结（2026-10-21 至 10-23 共四次强制关机）。已三层实证排除安全软件方向（DLP 白名单不含 cargo/rustc、受控重建期间其 IO 占比 0.01%、无 minifilter/目录监视、Defender 实时保护关闭、系统日志无磁盘错误）。
- 决策：工作区根 `Cargo.toml` 固定 `[profile.dev] debug = 0`。需要完整回溯时按命令临时覆盖：`$env:CARGO_PROFILE_DEV_DEBUG="2"; cargo test -p <crate> --bins`，用完即清。备选 `debug = "line-tables-only"`（保留 panic 行号、PDB 仍小一个数量级）保留为后续可选项。
- 理由：开发期几乎不需要可变级别调试（问题靠测试断言与日志定位），而链接写入是这台机器唯一的硬故障源；关闭后单个测试二进制 PDB 从 159 MB 降到 15.6 MB。
- 实测（2026-10-23，冷缓存）：`cargo clean` 清掉 17158 个文件 / 13.7 GB（9.5 s）；随后 `cargo test --no-run -j 2` 全量通过；`target` 内 >50 MB 的 PDB **0 个**，154 个 PDB 合计 671.6 MB，最大 36.6 MB；`touch crates/rpi-tools/src/lib.rs` 后 `cargo test -p rpi-tools --lib` 72 通过（4.67 s）。
- 已知边界（不是回归）：`touch` 底层 crate 仍会重链接全部测试二进制，即使 `debug = 0` 也是一次 GB 级写入、NVMe 上数十秒磁盘 100%——这是物理事实。**在这台开发机上，全量重建（`cargo clean` + `--workspace`）仍会整机冻结，因此全量构建与 clippy 一律交给 CI，本机只做单 crate 增量构建。** 仍卡顿时用 `cargo test -j 1`，或按 `-p` 分批。
- 被否决备选：只在本地 `.cargo/config.toml` 覆盖（clone/换机/CI 即丢，团队照踩）；改 `[profile.release]`（与本问题无关）；加 `sccache`（不减少最终落盘，且引入新依赖）。

---

## ADR-018 · runner 单实例租约（同 runner-id 互斥）

- 状态：已接受（2026-10-23）
- 背景：`revive_orphaned_fixing` 按 `owner_runner_id` 复活启动前的在途单；两个**同 id** 的活进程会在各自启动时把对方的活单当孤儿回收，重复修复并双推同一 `agent/fix/*` 分支。claim 的原子 owner 只能挡住异 id 抢占，挡不住同 id 复活。
- 决策：schema v8 在 `runners` 增加 `instance_id` / `lease_until`。`nuwa work` 启动时以随机实例 id 在同一事务内完成「租约检查 + runner 注册」：租约被其他实例持有则等待至多 TTL+10s（覆盖崩溃重启），仍失败则拒绝启动；心跳每 5–15s 续租；续租失败（被接管）以退出码 0 终止，让新持有者继续而不是让 systemd 再起第三个竞争者；正常退出或收到 SIGTERM 时释放租约（否则 systemd 重启要等满 TTL）。`revive_orphaned_fixing` 语义不变，由租约保证它只在独占时运行。
- 边界：这是单写者**前置条件**，不是分布式锁——不做 fencing token；进程长时间暂停/分区后可能出现短暂重叠，由续租失败退出收敛。租约 TTL 60s，崩溃后最迟一个 TTL 可被接管。
- 被否决备选：仅比较 heartbeat 新鲜度（启动时刚写入的心跳无法区分死进程与新进程）；见到同 id 直接失败不等待（崩溃重启会与自己的旧租约冲突，systemd 反复失败）；引入外部锁服务（单机单库部署，超出边界）。

---

## ADR-019 · 修复事实的一致性与安全发布边界

- 状态：已接受（2026-10-23）
- 背景：配置刷新与基线更新错序、异步 usage 未排空、状态与审计分开提交，会使护栏依据的事实与实际执行不一致；运行中原始转录与内存会话撤销还存在跨进程生命周期缺口。
- 决策：
  1. runner 在 fetch/reset 后刷新权威配置再领单；若新 base 开始跟踪指令文件，其优先级高于旧私有副本，解析成功后才切换来源。基线移动后的重验与风险画像使用同一个新 base SHA。所有网络 Git 操作必须有截止时间。
  2. 领取及审计、信号聚合及自动回归、PR 拒绝及失败计数分别原子提交；自动回归不得复用允许 owner 重开 ignored 的通用状态迁移。拒绝必须同时匹配当前状态与预期 PR URL，重复或跨轮陈旧结果不累计 attempts；退避从故障发生时计时。
  3. 最终 token 账目必须等待已产生的 usage 事件处理完成，不能通过固定延迟猜测完成，也不能用旧快照覆盖已记录进度。
  4. serve 只读取 runner 已发布的脱敏转录快照，不读取原始 JSONL，也不接收 work 的密钥。成功或失败结束均尝试发布；发布失败明确告警，但不改变修复业务结果或消耗额外 attempts。未完成、被中断、发布失败或未生成快照的历史 run 不回退展示原始日志。保留原始私有证据，脱敏快照逐文件原子发布；字符串、键名及以数字表示的已知凭证均须脱敏。
  5. 控制台会话绑定 serve 启动代际。每次重启均要求重新登录，即使 UI token 未改变；以此保证内存撤销表丢失后旧 Cookie 也不能恢复有效。
  6. 修复最终报告新增 `OUTCOME: fixed|not_fixed|unverified`，与 ROOT CAUSE、CHANGE、VERIFICATION 构成四行契约。基准只把唯一且合法的 `OUTCOME: fixed` 视为成功声明；实际 repaired 仍由独立检查决定，缺失、冲突或未知声明不猜测为成功。
- 取舍：转录不再承诺实时展示原始内容，重启增加一次登录；换取不跨进程传播密钥、不新增会话数据库迁移。现有单二进制、SQLite 与人工合并边界不变。
- 指纹兼容性：修正 Rust 标准 backtrace 函数帧解析后，受影响信号会得到新指纹；不重写已有问题单、PR 与审计身份。历史记录保留，Rust 客户端若固定了对应 golden 值需同步更新；Go golden 不变。
