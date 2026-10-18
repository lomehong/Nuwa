# Web 控制台 v3 Implementation Plans

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md)（v3，2026-10-17）
**Decision record:** [`docs/04-decisions.md`](../../04-decisions.md) ADR-009

## 为什么拆分

v3 覆盖数据层、runner 遥测、JSON API、鉴权、SPA 与切流，单个计划会超过可审阅规模，也无法做到"每个计划独立可验收"。按里程碑拆成可独立验收的计划，每个都产出可运行、可测试的软件：

| 计划 | 范围 | 状态 | 独立验收 |
|---|---|---|---|
| [`M0 线上热修`](2026-10-17-web-console-m0-hotfix.md) | XSS、reopen、env 键、心跳顺序、旧 POST 同源校验 | 已完成 | 旧控制台修复，clippy/test/冒烟通过 |
| [`M1a 数据层与 Store`](2026-10-17-web-console-m1a-store.md) | 迁移、runners/runs/signal_daily、events 审计、stuck/requeue/reopen、配额/清理 | 已完成 | Store 测试全绿；旧 CLI/UI 不回归 |
| [`M1b Runner 遥测`](2026-10-17-web-console-m1b-telemetry.md) | repair 进度、deliver 证据、work 注册/心跳/run 生命周期 | 已完成 | faux 全链路测试断言 run 遥测 |
| [`M2a 鉴权与路由`](2026-10-17-web-console-m2a-auth-router.md) | 会话、CSRF、限速、`/api/v1` 路由、静态资源 | 已完成 | API 契约与安全测试 |
| [`M2b 只读 API`](2026-10-17-web-console-m2b-read-api.md) | overview/issues/detail/runners/events/trends | 已完成 | 六端点集成测试 |
| [`M2c 动作与设置 API`](2026-10-17-web-console-m2c-actions-settings.md) | reopen/requeue/sync/restart/settings/system/transcript | 已完成 | 动作/设置/系统/转录集成测试 |
| [`M3 SPA 核心`](2026-10-18-web-console-m3-spa-core.md) | Vite/React、登录、外壳、总览、问题单、详情、转录、响应式与无障碍 | 已完成 | 136 前端测试 + 浏览器三档 UAT |
| [`M4 只读页面`](2026-10-18-web-console-m4-readonly-pages.md) | 运行器、活动/审计、趋势时间范围、深链 | 可执行 | 运行器/活动页 + 三档 UAT |
| [`M5 动作与设置`](2026-10-18-web-console-m5-actions-settings.md) | reopen/requeue/sync、重启、设置保存/测试/重启编排、E2E | M4 完成后执行 | 写操作审计 + Playwright 冒烟 |
| [`M6 加固与切流`](2026-10-18-web-console-m6-cutover.md) | a11y/搜索/退避/保留清理、移除旧 UI、0.2.0、演练 | M5 完成后执行 | axe + 5k 基准 + 回滚演练 |

## 执行顺序

1. **M0** 已完成，独立 patch 降低线上暴露窗口。
2. **M1a → M1b** 已完成；M1a 只改 `shanhaijing`，M1b 接 `work`。
3. **M2a → M2b → M2c** 已完成，API 契约冻结。
4. **M3 → M4 → M5 → M6** 顺序执行；旧 UI 在 M6 删除。

## 全局约束（所有计划共享）

- 自建 crate 零 clippy warning；行为改动带测试。
- 不碰 vendored `crates/rpi-*`；修复/交付/护栏行为不变（除非计划明确声明）。
- 凭证只从环境变量读取。
- 本机构建前先处理 `nuwa.exe` 锁；验证用 shared-target 产物。
- 遥测 best-effort，绝不阻断修复主链。
- 每个任务一个 commit；大版本发布前完成生产库副本演练与回滚演练。

## 执行方式

每个计划按任务顺序执行，任务内置"失败测试 → 实现 → 通过 → 提交"循环。推荐使用 `superpowers:subagent-driven-development`，每个计划独立派发与评审；也可使用 `superpowers:executing-plans` 在当前会话分批执行并在检查点停下评审。
