# 02 · 背景：上游 pi-rust 成熟度评估

状态：评估完成（2026-10-15，五维并行调研）

## 项目识别

[rpi](https://github.com/bigfish1913/pi-rust) — 自述"a Rust-native coding-agent runtime"。
NOTICE 证实其为 Mario Zechner 的 TypeScript `pi` SDK（[earendil-works/pi](https://github.com/earendil-works/pi)，MIT，115k★ 级头部项目）的**独立 Rust 重写**（非 fork），保留了移植谱系标注（"Mirrors packages/agent/src"）。

## 关键数据

- 2 个月历史（2026-08-15 创建），445 commits，release 0.3.24（发版节奏 ≈0.85 版/天）
- 9 crates：rpi-agent(3.4k 行) / rpi-ai(19.4k) / rpi-tools(8.8k) / rpi-harness(25.2k) / rpi-tui(27.2k) / rpi-cli(48.5k) / extensions / plugin-sdk / telemetry；src 共 266 文件 / 14.7 万行
- 1662 个测试，9/9 crate 有测试，含行为级测试（精确事件序列断言、真实 cdylib 跨 FFI 生命周期、faux 离线 mock provider）
- 单人项目：作者占绝对主导，外部贡献 10/445 commits；104★；crates.io 总下载 <2k
- 重度 AI 辅助开发（codex/* 分支、commit.id、.rpi/.codegraph、docs.rs 历史页残留生成指令原文），痕迹经刻意清洗

## 五维评分

| 维度 | 评分 | 要点 |
|---|---|---|
| 架构与完成度 | 4.5/5 | 分层无环、薄核心厚应用；TODO 密度 0.1/千行，`todo!`/`unimplemented!` 为 0；核心声称（crash-resumable loop / plugin ABI / 8 provider）全部落地 |
| 测试与 CI/CD | 4/5 | 行为级测试；发布工程超水准（5 平台 + sha256 + 原子 manifest + 自动 crates.io + CHANGELOG 对账）；**无 clippy、MSRV 不验证** |
| 代码质量 | 3.5/5 | unsafe 克制（集中 FFI，6 crate forbid）、thiserror 全覆盖 0 anyhow、0 git 依赖；**生产 unwrap ≈730 处（≈50/万行）**、无 `[lints]`、13% 文件超千行、最大 5771 行 |
| 文档与 DX | 3.5/5 | README 声称 95% 可验证、文档-代码同步抽查全中、2 步离线入门；docs.rs 0 示例、ROADMAP 失修 |
| 社区与生态 | 2/5 | 巴士因子 = 1；上游迭代极快，兼容性追赶依赖一人 |

## 对 Nuwa 的结论

- **工程上值得fork**：library-first 设计 + 薄核心 + crash-resumable loop 恰好匹配嵌入式修复场景；产出质量高于同体量个人项目
- **必须加固**：保留 crates 补 `[lints]` + clippy 进 CI；削减生产 unwrap；锁定 vendor 版本（上游 API 不稳定，2 个月 51 版）
- **适合裁剪**：147k 行中约 100k（tui/cli/extensions/plugin-sdk/telemetry）是终端产品面，Nuwa 不需要
