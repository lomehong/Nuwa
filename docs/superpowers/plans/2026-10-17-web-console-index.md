# Web 控制台 v3 Implementation Plans

**Spec:** [`docs/09-web-console-design.md`](../../09-web-console-design.md)（v3，2026-10-17）
**Decision record:** [`docs/04-decisions.md`](../../04-decisions.md) ADR-009

## 为什么拆分

v3 覆盖数据层、runner 遥测、JSON API、鉴权、SPA 与切流，单个计划会超过可审阅规模，也无法做到"每个计划独立可验收"。按里程碑拆成四个可执行计划，每个都产出可运行、可测试的软件：

| 计划 | 范围 | 状态 | 独立验收 |
|---|---|---|---|
| [`M0 线上热修`](2026-10-17-web-console-m0-hotfix.md) | XSS、reopen、env 键、心跳顺序、旧 POST 同源校验 | 可执行 | 旧控制台修复，clippy/test/冒烟通过 |
| [`M1a 数据层与 Store`](2026-10-17-web-console-m1a-store.md) | 迁移、runners/runs/signal_daily、events 审计、stuck/requeue/reopen、配额/清理 | 可执行 | Store 测试全绿；旧 CLI/UI 不回归 |
| [`M1b Runner 遥测`](2026-10-17-web-console-m1b-telemetry.md) | repair 进度、deliver 证据、work 注册/心跳/run 生命周期 | 可执行 | faux 全链路测试断言 run 遥测 |
| M2 API 与会话 | `/api/v1`、登录会话、CSRF、设置 schema、静态资源 | M1b 完成后编写 | API 契约与安全测试 |
| M3–M6 SPA | React 骨架、只读页、动作/设置、加固与切流 | M2 完成后编写 | 生产库副本 UAT + 切流演练 |

## 执行顺序

1. **M0** 可立即执行，独立发布 patch，降低线上暴露窗口。
2. **M1a → M1b** 顺序执行；M1a 只改 `shanhaijing`，M1b 才接 `work`。
3. **M2** 在 M1b 的 Store/遥测接口冻结后编写，避免 API 返工。
4. **M3–M6** 在 M2 契约稳定后编写；M0 的旧 UI 在 M6 删除。

## 全局约束（所有计划共享）

- 自建 crate 零 clippy warning；行为改动带测试。
- 不碰 vendored `crates/rpi-*`；修复/交付/护栏行为不变（除非计划明确声明）。
- 凭证只从环境变量读取。
- 本机构建前先处理 `nuwa.exe` 锁；验证用 shared-target 产物。
- 遥测 best-effort，绝不阻断修复主链。
- 每个任务一个 commit；大版本发布前完成生产库副本演练与回滚演练。

## 执行方式

每个计划按任务顺序执行，任务内置"失败测试 → 实现 → 通过 → 提交"循环。推荐使用 `superpowers:subagent-driven-development`，每个计划独立派发与评审；也可使用 `superpowers:executing-plans` 在当前会话分批执行并在检查点停下评审。
