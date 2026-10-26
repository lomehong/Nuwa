# 10 · 需求—设计—实现—验证追溯

状态：当前基线（v0.15.0，2026-10-26）。需求正文见 `01-vision.md` 与 `06-requirements.md`；设计决策见 `04-decisions.md`。每行是一个唯一设计，需求可在多行复用。`scripts/check-traceability.py` 校验编号、ADR 覆盖、孤立设计和证据字段。

## 追溯矩阵

| 需求 | 设计 / ADR | 实现证据 | 验证证据 |
|---|---|---|---|
| NFR-001 | DES-001 / ADR-001 | `crates/rpi-*`；根 `Cargo.toml` lints | `scripts/check-vendored.py`；CI clippy/tests |
| REQ-004 | DES-002 / ADR-002 | `shanhaijing/src/fingerprint.rs`、`lib.rs::report_signal` | 指纹、去重、冷却与原子上报测试 |
| REQ-003 | DES-003 / ADR-003 | `nuwa/src/deliver.rs`、`guard.rs` | 交付与护栏测试；目标仓 CI + 人工合并 |
| REQ-001 | DES-004 / ADR-004 | `deliver.rs` GitHub REST；`git_ops.rs` | URL 解析、凭证脱敏、超时测试 |
| REQ-002 | DES-005 / ADR-005 | `nuwa work` / `serve`；目标仓 runner | runner、serve 与 faux 端到端测试 |
| REQ-009, NFR-005 | DES-006 / ADR-006 | `rpi-agent` / `rpi-tools`；`nuwa.repo.json` | vendored 测试；Go/Rust/Node/Python 基准 |
| NFR-006 | DES-007 / ADR-007 | 文档命名与产品边界 | `docs/01-vision.md` 非目标审查 |
| NFR-002 | DES-008 / ADR-008 | 五个 vendored crate；`docs/vendored-patches.json` | `scripts/check-vendored.py` |
| NFR-004 | DES-009 / ADR-009 | `nuwa/src/serve.rs`、`session.rs`、`api/`；`web/src/` | API/CSRF/会话测试；Vitest、E2E、axe |
| NFR-007 | DES-010 / ADR-010 | `nuwa/src/verify.rs`、`deliver.rs` | base 红/分支绿、reproducer、diff 终检测试 |
| REQ-005 | DES-011 / ADR-011 | `nuwa/src/webhook.rs` | HMAC 向量、事件 ID、过滤与 drain 测试 |
| REQ-008 | DES-012 / ADR-012 | `shanhaijing/src/lib.rs` 状态与 attempts | 自动重试、ignored、owner reopen 测试 |
| REQ-004 | DES-013 / ADR-013 | `shanhaijing/src/fingerprint.rs` | repo 限定 golden、跨仓同栈隔离测试 |
| NFR-002 | DES-014 / ADR-014 | 已登记 vendored 本地补丁 | 补丁回归测试；`check-vendored.py` |
| NFR-007 | DES-015 / ADR-015 | `pipeline.rs`、`repair.rs`、`deliver.rs`、`skills.rs` | 单一路径、脱敏快照、delivery head 与上下文测试 |
| REQ-006, REQ-007 | DES-016 / ADR-016 | `nuwa/src/benchmark.rs`；`api/benchmarks.rs`；`web/src/features/benchmarks/` | 正/负场景、误报和交付门测试 |
| NFR-001 | DES-017 / ADR-017 | 根 `Cargo.toml` `[profile.dev] debug = 0` | CI/本地增量构建；磁盘写入口径记录 |
| REQ-008 | DES-018 / ADR-018 | `shanhaijing` runner lease；`main.rs` 心跳续租 | 互斥、过期接管、释放与迁移测试 |
| NFR-003 | DES-019 / ADR-019 | `pipeline.rs`、`repair.rs`、`session.rs`、`api/transcript.rs` | usage 排水、快照脱敏、会话代际与失败原子性测试 |
| REQ-005 | DES-020 / ADR-020 | `main.rs`、`deliver.rs`、`ci_watch.rs`、`shanhaijing` deliveries/ci_watches | 失败收敛、恢复 head/base、退避、终态和分页测试 |
| NFR-007 | DES-021 / ADR-021 | `nuwa/src/verify.rs` Go 深检三角 | 规格选择、红基线跳过、分支红拦截、双侧绿测试 |
| NFR-003 | DES-022 / ADR-022 | `nuwa/src/env_store.rs`、`secrets.rs`、`api/settings.rs` | env_store 读写/清除单测；设置 PUT 更新 store；并行测试不再改 OS env |

## 尚未实现／明确延期

以下内容不伪装为已实现，也不建立虚假的“完成”证据：

- `docs/07-open-items.md` A-3 后半：多仓库并发池和 runner 镜像矩阵。
- `docs/07-open-items.md` A-4：现场预定义验证探针协议——阻塞于现场 SDK（本轮排除）执行预定义脚本并回传结果。
- `docs/09-web-console-design.md` §19：SSE、GitHub OAuth、项目级权限等后续候选。
