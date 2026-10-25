# Nuwa（女娲）· 文档中心

> 天有裂缝，炼石补之。
> Nuwa 是一个自愈式修复 Agent：青鸟（现场采集 SDK）巡行于各系统，把运行时的裂缝报上山海经（问题单库）；女娲炼五色石为每一道裂缝定制补丁，由应龙（修复 runner）验证后化作 PR，合入你的仓库。天补好了，青鸟继续巡逻。

**一句话定位**：面向个人私有系统群的、问题单驱动的自动修复 Agent——现场只采集，云端做修复，产出直接是可合入的 PR。

## 文档索引

| 文档 | 内容 |
|---|---|
| [01-vision.md](01-vision.md) | 愿景、产品定位与边界 |
| [02-upstream-assessment.md](02-upstream-assessment.md) | 背景：上游 pi-rust 成熟度评估结论（fork 决策依据） |
| [03-architecture.md](03-architecture.md) | 架构共识：现场采集 / 服务端修复 / 验证探针 |
| [04-decisions.md](04-decisions.md) | 决策记录（ADR），含被否决的备选方案 |
| [05-naming.md](05-naming.md) | 命名宇宙（女娲/青鸟/山海经/五色石/应龙）与 Logo |
| [06-requirements.md](06-requirements.md) | 约束与要求（语言无关性、运行器工具链、仓库指令文件） |
| [07-open-items.md](07-open-items.md) | 待详细设计与待办清单 |
| [08-onboarding.md](08-onboarding.md) | 接入手册：四步接入一个系统 |
| [09-web-console-design.md](09-web-console-design.md) | Web 控制台设计（v3）：SPA + JSON API + 会话鉴权 + runner 遥测 |
| [10-traceability.md](10-traceability.md) | 需求、ADR、实现与测试的双向追溯矩阵 |

## 关键事实速览

- **起源**：fork 自 [bigfish1913/pi-rust](https://github.com/bigfish1913/pi-rust)（MIT），裁剪为自用运行时
- **形态**：服务端 Agent 运行器池（rpi 裁剪版）+ 各系统内的薄采集 SDK（青鸟，按系统语言编写，与 Rust 无关）
- **产出**：直接开 PR（非 draft），CI 为合并闸门
- **语言无关**：可修 Go / Java / Rust 等任何语言的仓库；语言差异被推到"采集 parser"与"runner 工具链"两个外围位
- **创建**：2026-10-15
