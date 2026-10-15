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
