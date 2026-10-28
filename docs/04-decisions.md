# 04 · 决策记录（ADR）

格式：编号 / 状态 / 背景 / 决策 / 理由 / 被否决的备选。日期均为 2026-10-15。

当前批次快照（2026-10-27）：main 基线 v0.17.0；Batch5 observations/schema v13 已本地实现并完成聚焦验证（ADR-027）；Batch6 reservations/schema v14 已本地实现，最终检查完成（2026-10-28，ADR-028）。未升版、发版或执行生产操作；前序 ADR 的 schema 与验证数字保留为各批时点记录，不代表当前完整验证或生产升级已完成。

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
- 决策：schema v8 在 `runners` 增加 `instance_id` / `lease_until`。`nuwa work` 启动时以随机实例 id 在同一事务内完成「租约检查 + checkout 占用检查 + runner 注册」：租约被其他实例持有则等待至多 TTL+10s（覆盖崩溃重启），仍失败则拒绝启动；**不同 runner id 也不能共用同一份 canonical checkout**（两个进程会互相 reset/fetch），直接拒绝并给出占用者 id。心跳每 5–15s 续租；续租失败（被接管）以退出码 0 终止，让新持有者继续而不是让 systemd 再起第三个竞争者；正常退出或收到 SIGTERM 时释放租约（否则 systemd 重启要等满 TTL）。`revive_orphaned_fixing` 语义不变，由租约保证它只在独占时运行。
- 边界：这是 schema v8 的单写者**前置条件**，当时不做 fencing token；租约 TTL 60s，过期后可被接管。**Batch1 补充（2026-10-27，ADR-023）**：schema v10 的 delivery/checkout 操作新增 token、generation 与 expiry 校验；runner checkout 还绑定实际实例。不能再把“续租失败后退出”当成交付与清理的唯一守卫，也不因此承诺外部副作用 exactly-once。
- 并发拓扑：同一 SQLite 队列支持两种形态——多进程（不同 `--runner-id` + 独立 clone）与单进程池 `--pool N`（#1 主 checkout，#2..N detached worktree；每 worker 独立 id/租约/心跳/trusted，共享 DB 与 fetch 锁）。单进程池是异步并发，不是 OS 进程隔离；Batch1 的保守 teardown 与中断残留人工复核规则见 ADR-023，不自动清除现场。镜像矩阵已提供 Dockerfile（base/go/rust/node/python，CI 构建验证）；registry 推送是所有者动作。
- 被否决备选：仅比较 heartbeat 新鲜度（启动时刚写入的心跳无法区分死进程与新进程）；见到同 id 直接失败不等待（崩溃重启会与自己的旧租约冲突，systemd 反复失败）；引入外部锁服务（单机单库部署，超出边界）。

---

## ADR-019 · 修复事实的一致性与安全发布边界

- 状态：已接受（2026-10-23）
- 背景：配置刷新与基线更新错序、异步 usage 未排空、状态与审计分开提交，会使护栏依据的事实与实际执行不一致；运行中原始转录与内存会话撤销还存在跨进程生命周期缺口。
- 决策：
  1. runner 在 fetch/reset 后刷新权威配置再领单；若新 base 开始跟踪指令文件，其优先级高于旧私有副本，解析成功后才切换来源。基线移动后的重验与风险画像使用同一个新 base SHA。所有网络 Git 操作必须有截止时间。
  2. 领取及审计、信号聚合及自动回归、PR 拒绝及失败计数分别原子提交；自动回归不得复用允许 owner 重开 ignored 的通用状态迁移。拒绝必须同时匹配当前状态与预期 PR URL，重复或跨轮陈旧结果不累计 attempts；退避从故障发生时计时。
  3. 最终 token 账目必须等待已产生的 usage 事件处理完成，不能通过固定延迟猜测完成，也不能用旧快照覆盖已记录进度。
  4. serve 只读取 runner 已发布的脱敏转录快照，不读取原始 JSONL，也不接收 work 的密钥。成功或失败结束均尝试发布；运行中每 3s 节流发布一次，结束再发布最终版。发布失败明确告警，但不改变修复业务结果或消耗额外 attempts。未完成、被中断、发布失败或未生成快照的历史 run 不回退展示原始日志。保留原始私有证据，脱敏快照逐文件原子发布；字符串、键名及以数字表示的已知凭证均须脱敏。
  5. 控制台会话绑定 serve 启动代际。每次重启均要求重新登录，即使 UI token 未改变；以此保证内存撤销表丢失后旧 Cookie 也不能恢复有效。
  6. 修复最终报告新增 `OUTCOME: fixed|not_fixed|unverified`，与 ROOT CAUSE、CHANGE、VERIFICATION 构成四行契约。基准只把唯一且合法的 `OUTCOME: fixed` 视为成功声明；实际 repaired 仍由独立检查决定，缺失、冲突或未知声明不猜测为成功。
- 取舍：转录不再承诺实时展示原始内容，重启增加一次登录；换取不跨进程传播密钥、不新增会话数据库迁移。现有单二进制、SQLite 与人工合并边界不变。
- 指纹兼容性：修正 Rust 标准 backtrace 函数帧解析后，受影响信号会得到新指纹；不重写已有问题单、PR 与审计身份。历史记录保留，Rust 客户端若固定了对应 golden 值需同步更新；Go golden 不变。

---

## ADR-020 · 无人值守闭环：失败收敛、交付幂等与持续 CI

- 状态：已接受（2026-10-24）
- 背景：修复进程只在一个有限窗口内消费 CI；PR 创建响应丢失时可能误删远端分支；issue-level 失败一律升级、attempt cap 等下一次领取才生效，无法形成稳定的无人值守策略。所有者的 token 套餐不要求新增预算控制，现有预算闸保留但不在本决策范围内扩展。
- 决策：
  1. **失败策略统一入口**：issue-level 修复/验证失败原子累加 attempts；低于 `max_attempts` 时回 `new` 并设置冷却，达到上限立即 `ignored`。provider 瞬时故障、环境故障不计数；人工关闭 PR 仍进入 `rejected` 且不自动重试；provider 配置错误仍升级。状态、attempts、冷却与审计同事务。
  2. **交付 intent 与恢复**：推送前持久化 `delivery`（repo/branch/base/head SHA/run），依次记录 pushed、pr_requested、pr_created。PR 创建失败时先查 GitHub，只有 open PR 的 head/base 精确匹配才采用；查询不确定则保留分支并标记 ambiguous。`nuwa work` 启动及每轮领单前对账，恢复“GitHub 已成功、本地未落库”的断点；这是减少重复副作用的恢复机制，**不保证 exactly-once**。schema v10 的 claim、原子完成与条件孤儿删除以 ADR-023 为准；已关闭/不匹配 PR 不等于零 PR。
  3. **持续 CI（schema v9 原始决策）**：新增 `ci_watches`，由 serve 单实例后台调度；watch 持久化 head SHA、CI 结论、下次检查、退避与记忆来源，重启后继续。green 不是终态，继续观察重跑。PR 仍 open 且 head 未变转红时关 PR 并按失败策略计数/重试/忽略；PR 已 merged 或已关闭时只记录 `ci_red` 告警，不自动重开、不自动回滚。CI 不可读/未完成不是终态，指数退避并保留 watch；超过 30 天过期。**schema v11 引入且在当前 v14 继续有效的 SHA 快照、共享租约、终态和 CI 记忆撤销契约以 ADR-024 为准**，不能沿用旧实现把 closed+green 当作可发布记忆；运行期观察的证据撤销见 ADR-027。
  4. **控制台**：问题详情透出 watch 状态、最近检查、下次检查和错误；只读，不提供手工推进 watch 的按钮。
  5. schema v9 新增 `deliveries`、`ci_watches`；所有恢复/收敛写操作走 Store 事务。
- 取舍：持续 CI 依赖部署中的 serve 常驻；不做外部队列、分布式锁、自动回滚或自动重开已合并问题。CI 重跑导致已合并后转红只能告警，根因/部署版本关联仍是后续能力。
- 被否决备选：把 CI 观察绑在修复进程（超时即失明）；PR 创建失败一律删远端分支（会丢失已存在 PR）；issue-level 失败一律人工升级（无法无人值守）；自动重开已合并问题（无法回滚已合入代码）。

---

## ADR-022 · 运行期配置不改进程环境（env_store）

- 状态：已接受（2026-10-26）
- **Batch3 修订（2026-10-27，ADR-025）**：本条的“不再运行期改 OS env”继续有效；以下“保存更新 store / 即时生效”仅为当时实现的历史记录，现已撤销。保存只更新文件与版本元数据，不改启动 `env_store`、凭证或鉴权状态，全部 schema 项均需重启相关进程。
- 背景：`setenv`/`unsetenv` 与 `getenv`、`environ` 迭代、`fork`/`exec` 并发在 glibc 下不保证线程安全；测试与设置页的运行期 env 改写会与派生的 `git`/`go`/`node`/`cargo` 子进程竞争。表现为**间歇挂死**（同进程并行测试），挂死后内层 `timeout` 与 CI step/取消都难以收尾（docs/07「CI 测试卡死」，2026-10-26 探针复现）。
- 决策：新增 `env_store`（`RwLock` 单例）。`main()` 在任何线程派生之前**单线程**快照 OS 环境，并由 `Secrets::scrub_os_env()` 擦除已知密钥的 OS 副本；此后运行期读取走 `env_store::var/vars`（未覆盖的键回退 OS 环境），设置保存只写 `nuwa.env` 并更新 store（不再 `set_var`），测试用 `env_store::set` 而非 `std::env::set_var`。`Secrets::load()` 从快照读取密钥；子进程白名单由 store 快照 + `SAFE_ENV` 白名单构造（密钥不在白名单内，不会下发）。
- 取舍：设置保存对运行中进程**即时生效**（旧实现只写文件、要重启才生效——这是顺带修正的行为）；`env_store` 是进程内单例，测试写入仍自行串行（保留 `env_guard`）；不再支持「运行期改 OS env 让第三方库看到」（本来也不安全）。
- 被否决备选：给所有 env 读写加全局 RwLock（第三方库与子进程派生不可控，覆盖不全）；仅测试用 `serial_test`（生产设置页写入与 repair 派生仍有同一竞争）。

---

## ADR-021 · 验证门深检三角（Go vet/build）

- 状态：已接受（2026-10-26）
- 背景：ADR-010 的复现门解决“能否证明 bug 修好”，但配置的 `go test ./...` 不覆盖 main 包运行时路径与全部 vet 分析；agent 的改动可能让构建或静态检查退化而 red/green 检查仍然通过。docs/07 的 mindloop 案例即 `go test` 绿、main 运行时 panic 未被覆盖。
- 决策：问题语言为 `go` 且干净 base worktree 根有 `go.mod` 时，验证门额外运行 `go vet ./...` 与 `go build ./...`，各自独立超时（复用 `failing_check.timeout_secs`）。base 绿 → 分支必须绿，否则拒绝交付；base 红/超时/工具不可用 → 跳过并在证据中注明（默认分支已坏，不能归因本次修复）。深检不替代复现门：base 绿且无 `reproduce_check` 时仍要求 `.nuwa/repro.sh`。结果写入 `verification_detail` 与 PR 正文。
- 边界：只对根 `go.mod` 的 Go 仓库自动启用；无根 `go.mod` 的多模块仓不猜测命令。其他语言暂不自动推导（`cargo check`/`npm build` 的仓库差异过大），需要时用 `reproduce_check` 显式配置。
- 被否决备选：对所有语言自动推导构建命令（误报率高）；把深检当复现门（vet/build 不能证明运行时 bug 被修复）；base 红也强制分支绿（把既有债算到本次修复）。

---

## ADR-023 · Batch1：精确交付所有权、原子完成与保守清理

- 状态：已接受（2026-10-27）；Batch1/schema v10 已完成，下述结果是该批完成时的历史验证快照；Batch2/schema v11 见 ADR-024，Batch3/schema v12 见 ADR-025，Batch5/schema v13 见 ADR-027，当前 Batch6/schema v14 见 ADR-028。代码基线 v0.17.0，不代表本批已发布，未执行生产迁移或部署。追溯：DES-023 → REQ-005、REQ-008（docs/10）。
- 背景：runner id 不区分进程代际；独立 fix、恢复任务与活跃交付可能竞争同一 checkout/intent。PR 响应丢失、基线合并改变 head、完成记账分步写入，以及无条件分支清理会误收养结果、留下半完成状态或删除他人的证据。ADR-018/020 的原有租约与恢复约束不足以覆盖这些边界。
- 决策：
  1. **schema v10 所有权**：delivery claim 使用随机 token、递增 generation 与 expiry；领取原子仲裁，续租/状态迁移/完成须匹配当前有效 claim。canonical checkout 有独立 claim，与 runner 占用互斥；runner 路径还校验实际存活的 `instance_id`，同名 id 不能继承后继实例的权限。独立 `nuwa fix --db` 默认 `shanhaijing.db`，与 work 共用同一 Store/数据库，使用自己的 checkout claim，不伪造 runner 身份；租约覆盖修复和网络等待。不同数据库不能协调同一 checkout。
  2. **有界恢复与续租**：pending intent 在 Store 内原子领取，恢复调用每次最多一条，不预领一批后串行等待。delivery/checkout claim 的 TTL 60s，等待期间每 10s 续租；等待前后及副作用边界重新检查权限，续租失败或失权则停止后续副作用/写回。已经发出的远端请求可能仍生效，不宣称可撤回。
  3. **精确 head 与原子完成**：基线合并产生新 head 时，旧 intent 原子进入 `superseded` 并领取新 head 身份，保留旧 head 历史而非覆写。创建/采用 PR 后，delivery、关联 issue/run 和 CI watch 在同一 Store 事务内完成；独立 fix 无关联 issue/run 时不虚构记录。仅 open 且 head SHA/base 精确匹配的 PR 可以采用。
  4. **默认保留、条件删除**：只有查询明确证明零 PR，且仍拥有交付权限，才允许以 expected SHA + `force-with-lease` 条件删除孤儿远端 ref；tip 改变或删除结果不明仍保留证据。PR 已关闭、head/base 不符、响应畸形、分页不完整及任何未知结果都不是“无 PR”，不得据此删分支。本地清理必须同时持有 checkout，且分支与 tip 符合预期；失权/不匹配不强制 reset 或删除。
  5. **收紧自动维护**：脏树 preflight 拒绝，不再自动破坏性 reset/clean；stale/terminal 分支自动清理禁用，保留给所有者复核。此项覆盖此前自动恢复/终态扫除策略，不取消有完整所有权与 branch/tip 证据的交付局部清理。变更判定与 staging 的 `:(exclude)nuwa.repo.json` 契约不变。
  6. **直接交付路径可测**：PR 正文在内存构造并直接传给 GitHub，不写 `.git` 临时文件；测试替换实际交付路径的 GitHub 接口，Git 使用真实本地 bare remote 和 linked worktree（`.git` 为文件），覆盖响应丢失、崩溃、续租/接管、supersede、完成回滚及条件清理，而非仅旁路 helper。
  7. **detached 池工作树保真**：保留逻辑 PR base，交付后安全恢复到预先固定的 detached HEAD，而不是切到 base 分支。pool teardown 保留脏树或附着分支的 worktree 证据，只对干净且 detached 的 worktree 调用不带 `--force` 的 `git worktree remove`；进程骤停可能留下 worktree，须由所有者人工复核，不能自动清除现场。
  8. **仅推分支的持久语义**：branch-only 路径在 push 前持久写入 `branch_only`，排除在 PR 自动恢复之外；崩溃后由所有者人工复核，不因没有 PR 自动删除分支。只有 push 成功且经验证、checkout 安全恢复后，才进入终态 `branch_pushed`。
- 升级契约（Batch1 历史 v9 → v10；当前升至 v14 见 ADR-028/docs/08）：必须由所有者停止全部旧写者及自动重启入口，确认无在途写入，一致备份 DB/WAL 与 env 后再由新版打开 Store。迁移后的旧 intent 无 owner、可以被领取，**仅在停旧写者升级的前提下安全**；不支持新旧写者混跑，不把无 owner 等同于无人执行。生产迁移尚未进行。
- 验证状态（Batch1 历史本地限定范围）：`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；`cargo test -p shanhaijing --offline` **132 通过**；`cargo test -p nuwa --bin nuwa --offline -- --test-threads=4 --skip benchmark::tests` **388 通过、1 ignored、9 个 benchmark 测试被过滤**。不构成 workspace、web、当前 Batch3 完整套件或真实 GitHub/生产部署验证结论。
- 取舍与边界：宁可保留 ambiguous intent/分支并等待所有者，也不猜测远端结果或丢弃中断修复。claim 是协作写者的本地权限约束，不是 OS 沙箱，不能隔离绕过 Store 的进程或保证 GitHub exactly-once。本决策只覆盖 Batch1；“Batch2–Batch6 均未实现”是该批完成时的历史结论，当前顺序与状态见 ADR-028，不预记更高批次或部署成功。
- 被否决备选：仅凭 runner id/问题单终态清理；PR 请求失败或不匹配就删除；无条件远端删除；脏树自动 reset/clean；预领整批 intent 后串行网络恢复；伪造 runner 登记供独立 fix 借权；新旧写者混跑升级。

---

## ADR-024 · Batch2：SHA 绑定 CI 观察与可撤销记忆证据

- 状态：已接受、已实现（2026-10-27）；以下为 Batch2/schema v11 完成时的历史快照；当前代码基线 v0.17.0、schema v14 见 ADR-028。不代表已发布或完整套件通过。未执行生产迁移、部署、真实 GitHub/API 或付费模型测试。追溯：DES-024 → REQ-005、REQ-008、NFR-007（docs/10）。
- 背景：按 PR 号读生命周期不能证明结论属于交付 SHA；用下次检查时间充当租约会把排程和所有权混在一起。前台独立轮询、旧代际迟到写回及“同指纹覆盖一行记忆”会污染新交付、恢复已撤销来源，或让手动启用把无效证据重新送入提示。
- 决策：
  1. **精确身份与独立租约**：watch 以 `(slug, pr_number, head_sha)` 唯一，repo/fingerprint/run/PR URL 的关联不得挪用；重复登记不复活终态。schema v11 新增随机 claim token、递增 generation、expiry，独立于 `next_check_at`；领取、续租、完成及错误写回均核验有效所有权。后台一轮最多 20 条、逐条领取而非预领排队；TTL 120s，每 10s 续租，覆盖网络等待与最终事务，失权停止后续请求/写回。
  2. **前后台共用一条观察路径**：`work` 前台 `wait_for_delivery` 只等待所交付 PR/SHA，和 serve 共用 Store claim、持久检查计划及处理器，不提前绕过退避或活跃持有者。前台预算耗尽/取消仅停止等待，watch 持久保留；未注册/未完成的 CI、不可读响应与超时不是红灯。pending 30s、green 5min 后复查，错误指数退避至多 1h，观察超过 30 天过期；持续推进仍需 serve 常驻。
  3. **SHA 快照与保守关闭**：PR 快照同时严格解析 state/merged/head SHA；未知或不一致响应报错退避，不猜测状态。head 改变进入终态 `head_changed`，不跟随替换 head。green+open 继续观察，green+merged 终态可发布记忆，closed 即使 CI green 也不发布。红灯关闭前重读 open/同 SHA 并检查当前 issue/run/delivery 归属，关闭后再确认 closed/同 SHA 才允许失败计数与重试；外部关闭/合并只记录结果，不自动重开或回滚。
  4. **本地后果同事务**：`finish_ci_watch_owned` 在有效 claim 下原子提交 watch、精确关联 run 的 CI 结论、适用时的 issue 失败收敛、记忆发布/撤销及必要审计；任何一步失败整体回滚，旧 watch 不能拒绝新一轮交付。watch 用 green/red，run/API 保持 success/failure 词汇，不混写。
  5. **来源精确、撤销单调**：`repair_memory_evidence` 保存 `(repo, fingerprint, source_kind, source_id, commit_sha)` 的独立内容并校验 PR URL；CI 来源为 `ci_green`、`source_id=watch.id`，branch-only 本地验证来源为 `local_verification`、`source_id=run_id`。red/closed/head_changed 为该精确 watch/SHA/PR 来源保留撤销 tombstone（即使从未见绿），不抹除其他提交或独立来源。迟到 green 不得复活、刷新已撤销来源的摘要；直接红灯不创建摘要、不触发摘要淘汰。普通摘要裁剪保留证据/tombstone，新的有效来源可独立入选。
  6. **人工偏好不等于证据**：迁移的旧记忆是 `legacy_unknown`，可见但视为未验证、不得注入。仅有效、未撤销的 `local_verification`/`ci_green` 且未手动 `disabled` 才能进入同仓提示；重新启用只清除手动停用，自动写入也不清除此偏好。API/页面分别展示证据有效性、验证来源、撤销时间/原因及手动停用。每仓保留最近 50 条未停用摘要（停用摘要另保留），不以此裁剪证据；显式删除会在同事务删除该记忆摘要及证据，不是保留 tombstone 的停用操作。
- **外部竞态限制**：GitHub close API 不支持原子 expected-head 条件；关前/关后快照和本地租约只能缩小、发现部分检查后使用（TOCTOU）竞态，不能阻止已发出的 close 与 push/merge 竞争。除非不执行自动 close，否则无法消除此残余风险；本批没有新增“关闭自动 close”的配置开关，不保证远端 exactly-once 或“绝不误关”。
- 升级契约（Batch2 历史目标 v11；当前目标 v14 见 ADR-028）：从 v9 经 v10、或从 v10 升级，都必须先停全部旧写者与自动重启入口、一致备份 DB/WAL 与 env，再由新版打开 Store。v10 → v11 增加 CI claims/唯一身份及记忆证据表，旧记忆保留内容与 disabled、标为 `legacy_unknown`；旧 watch 初始无 claim 仅在停旧写者前提下可安全领取。拒绝高于支持版本的数据库，不支持新旧写者混跑或让旧版直接写升级库。生产操作尚未执行，规程见 docs/08。
- 验证证据（已有本地聚焦结果，本次文档更新未重跑）：`cargo test -p shanhaijing --offline` **148 通过**；`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；Nuwa `ci` 过滤 **50 通过、371 filtered**（最终 summary 修正前的结果，不能充当该修正后的复测）；`export_bindings` **72 通过**；`npm.cmd run typecheck` 通过；memory 页面 **6 通过**。不外推为完整 Nuwa、workspace 或完整 web/lint/build 验证。
- 批次顺序：本条完成时的“Batch3 下一批”已由后续 ADR 更新；Batch1/schema v10、Batch2/schema v11 已完成，Batch3 配置/schema v12、Batch4 可信验证/路径护栏已本地实现；Batch5/schema v13 已本地实现并完成聚焦验证；**当前 Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28，ADR-028）**。不把旧 A-3 池/镜像当作后续批次完成证据。
- 被否决备选：以 PR 号或字符串状态代替 SHA 快照；用检查计划充当租约；前台绕过后台调度；未知/超时作红灯；分步写 watch/run/issue/记忆；按 fingerprint 撤销全部来源；迟到 green 覆盖 tombstone；人工启用恢复证据；声称 GitHub close 可用本地 fencing 消除竞态。

---

## ADR-025 · Batch3：保存版本与不可变启动输入证据

- 状态：已接受、已本地实现（2026-10-27）；代码基线 v0.17.0，schema v12；未升版、发版、执行生产迁移或部署。追溯：DES-025 → NFR-003、NFR-004（docs/10）。已有验证的时点与范围见 docs/07 §D，不预记最终聚焦复测。
- 背景：文件保存、进程实际配置与心跳是不同事实；运行期更新 `env_store` 会制造部分热更新，而“最近 runner”或审计时间不足以证明 serve/各 worker 加载了哪版输入，密钥也不能靠明文遥测对比。
- 决策：
  1. **只保存，不热更新**：29 键 schema 全部 `requires_restart=true`，包括只读路径项。PUT 不改 OS env、启动 `env_store`、已加载 Secrets、serve 鉴权/会话或其他活配置；重启相关进程后以其独立启动证据确认。此项覆盖 ADR-022 当时的“保存更新 store / 即时生效”。
  2. **版本身份而非内容摘要**：schema v12 新增 `config_saved(env_id, revision, saved_at_ms)` 与 `runners.startup_config_json`。`env_id` 为解析符号链接后的绝对文件身份（缺失文件解析最近存在祖先；Windows 规范为小写）；revision 为随机 UUID，不是密钥、密钥 hash 或内容 hash。DB 版本表只存元数据，审计只存键名；快照只存脱敏非密钥值，绝不落库密钥或原始输入。
  3. **协作保存串行，但跨介质不原子**：同一 DB 的 `BEGIN IMMEDIATE` 覆盖读、校验、备份、文件替换、revision 与逐键审计；GET 的文件值与 saved revision 也来自共享保存事务中的同一读快照。文件替换与 DB commit **不构成一个原子提交**；替换后失败或任务结果不明返回 500 `config_save_indeterminate`，要求重新读取而非报告成功/盲目回滚。file/DB marker 不一致为 persistence `unknown`，activation 也不能证明。
  4. **建立与保留版本**：成功的非空 PUT 即使无值变更，也会为 unversioned/不一致文件建立新 revision；一致且无变更/迁移的 no-op 保留 revision，`restart_required=false`。空 PUT 不迁移、不建立版本。文件及 `.bak` 都经同目录受保护临时文件、sync、`persist` 原子替换，Windows 不再截断原文件；Unix 写入前设 0600，Windows ACL 仍由所有者配置。
  5. **启动输入证明**：从启动环境快照取 `NUWA_CONFIG_REVISION`，须等于文件 marker、已提交 DB revision，且该角色全部适用可写 schema 输入与文件**精确一致（含缺失/存在与密钥，在内存比较）**，才能设置 `loaded_revision`。歧义语法、重复/旧拼写、缺启动环境 marker 的外部注入即使值相同、或外部 `GITHUB_TOKEN` fallback 都不能猜测为已加载。该证明绑定 managed input revision，不承诺最终 effective 值等于文件：显式 CLI 覆盖可保留输入证明，逐键标 `cli`，同命令重启不会消除覆盖；已知规范化/限幅标 `derived`，来源不明或无法解释的覆盖使证明失效。
  6. **不可变且分实例**：`serve::startup_state` 与 `run_work_with_environment` 在真实启动路径记录解析后的值/来源；work 在派生 worker 或等待租约前只取一次证据。每个 OS 进程一个随机 `process_instance_id`，pool worker 共享它，但 runner id/`runner_instance_id` 各自独立。runner 快照仅允许当前未过期租约实例首次设置（完全相同重试可接受），接管清空旧快照；心跳/配置刷新不覆写启动事实。provider/model/base 来自实际 resolver：zai 缺省 `glm-5.3-flash` 与 coding endpoint，Anthropic 缺省 catalog 首项且显式未知模型不伪装成已解析默认，faux 取实际离线 provider 默认，不使用旧显示标签代替事实。
  7. **三轴展示，不选最新 runner 当权威**：持久化 `saved/unversioned/unknown`；逐进程 activation `active/stale/unknown`（同文件身份且输入 revision 相等/旧版/未获证明）；liveness `online/offline/unreported` 各自独立。offline 可保留历史 active 证据，但不代表仍运行；配置 unit 未上报与旧 runner 无快照均不能推断生效。设置、system、runner 视图展示适用非密钥实际值与显式来源；`restart_pending/effective_diff` 只作为 additive API 的已弃用兼容字段，不再驱动 UI 生效结论。
  8. **候选探测不是活进程证据**：显式 `candidate=saved` 使用文件+默认且不允许 draft values；`draft` 使用同一次文件读取+草稿（旧请求默认 draft），绝不回退 live env/key。更改 zai base URL 必须显式提供该草稿 key，不能携带保存/运行中的 key 去新 host；探测不保存、不证明激活。
- **所有者来源契约（不是密码学证明）**：带 marker 文件由所有者控制；任何 out-of-band 手工编辑**必须移除/使 revision marker 失效**，再经非空 PUT 重新建立版本。保留 marker 的人工改写不能被本机制认证或可靠检测；UUID 不是内容签名，不宣称防篡改。`migrate-env` 真正重写时移除 marker，无改写则保留；它不参加保存事务，只能停写、离线维护，**不得与设置保存并发**。
- 升级契约：schema v12 只能停全部旧写者与自动重启入口后，一致备份 DB/WAL、env/其备份与必要现场，再在隔离副本验证并由新版迁移；v11 → v12 不编造旧配置版本或旧 runner 启动证明，迁移后需受控保存/重启。详见 docs/08；此处是待执行规程，没有执行迁移、systemd 或平台部署验证。
- 批次顺序（当前以 ADR-028 为准）：Batch1/2 已完成（结果保留为历史快照）→ **Batch3/schema v12 已本地实现** → **Batch4 可信验证/路径护栏已本地实现、限定检查完成，该批未新增 schema** → **Batch5 observations/schema v13 已本地实现并完成聚焦验证** → **Batch6 reservations/schema v14 已本地实现并完成本地最终检查（2026-10-28）**。
- 被否决备选：保存热改 env_store/auth；以文件 marker、相同值、最新 runner、心跳在线或审计时间代替启动证明；存密钥 hash；跨文件/DB 假称原子；把 active 当作 CLI 值等于文件；把所有者可改 marker 当作密码学认证。

---

## ADR-026 · Batch4：冻结所有者 oracle 与真实文件系统路径护栏

- 状态：已接受、已本地实现（2026-10-27）；代码基线 v0.17.0，**schema 仍为 v12**，限定检查完成（Batch4 sidecar 45/45 历史；完整 workspace nextest 2026-10-28 通过）；未升版、发版、生产迁移或部署。追溯：DES-026 → REQ-003、REQ-006、REQ-007、NFR-003、NFR-007（docs/10）。分时点验证见 docs/07 §D，不宣称全绿。
- 背景：候选可修改的复现脚本、测试或辅助输入不能自行成为可信证明；只在工具 hook 检查字符串不能覆盖递归读取、路径别名及实际 I/O。冻结来源与路径授权必须接入真实修复/验证/交付路径，同时不能冒称 OS 隔离。
- 决策：
  1. **显式策略，不静默切换**：`nuwa.repo.json` 的 `verification_policy` 默认 `compatibility`；未配置 owner oracle 时沿用较弱门禁，证据明确为 `legacy_untrusted`，不自动升级 strict。显式 `strict` 缺少 `trusted_verification` 在模型、创建本地交付分支、staging、push、PR 前拒绝；任何策略只要配置 oracle 就必须通过它，不允许失败后降级。issue/模型输出不得建立 owner 权威。
  2. **所有者维护的完整闭包**：`trusted_verification.check` 启动字面量受保护入口；`protected_inputs` 显式穷举入口、helper、fixture、配置等全部验证依赖。修复前从固定 base 的 tracked 文件，或私有 owner 配置对应的预置 untracked 文件捕获 bytes、文件类型、Git/native mode 与 manifest，冻结在 checkout 外。闭包完整性由所有者负责，不自动发现依赖；入口若再从候选加载断言/helper，就不满足本契约。任意故障程序退出 1 不会在数学上证明业务 bug，owner 必须把预期缺陷退出码设计为 1、通过为 0、意外 setup/运行故障为 2+；超时/缺命令/缺输入均不作 red。
  3. **同址导出、独立入口**：base 与候选轮流导出到同一物理 `NUWA_VERIFY_ROOT`，每侧重建 target/home/tmp/cache，不携带 Git 元数据或 base/branch 标签；最小固定环境不继承 agent 扩展、凭证、代理、候选环境或 `PYTHONPATH`。入口从外部冻结目录启动，不从候选树启动；先在修复前证明 base exit 1，再要求候选 owner exit 0，候选 `failing_check` 与适用的 base-green 深检也须绿。模型 `.nuwa/repro.*`、修改测试或自述不能替代 oracle。
  4. **冻结上下文贯穿交付**：同一个 `VerificationContext` 绑定 base SHA 与完整 candidate tree；本地交付也须在副作用边界复核。移动 base 后保留原上下文重验，不采用新 oracle；tracked owner 内容/模式变化仍拒绝。池从权威配置根冻结全部输入后、模型前，仅向工作树补缺失的私有 untracked 输入，不收养候选 bytes、不覆盖已有篡改。private 路径在 staging、candidate tree、diff 采用一致的精确文件排除，保留 `:(exclude)nuwa.repo.json`；tracked oracle 不被排除，不能隐藏篡改。
  5. **读禁区与禁改分离**：`forbidden_paths` 禁写但可由内置文件工具读取；`denied_read_paths` 及内建凭证/Git/私有验证/会话/DB 规则禁读且禁写。`guard/local_env.rs` 包装实际执行环境，递归 list/find/grep 与文件读写均先授权，目录移动/删除检查后代；不是仅依赖 `before_tool_call` hook。路径必须留在 canonical checkout 内，符号链接、Windows junction/reparse、特殊条目拒绝，不能以别名绕过。
  6. **证据与验收**：验证 detail/PR 记录 `owner_trusted` 或 `legacy_untrusted`、base/candidate tree、owner 来源与摘要、manifest 摘要和退出结果；不公开冻结输入内容或私有原始输出。`nuwa benchmark batch4 --output <新建临时 JSON>` 使用密封 manifest 的计算摘要、15 场景 × 3 次 = 45 条 faux 记录，仅写安全元数据/摘要/声明/实际结果 sidecar，不打开默认业务 DB、不跑 live model；证明的是流水线/门禁，不是模型修复能力。CLI 首次在 trials 前栈溢出，装箱后 sidecar 45/45 预期满足，但早于最后私有 seed cleanup 修正；完整 Nuwa 483 通过、3 失败、2 ignored 及后续聚焦复测保留在 docs/07 §D，不宣称完整套件全绿。
- **安全边界**：这是来源/完整性与内置文件工具授权，不是 OS 沙箱。shell、检查命令及候选程序仍有宿主运行用户权限；最小 env 不能阻止自行访问宿主文件/网络。路径复核存在 TOCTOU，hardlink 不能靠 link/reparse 标记隔离。当前 runner Docker 非执行/交付权限分离；独立低权限执行用户、不挂宿主 HOME/发布密钥、限制可写挂载与网络出口的方案需要平台授权，**未实现/未验证**，不是本批安全承诺。
- 批次顺序：Batch4 已本地实现、限定检查完成；后续 Batch5 observations/schema v13 **已本地实现并完成聚焦验证（ADR-027）**，Batch6 reservations/schema v14 **已本地实现并完成本地最终检查（2026-10-28，ADR-028）**。没有生产、真实 GitHub、systemd、付费模型、部署或 Unix 验证结论。
- 被否决备选：把 agent reproducer 当 owner oracle；从候选重新读取权威；missing oracle 静默回退；把任意非零都算 bug；排除 tracked oracle 隐藏篡改；只做 hook 字符串过滤；把 env 白名单、Docker 或路径复核声称为 OS 权限隔离。

---

## ADR-027 · Batch5：可审计信号观察与仅调用方可授予的权威

- 状态：已接受、已本地实现并完成聚焦验证（2026-10-27）；代码基线 v0.17.0，**该批 schema v13**；未升版、发版、执行生产迁移或部署。追溯：DES-027 → REQ-004、REQ-005、REQ-008、NFR-003（docs/10）。后续 Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28，ADR-028）；以下 Batch5 证据不变。
- 背景：接收时间不等于故障发生时间；迟到报告不能仅凭自报时钟推翻终态，HTTP 鉴权也不能证明来源事件与环境/发布版本关系。问题单聚合、有限回执去重、记忆证据有效性与人工停用必须分开审计，PR 合并不能充当部署或恢复证据。
- 决策：
  1. **兼容信封，独立回执**：`Signal` 增加默认缺省的可选 `observed_at`（epoch 毫秒）、`environment`、`release_sha`、`source_event_id`；不改指纹，旧 payload 沿用原回归行为。schema v13 增 `signal_observations`，分别保存服务端 `received_at_ms`、原始采集时间与元数据、来源授信、时间状态、处置及重复关联。每问题单仅保留按 `received_at_ms DESC, id DESC` 排序的最近 100 条，另按接收时间清理超过 30 天的回执，不按采集时间清理。
  2. **身份拒绝，不截断**：`environment`/`release_sha`/调用方 `trusted_source` 分别最多 128 UTF-8 字节，`source_event_id` 最多 256 字节；提供时拒绝空白、控制字符和超长值，不截断身份。缺失时间标 `missing`，非正数标 `invalid`，晚于本次接收时间标 `future`；无效/未来整数仍保留但不授予时间权威，合法时间无来源授信为 `untrusted`，二者均满足才为 `trusted`。
  3. **仅调用方授权**：`Store::report_signal_with_source` 的来源由调用方传入，不从 JSON 自报读取。CLI 只有所有者显式使用 `nuwa ingest --trust-observations <source-id>` 才授信；这是 **OWNER ATTESTATION（所有者确认）**：已核验导入时钟、来源事件 ID，以及环境与精确发布版本的关系，不是密码学认证或普通导入选项。HTTP bearer/token 鉴权永远不建立观察权威，payload 不能自授信。
  4. **迟到判定从严**：仅对 `merged/rejected`，在终态 `state_changed_at` 已知且为正、合法可信采集时间**严格早于**该时间时标 `delayed`，不重开。相等、终态时间未知、采集时间缺失/无效/未来或未授信，均沿用原回归规则；ignored/escalated 不因此自动重开。重复事件另按下一条处理，不是新的回归证据。
  5. **有界去重，不保证全局一次**：仅在保留的同 fingerprint、调用方可信 source/event 身份中比对 `observed_at`、environment 与 release 的精确元数据；匹配标 `duplicate`，冲突拒绝，不静默覆盖。完整 40/64 位十六进制 SHA 统一小写，不截短、不把分支名当 SHA。每个成功接收的回执（包括 delayed/duplicate）仍累计 occurrences 与接收日 `signal_daily`；回执裁剪/过期后不能保证再次识别，不宣称全局 exactly-once。
  6. **证据撤销与人工开关独立**：有效可信观察携带明确 environment、完整 40/64 位十六进制精确 release，且采集时间严格晚于 `evidence.recorded_at` 时，适用的回归证据撤销匹配 repo/fingerprint/SHA 的记忆证据；delayed/duplicate/muted 不作为新的撤销依据。即使记忆已手动 `disabled`，仍须撤销该证据；不清除 disabled，不波及独立提交，重新启用也不能复活证据。环境/版本关系由所有者授信承担，不因此新增部署集成。
  7. **展示未知，不推断恢复**：问题详情 API/UI 展示有界回执、来源授信与时间状态，`deployment_status`、`recovery_status` 均为 `unknown`；尚无部署/健康集成，merged、CI green 或上报 release 都不等于已部署/已恢复。
  8. **每回执事务，不是批原子**：单回执的聚合计数、状态、观察与适用证据撤销同事务。HTTP 元数据错误返回有界、通用 400，不回显 payload/元数据值；预校验不代表整个 JSONL 原子，后续运行期失败前已提交的回执保留，响应说明已提交数量，不报告整批回滚或全局一次处理。
- 升级契约：v12 → v13 是增量事务迁移，建表/索引及 `user_version` 同事务；旧问题单不从 first_seen/last_seen 等字段伪造 observations，失败保留 v12。所有者授权后先停全部旧写者/自动重启入口、一致备份 DB/WAL 与 env，在隔离副本验证再受控升级；不支持混跑或旧版写 v13，规程见 docs/08。**未执行生产操作**。
- 验证证据（父任务提供，本次仅文档；均为“手动 disabled 仍撤销证据”修正后）：Store **173 全部通过（含迁移）**；Nuwa `ingest` 过滤 **11 通过**、issues API **15 通过**、`ts-export` **78 通过**（已知 serde 警告）；`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps -- -D warnings` 通过。web typecheck/lint、IssueObservations + IssueDetailPage 聚焦 **23 通过**、build 通过；架构 **17 模块 / 6 crate / 2 条债**、追溯 **16 需求 / 27 设计、零缺口**通过；生成文件 whitespace 已修正，TS 重新导出及 diff 检查通过。
- 验证边界：Batch5 的最终 workspace/web 套件已在 2026-10-28 最终检查中通过；不覆盖或抹去 Batch4 的历史失败与聚焦复测记录，不声称跨平台、真实 GitHub/API、systemd、付费模型或生产验证通过。完整时点见 docs/07 §D。
- 被否决备选：把 bearer 鉴权/JSON 自报当授信；按采集时间淘汰回执；截断身份；迟到判定包含相等或未知时间；去重后不计回执；把有限历史宣称全局 exactly-once；因 disabled 跳过证据撤销或启用复活证据；把 merged 当已部署/恢复；预校验即宣称 JSONL 批原子；迁移伪造旧观察。

---

## ADR-028 · Batch6：持久 PR 准入槽位与最终 token 闸

- 状态：已接受、已本地实现（2026-10-27），**schema v14，最终检查完成（2026-10-28）**；main 基线 v0.17.0，未升版、发版或执行生产操作。追溯：DES-028 → REQ-003、REQ-005、REQ-008、NFR-003（docs/10）。
- 背景：先读已开 PR 数再创建会被并发越过；租约过期、跨午夜或响应丢失不能证明没有远端副作用。token 已花费事实也不能靠交付拒绝撤回。
- 决策：
  1. **原子准入、沿用交付权限**：schema v14 新增 `pr_slots` / `pr_slot_deliveries`；按 repo 与 UTC 准入日，在 `BEGIN IMMEDIATE` 内核验 delivery claim 的 token/generation/expiry 并计数、预留。`reserved/requested/ambiguous/consumed` 计入，`released` 不计；没有独立槽位 token。
  2. **持久而非 TTL 退款**：释放/过期 claim、重启、遥测清理均不退款；跨午夜与接管重试复用同一槽位和原准入日。仅请求前的安全 head supersede 可在同事务转移预留，不能转移 requested/ambiguous；确认同 repo + PR URL 的重复交付归并为一个 consumed 槽位。
  3. **所有创建入口共闸**：work、pool、独立 fix 的真实交付在 push 尝试前预留，请求前再幂等核验并持久写 requested。预 push PR 配额拒绝释放问题单、冷却/待命，不计 attempt；branch-only 不占 PR 槽位。push 后门禁拒绝保留精确分支、intent/run 与槽位恢复证据，不因取消/响应丢失退款；恢复先查精确 head/base，只有确认无 PR 才允许终结/释放及条件清理，不确定保持 ambiguous。此机制**不保证 GitHub 外部 exactly-once**。
  4. **分开三个时钟**：准入时间/UTC 日不随确认改变；GitHub `created_at` 解析为可选 `created_at_ms`，本地采用/完成时间为 `confirmed_at_ms`。旧 `pr_opened_at` 是本地观察，迁移不得伪造 GitHub 创建时间；缺失/无效 API 时间保持 unknown，不阻止采用身份有效的 PR。完成时槽位消费、适用 issue/run/watch 同事务。
  5. **保守迁移**：v13 → v14 的表/索引/历史归并/版本同事务，失败保留 v13；历史 runs/issues/已完成 deliveries 按 repo + PR URL 去重，无 URL 保留独立证据。未确认的 requested/ambiguous 保守占位，不把旧 intent/pushed 或确认时间伪作创建证据。
  6. **token 是观察闸，不是预留硬封顶**：模型前已知仓库用量 `>=` 日/滚动 30 天限额即拒绝；最终个体/仓库闸仅在 `>` 时拒绝。最终用量取当前 run 各 cached/uncached 分量的持久值与报告最大值，扣除已计当前 run 后再加入，含跨窗口当前修复；失败 work 与独立 fix 也持久记账。窗口按 run started_at 聚合，monthly 是滚动 30 天而非自然月。运行中/并发、provider 部分或未知 usage 仍可超支；没有 token reservations 或费用硬 cap，拒绝交付不能撤回已花 token。
  7. **UI/API 不混用指标**：projects/runners additive `pr_slots_today` 是仓库 UTC 准入槽位，`prs_today` 保留按 PR URL 去重的创建/历史本地观察指标，不作准入依据。runner `daily_pr_limit` 是该进程上报值，project `pr_limit` 是上报值观测最大值；不是中心统一限额，各创建进程配置可能不同。
- 运维：v9–v13 升至 v14 必须停全部旧写者/自动重启入口、WAL 一致备份、核对 pending deliveries/slots 并先验证隔离副本；v14 重开亦须检查 pending。不混跑、不直接降级、不按 TTL 或人工猜测退款，详见 docs/08。未执行生产操作、付费模型或平台隔离验证。
- 现有证据仅为 **Store 186 通过（timestamp/CLI refinements 前）与 quota 过滤 3 通过（refinements 前）**；不能视为当前最终复测。delivery 42 项运行结果待确认；最终格式/clippy、后端/迁移/CLI/API、TS 导出、web、架构/追溯与 diff gates 均待记录。本次仅文档；Batch5 已有结果与 Batch4 完整套件失败证据保留在 docs/07 §D。
- 被否决备选：以 PR 历史计数代替原子预留；租约 TTL/午夜自动退款；请求后 supersede 挪用槽位；把确认时间当 GitHub 创建时间；把 runner 观测值当统一限额；声称 token 闸可硬封顶费用或撤回消费。