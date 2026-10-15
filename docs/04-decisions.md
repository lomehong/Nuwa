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
