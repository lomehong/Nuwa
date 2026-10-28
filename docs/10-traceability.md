# 10 · 需求—设计—实现—验证追溯

状态：main 基线 v0.17.0；2026-10-27 Batch1/2 已完成，Batch3/4 已本地实现，Batch5/schema v13 已本地实现并完成聚焦验证；Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28），未升版/发版。最终本地证据含完整 workspace nextest `--no-capture` 1486 通过 / 2 skipped；不代表 Linux CI/平台/GitHub/systemd 验证；生产迁移/部署、付费模型与平台隔离验证未执行。需求正文见 `01-vision.md` 与 `06-requirements.md`；决策见 `04-decisions.md`。每行唯一设计，需求可复用。`scripts/check-traceability.py` 校验编号、ADR 覆盖、孤立设计及证据字段；“测试已编写”不等于通过。最终本地检查（2026-10-28）：追溯 **16 需求 / 28 设计、零缺口 / 零孤立 / 零编号错误**，`scripts/check-traceability.py` 通过；完整测试与 web 证据见 docs/07 §D。

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
| NFR-003 | DES-022 / ADR-022 | `nuwa/src/env_store.rs`、`secrets.rs` 启动单线程快照/清秘，运行期不改 OS env；保存语义已被 ADR-025 修订 | env_store 读写/清除单测；测试不再改运行期 OS env；历史“PUT 更新 store / 即时生效”已撤销，不能宣称历史挂死根因已修复 |
| REQ-005, REQ-008 | DES-023 / ADR-023 | `shanhaijing/src/migration.rs`、`lib.rs` schema v10 delivery/checkout claims、原子完成与 supersede；`nuwa/src/deliver.rs`、`git_ops.rs`、`pipeline.rs`、`main.rs` 所有权/续租、直接内存 PR 正文、条件删除与保守 preflight；detached HEAD 安全恢复、保守 pool teardown、`branch_only`/`branch_pushed` | **Batch1 历史限定范围通过**：Store claim 竞争/过期/旧代际拒写、完成回滚与 supersede；实际 GitHub 接口 fake + 真实本地 remote/linked worktree 的交付、响应丢失、断点/续租/清理测试；该批结果：clippy 通过、Store 132 通过、Nuwa 388 通过 / 1 ignored / 9 个 benchmark 测试被过滤，不是当前完整套件复测 |
| REQ-005, REQ-008, NFR-007 | DES-024 / ADR-024 | `nuwa/src/ci.rs` 严格 PR SHA 快照；`ci_watch.rs` 逐条 claim/续租、前台 `wait_for_delivery` 共用排程、关前/关后复核；`main.rs` 接入；`shanhaijing/src/migration.rs` schema v11、`lib.rs::finish_ci_watch_owned` 原子后果及来源精确 tombstone；`api/memory.rs`、`web/src/features/memory/` 证据与人工停用分离 | **已有本地聚焦结果**：Store 148 通过、clippy 通过；Nuwa `ci` 过滤 50 通过 / 371 filtered（最终 summary 修正前，非修正后复测）；`export_bindings` 72 通过；`npm.cmd run typecheck` 通过；memory 页面 6 通过。覆盖租约接管/旧代际拒写、SHA 变化/未知响应、原子回滚、精确撤销/迟到重放及手动启用不恢复证据；命令和边界见下，不宣称完整 Nuwa 通过 |
| NFR-003, NFR-004 | DES-025 / ADR-025 | `shanhaijing/src/migration.rs` schema v12、`config_state.rs` 保存事务/版本元数据/实例 fenced 不可变快照；`nuwa/src/config_activation.rs` 同事务 values/revision、启动 marker/全适用输入证明与三轴投影；`settings.rs` 受保护原子文件替换/迁移失效 marker；`api/settings.rs` 仅保存/不确定提交/显式候选；`serve.rs`、`main.rs` 真实启动接线及实际 provider resolver；`web/src/features/settings/ProcessConfiguration.tsx` 与设置/system/runner 逐实例展示 | 已编写迁移保留/回滚、跨 Store 保存、无值变更采用文件、一致 no-op、提交不明、GET 读快照一致性、密钥不泄露、三方 marker/精确输入、CLI 来源、pool 单进程/多实例及旧代际拒写、候选无 live fallback/换 host 要新 key、三轴 UI 回归。已有结果：Store 155、Nuwa 433 / 1 ignored / 9 benchmark filtered、ts-export 77（最后一致读取 helper 前，非修改后复测）；严格 clippy 通过；web typecheck/lint、409 测试/55 文件、build 通过。完整时点/限制见下及 docs/07 §D；最终聚焦复测由父任务另记，本次未运行检查 |
| REQ-003, REQ-006, REQ-007, NFR-003, NFR-007 | DES-026 / ADR-026 | `nuwa/src/instructions.rs` 显式 compatibility/strict 与 owner oracle 闭包；`verify/trusted.rs` 修复前权威冻结、同址导出/独立入口、完整性与 moved-base 原上下文重验；`verify.rs` candidate tree/精确私有排除；`pipeline.rs` pool 权威根冻结/补缺与统一准备；`guard/local_env.rs`、`guard/path_policy.rs` 真实递归 I/O 授权；`repair.rs` 实际环境接线；`deliver.rs` 交付边界复核；`check.rs` owner 预检；`benchmark/batch4.rs`、`assets/benchmarks/batch4-manifest.json` 密封 faux/sidecar，schema 仍 v12 | **已本地实现、限定检查完成（完整 workspace nextest 2026-10-28 通过）**：guard 26 通过（Windows junction 无 skip）；verify 30 通过早于 owner-seed，trusted 17/18 后 negative pathspec 修正及 seeded 聚焦通过。广域 Nuwa 曾 438 通过/1 失败/2 ignored/39 filtered；后续完整 Nuwa **483 通过/3 失败/2 ignored**。CLI 首次栈溢出，装箱后 sidecar **45/45 预期满足**，早于最后私有 seed cleanup 修正。Go build 输出污染已修、verify 35/Store 155/严格 clippy 与若干单独回归通过，完整 workspace nextest 2026-10-28 通过；并发 ownership-loss 未解边界见 docs/07 §D |
| REQ-004, REQ-005, REQ-008, NFR-003 | DES-027 / ADR-027 | `shanhaijing/src/lib.rs` 可选 Signal 元数据、`report_signal_with_source` 逐回执事务与兼容回归；`observations.rs` 字节验证、调用方授信/时间状态、迟到/有限去重、100 条保留与精确版本证据撤销；`migration.rs` schema v13 增量事务与不伪造历史，`lib.rs` 按接收时间 30 天 prune；`nuwa/src/main.rs` 显式 owner-attested CLI 与通用解析错误；`serve.rs` HTTP 不授信、通用 400/运行期部分接收；`api/issues.rs` 有界 observations 与部署/恢复 unknown；`web/src/features/issues/IssueObservations.tsx` 回执/未知状态展示 | **已本地实现并完成聚焦验证（disabled 撤销修正后）**：Store **173 全部通过（含迁移）**；Nuwa `ingest` 过滤 **11**、issues API **15**、`ts-export` **78** 通过（已知 serde 警告）；严格 clippy 通过；web typecheck/lint、IssueObservations + IssueDetailPage **23 测试**、build 通过。架构 **17 模块 / 6 crate / 2 条债**、追溯 **16 需求 / 27 设计、零缺口**通过；生成文件 whitespace 修正后 TS 重新导出及 diff 检查通过。父任务提供，本次仅文档；完整 workspace/web 未重跑，不覆盖 Batch4 历史失败，边界见下及 docs/07 §D |
| REQ-003, REQ-005, REQ-008, NFR-003 | DES-028 / ADR-028 | `shanhaijing/src/pr_slots.rs` IMMEDIATE 准入、delivery claim 权限/持久槽位、supersede 转移与 repo/PR URL 归并；`migration.rs` schema v14 增量/历史保守占位，`lib.rs::complete_delivery_owned_at` 创建/确认分时及原子消费、单调 run usage/独立 run；`nuwa/src/quota.rs` admission/最终 token 闸，`deliver.rs` prepush 预留/请求前幂等复核/响应丢失保留，`main.rs` work/fix 用量与无 attempt 配额待命/恢复；`api/projects.rs`、`api/runners.rs` additive 槽位与上报限额，`web/src/features/projects/`、`features/runners/RunnerCard.tsx` 分开展示 | **已本地实现，本地最终检查完成（2026-10-28）**：Store **187 通过**；delivery **43 通过**；quota **7 通过**；full workspace nextest `--no-capture` **1486 通过 / 2 skipped**（默认 capture 的 3 个 Windows deliver 超时见 docs/07 §D）；fmt/clippy/arch/traceability/vendored、TS export **78 通过**、web **425 通过** / build、faux benchmark **14/14** 通过；未执行生产迁移/部署、真实 GitHub/API、systemd 或付费模型。 |

## Batch1 证据边界（完成时的历史快照，所有权/清理契约仍有效）

- pending 恢复原子领取、每次最多处理一条，claim TTL 60s、每 10s 续租；PR 完成时 delivery/关联 issue/run/watch 同事务。runner checkout 绑定实际实例；独立 `fix --db` 默认 `shanhaijing.db`，须与 work 使用同一数据库，持有自己的 checkout claim 覆盖修复与网络等待，不伪造 runner。
- 远端孤儿删除只允许在查询明确证明零 PR 后以 expected SHA + `force-with-lease` 执行；不匹配/关闭 PR、畸形响应、分页不完整与未知结果均保留分支。本地清理须拥有 checkout 且匹配预期分支/tip；脏树 preflight 拒绝，stale/terminal 自动清理禁用。
- detached 池工作树保留逻辑 PR base，交付后安全恢复到预先固定的 detached HEAD，不切到 base 分支。pool teardown 保留脏树或附着分支的 worktree 证据，只对干净且 detached 的 worktree 调用不带 `--force` 的 `git worktree remove`；进程骤停可能留下 worktree，须人工复核，不自动清除现场。
- branch-only 路径在 push 前持久写入 `branch_only`，排除在 PR 自动恢复之外；崩溃后须人工复核，不因没有 PR 自动删除分支。只有 push 成功且经验证、checkout 安全恢复后，才进入终态 `branch_pushed`。
- schema v9 → v10 是 Batch1 当时目标，当前目标 v14；升级必须由所有者停止全部旧写者并一致备份 DB/WAL 与 env，迁移后的无 owner 旧 intent 仅在该停写前提下可安全领取。规程见 `08-onboarding.md`，**生产迁移未执行**。
- 本地限定范围结果：`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；`cargo test -p shanhaijing --offline` **132 通过**；`cargo test -p nuwa --bin nuwa --offline -- --test-threads=4 --skip benchmark::tests` **388 通过、1 ignored、9 个 benchmark 测试被过滤**。
- 测试使用 fake GitHub，而 Git/linked worktree 为真实本地操作；上述结果不构成 workspace、web、完整验证或真实 GitHub/生产部署验证结论，也不是 OS 沙箱验证，不保证外部副作用 exactly-once。

## Batch2 实现与聚焦证据边界（schema v11 历史完成快照）

- watch 精确身份为 `(slug, pr_number, head_sha)`；repo/fingerprint/run/PR URL 关联不可挪用，终态不因重复登记复活。CI claim 的 token/generation/expiry 与 `next_check_at` 独立，TTL 120s、每 10s 续租；后台至多 20 条逐条领取。前台仅等待所交付 PR/SHA，和 serve 共用租约/持久排程/处理器，不绕过未来检查或活跃 owner；超时/取消保留 watch，前台结束后的持续观察依赖 serve。
- 严格读取 state/merged/head 快照；未知响应、不可读、未完成或超时不是 CI 红灯。head 变动为终态 `head_changed`、不跟随新 head；green+open 继续观察，green+merged 可发布记忆，closed 即使绿也不发布。红灯关闭前重查 open/同 SHA 和当前交付归属，关闭后确认 closed/同 SHA 才允许适用的 issue 失败计数/重试；外部关闭/合并不自动重开/回滚。
- Store 在有效 claim 下同事务更新 watch、精确 run 结论、适用的 issue 收敛、记忆与必要审计，失败整体回滚；旧观察不误伤新交付。watch 的 green/red 映射为 run/API 的 success/failure。
- 记忆证据按 `(repo, fingerprint, source_kind, source_id, commit_sha)` 独立存内容并校验 PR URL；CI `ci_green` 的 source id 为 watch id，本地 `local_verification` 为 run id。red/closed/head_changed 留下该精确 watch/SHA/PR 来源的 tombstone（撤销记录，即使未曾绿），不抹除其他提交/独立来源；迟到 green 不能复活或刷新已撤销来源的摘要。直接红灯不创建/淘汰摘要，普通摘要裁剪保留证据/tombstone；显式删除则同事务移除摘要与证据。
- `disabled` 是独立人工偏好，自动写入不清除，启用不恢复证据；只有有效、未撤销的已知验证来源且未手动停用才注入。迁移旧记忆 `legacy_unknown` 可见但未验证、不可注入；API/页面分别透出有效性、来源及撤销时间/原因。每仓裁剪最近 50 条未停用摘要，停用摘要另保留，不按该数量裁剪证据。
- **GitHub close 残余 TOCTOU**：远端不支持原子 expected-head close，前后快照/本地租约不能消除与 push/merge 的检查后使用竞态；除非不执行自动 close，否则该限制无法消除。本批未新增禁用自动 close 的配置开关，不保证绝不误关或外部 exactly-once。
- **已有结果，本次文档更新未重跑**：`cargo test -p shanhaijing --offline` **148 通过**；`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；Nuwa `ci` 过滤 **50 通过、371 filtered**（最终 summary 修正前，不能充当修正后复测）；`export_bindings` **72 通过**；`npm.cmd run typecheck` 通过；memory 页面 **6 通过**。不外推为完整 Nuwa/workspace/web 或 lint/build/E2E 通过。
- Batch2 当时迁移目标 v11，当前 v14（Batch6）：先停全部旧写者与自动重启入口、一致备份 DB/WAL 与 env，再在隔离副本验证并由新版打开 Store，不能新旧混跑或让旧版直接写升级库。旧 watch/intent 无 claim 只在停旧写者前提下安全；回滚恢复源 schema 一致备份/配套 env 及对应二进制，不能仅改版本号。规程见 docs/08；**未执行生产迁移、部署、真实 GitHub/API 或付费模型测试**。

## Batch3 实现与证据边界（该批 schema v12，本地实现）

- 全部 29 键 schema `requires_restart=true`；保存不改启动 env_store、Secrets、live auth 或会话。schema v12 只新增 `config_saved` 版本元数据和 runner 脱敏非密钥 startup JSON；随机 UUID revision 不是秘密、内容/密钥 hash，审计只记键名。文件/备份采用受保护同目录 tempfile + sync + persist 原子替换（Windows 同样适用，Unix 首字节前 0600，Windows ACL 所有者负责）。
- 同 DB 的保存由 `BEGIN IMMEDIATE` 串行，GET values 与 saved revision 取同一事务快照；文件与 DB commit 不原子。替换后失败/结果不明返回 `config_save_indeterminate`，重新读取复核，不盲目回滚；mismatch persistence unknown 且不能证明 activation。非空成功 PUT 采用未版本化/不一致文件，即使无值变化也新建 revision；一致 no-op 保留，空 PUT 不建版本。
- 启动环境 `NUWA_CONFIG_REVISION` = 文件 marker = DB revision 且该角色全部适用可写 schema 输入精确相等（含缺失/存在和密钥，仅内存比较）才能证明 loaded_revision。缺环境 marker 的外部注入即使值相同也 unknown，不用文件 marker/最新 runner/心跳代替证明。显式 CLI 可以改变 effective 值而保留 managed input revision 证明，逐键 source=cli，active 不等于 CLI 值与文件相等；同命令重启不消除覆盖。
- serve/work 真实启动路径记录实际解析/default/derived 与 provider/model/base，不用旧标签当 resolver 结果。一个 OS 进程一个 UUID、pool 捕获一次且共享；runner 租约实例独立，快照仅当前有效实例首次写入、接管清空、心跳不覆盖。三轴 persistence saved/unversioned/unknown、activation active/stale/unknown、liveness online/offline/unreported 分离，离线 active 只保留历史证据，未上报/缺快照不猜测；legacy restart_pending/effective_diff additive 保留但弃用，无 UI 激活权威。
- 所有者手工编辑文件必须移除/失效 marker；保留 marker 的改写不能认证/可靠检测，这是 owner-controlled 来源约定，不是密码学认证。migrate-env 真正重写时移除 marker，只能停写离线维护，不得与保存并发。候选探测显式 saved/draft（旧请求默认 draft），只读文件+默认/草稿，不借用 live env/key；换 zai base URL 要显式草稿 key，不证明激活。
- **已有结果、时点不可混用**：Store 155 通过；Nuwa 433 通过 / 1 ignored / 9 benchmark filtered；ts-export 77 通过，均为最后一致读取 helper 修改前（DTO 契约未变），不是该 helper 修改后复测；严格 clippy 通过。web typecheck/lint、409 测试/55 文件、build 通过；改前完整 web 400/55 也绿、未复现历史等待，不能宣称历史等待根因修复。架构 17 模块/6 crate/2 条债、vendored 14 文件通过，追溯 16 需求/24 设计是添加 ADR-025 前结果，**不是新增 ADR/DES 后复测**。父任务另记最终聚焦 recheck，本次仅文档、不执行测试/构建/检查。
- Batch3 当时生产目标 schema v12，当前 v14 见 Batch6；全部旧写者与自动重启入口暂停、DB/WAL 与 env 一致备份、隔离副本验证和恢复规程见 docs/08；迁移不编造旧启动证明。**未执行生产迁移/部署、完整 workspace、跨平台、真实 GitHub/API、systemd 或付费模型验证**；基线仍 v0.17.0、未升版发版。

## Batch4 实现与证据边界（schema 仍 v12，本地实现、限定检查完成）

- 默认 `verification_policy=compatibility`，无 oracle 明确 `legacy_untrusted`，不静默切 strict；strict 缺 owner oracle 在模型/本地交付分支/staging/push/PR 前拒绝，任何策略配置了 oracle 都必须通过、不降级。`nuwa check --json` 的 `verification` 明示 owner baseline/legacy/invalid；交付经既有 `runs[].verification_detail` 文本与 PR 透出 TRUST 摘要，不新增 owner 管理 API。
- owner 显式维护字面量入口与 `protected_inputs` 完整依赖闭包；模型前从 tracked base 或 owner 预置 untracked 文件冻结 bytes/type/mode/manifest 到 checkout 外。入口从冻结目录运行，base/candidate 重置同一物理 target/home/tmp/cache，无 Git 元数据/侧别标签，最小固定 env。预期 bug exit 1、通过 0、意外 setup 故障 2+；候选 failing_check 与适用 base-green 深检额外必须绿。闭包 owner 负责，任意错误 oracle 的 exit 1 不是业务 bug 的数学证明。
- pool 先从权威配置根冻结再补缺失私有输入；篡改不恢复后接受。private untracked protected 路径在 staging/tree/diff 精确一致排除，保留 `:(exclude)nuwa.repo.json`；tracked oracle 不排除，内容/mode 篡改拒绝。移动 base 保留原 VerificationContext 重新验证，不采用新 oracle；完整 candidate tree 绑定交付。
- 禁改可读不可写，内建与配置禁读规则禁读禁写；真实 LocalExecutionEnv 覆盖递归读取和文件/目录操作，不仅 hook。拒绝 checkout 逃逸、symlink/junction/reparse。shell/检查/候选代码仍有宿主用户权限，路径检查有 TOCTOU/hardlink 局限，**不是 OS 沙箱**。现有 Docker 不是执行/交付权限分离；独立低权限用户、无宿主 HOME/发布密钥挂载、严格写入/网络出口限制需要平台授权，未实现/未验证。
- `nuwa benchmark batch4 --output <新建临时 JSON>` 独立 faux：15 场景 × 3 trials、turn cap 12，密封 manifest 的 fixture/oracle 计算摘要与固定 base recipe；只写安全元数据/摘要、声明/实际结果 sidecar，不打开默认业务 DB、不跑 live。覆盖真正修复、删除/弱化断言/helper 替换/mode、吞异常/reproducer-only/branch-SHA/noop、always-pass/缺 oracle-input-command/setup failure/timeout；不是模型能力测评，声明不等于实际结果，密封摘要不代表已执行验收。
- **已有证据及失败不可抹去**：guard 26 通过（Windows junction 无 skip）；verify 30 通过早于 owner-seed，随后 trusted 17/18，negative pathspec 修正后 seeded 聚焦通过。delivery ownership-loss 隔离 1 通过，仅改每场景 setup deadline、1s 取消不变；广域 Nuwa 曾 **438 通过/1 失败/2 ignored/39 filtered**。CLI 首次栈溢出，装箱后 **45/45 预期满足**，早于最后私有 seed cleanup 修正。后续完整 Nuwa **483 通过/3 失败/2 ignored**：Go build 输出污染已修且回归通过，另两条 private-oracle delivery/双 worker pool 单独重跑通过，**并发负载 ownership-loss 根因未证明解决**；修后 verify 35、Store 155、格式/严格 clippy 通过不等于完整 Nuwa 全绿。详细时点保留在 docs/07 §D，本次不运行检查。
- 没有生产、真实 GitHub/API、systemd、付费模型、部署或本批 Unix 测试；不以旧批次/Windows 聚焦结果代替这些验证。Batch4 本身不新增 schema；后续 schema v13/v14 当前状态见下。

## Batch5 实现与证据边界（schema v13，本地实现并完成聚焦验证）

- ADR-027 / DES-027：`Signal` 可选且默认缺省的 `observed_at`（epoch 毫秒）、`environment`、`release_sha`、`source_event_id` 不改变指纹/旧 payload 回归。服务端接收时间与采集时间独立，回执存来源授信、时间状态、处置/重复关联；每问题单按 `received_at_ms DESC, id DESC` 保留 100 条，另按接收时间清理超过 30 天记录，不按采集时间清理。
- environment/release/trusted source 各 128 UTF-8 字节、source event 256 字节；拒绝空白/控制字符/超限，不截断身份。缺失/非正/未来时间保留为 missing/invalid/future，合法未授信为 untrusted；只有合法时间加调用方授信可授权。CLI `nuwa ingest --trust-observations <source-id>` 是 **OWNER ATTESTATION**：所有者核验导入时钟、来源事件 ID 与环境/精确 release 关系；HTTP bearer/token 鉴权永不授信，payload 不可自授信，不是密码学证明。
- merged/rejected 的 delayed 仅限可信有效采集时间严格早于已知正值终态 `state_changed_at`；相等、终态时间未知、缺失/无效/未来/未授信时间保留原回归规则。重复识别只限保留历史的同 fingerprint、可信 source/event 且采集时间/环境/release 精确匹配；冲突拒绝，匹配记 duplicate。每条成功接收仍计 occurrences/接收日 daily，不宣称全局 exactly-once。
- 适用的可信有效回归观察有明确环境、完整 40/64 位十六进制 release，且采集时间严格晚于 evidence.recorded_at，撤销匹配 repo/fingerprint/SHA 的证据；delayed/duplicate/muted 不作为新撤销依据。**即使记忆已手动 disabled 也撤销**，不清除此开关、不波及独立提交，重新启用不复活证据。此关系由所有者授信，不代表已接入部署/健康系统；API/UI 的 deployment_status/recovery_status 固定 unknown，merged 不等于 deployed/recovered。
- 单回执的计数/状态/观察/适用撤销同事务。HTTP 元数据错误为有界通用 400，不反射 payload；预校验不意味着 JSONL 批原子，逐回执提交可能在运行期失败前接受部分，已提交回执不回滚。schema v12 → v13 建表/索引/版本增量同事务，不从旧问题单伪造 observations；操作须所有者授权、停全部旧写者/自动重启、一致备份 DB/WAL/env 并先在隔离副本验证（docs/08），**未执行生产操作**。
- **验证时点与边界**：上方 DES-027 的最终本地聚焦结果均在手动 disabled 仍撤销证据修正后，命令与完整时点见 docs/07 §D；Batch5 的最终 workspace/web 套件已在 2026-10-28 最终检查中通过；不覆盖 Batch4 历史失败记录及其聚焦复测，不宣称跨平台、真实 GitHub/API、systemd、付费模型或部署验证通过。Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28）。

## Batch6 实现与证据边界（schema v14，本地实现、最终检查完成（2026-10-28））

- ADR-028 / DES-028：repo + UTC 准入日的 IMMEDIATE 原子槽位，权限核验 delivery claim token/generation/expiry；reserved/requested/ambiguous/consumed 计数，released 不计。claim TTL/释放、重启/prune 不退款，午夜/接管复用原槽位；仅请求前安全 supersede 原子转移，确认同 repo + PR URL 归并。准入日、API created_at 与本地 confirmed_at 分离，旧 GitHub 创建时间 unknown。
- work/pool/独立 fix 均 prepush 预留、请求前最终幂等检查；预 push PR 拒绝释放 issue、不计 attempt，postpush 门禁保留 branch/intent/run/槽位直至确认不存在。响应丢失/取消不退款，精确 head/base 恢复，不保证 GitHub 外部 exactly-once。v13 → v14 表/索引/历史归并/版本同事务；未确认 requested/ambiguous 保守占位。v9–v14 停全部旧写者/自动重启、WAL 一致备份、pending 核对/隔离副本与禁止混跑/直接降级/TTL 猜测退款见 docs/08。
- token admission 为已知仓库用量 `>=` 拒绝，final 个体/仓库为 `>`；当前 run cached/uncached 分量各取持久/报告最大值、去重且含跨窗口修复，失败 work/独立 fix 单调记账。按 run started_at 聚合，monthly 滚动 30 天非自然月；运行中/并发/provider 部分或未知 usage 可超支，无 token reservations/费用硬 cap，不能撤回花费。
- projects/runners additive pr_slots_today 与 prs_today 历史创建/本地观察分离；runner daily_pr_limit / project pr_limit（上报观测最大值）不是中心统一限额，各创建进程配置可能不同。仅有 Store 186（timestamp/CLI refinements 前）、quota 3（refinements 前）通过；delivery 42 项及全部最终 gates 待确认。本次仅文档，无新增检查通过、生产操作、付费模型或平台隔离证据。

## 六批顺序与当前状态

Batch1 所有权/schema v10 **已完成** → Batch2 SHA 绑定 CI 与记忆/schema v11 **已完成** → Batch3 配置/schema v12 **已本地实现** → Batch4 可信验证/路径护栏 **已本地实现、限定检查完成，该批不增 schema** → Batch5 observations（信号观察）/schema v13 **已本地实现并完成聚焦验证** → Batch6 reservations（预留）/schema v14 **已本地实现并完成本地最终检查（2026-10-28）**。

## 尚未实现／明确延期

以下内容不伪装为已实现，也不建立虚假的“完成”证据：

- “Batch2–Batch6 均尚未实现”是 Batch1 完成时的历史结论；当前 Batch1/2 已完成、Batch3/4 已本地实现，Batch5/schema v13 已本地实现并完成聚焦验证，**Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28）**。最终本地验证已完成（2026-10-28）；不预记更高 schema、生产迁移或部署。
- A-3 的单进程池与镜像定义已记于 `docs/07-open-items.md` §D（2026-10-26），不是 Batch4 验收或 Batch5–Batch6 的完成证据；registry 发布与生产运维验证仍由所有者执行，本批不记为完成。
- `docs/07-open-items.md` A-4：现场预定义验证探针协议——阻塞于现场 SDK（本轮排除）执行预定义脚本并回传结果。
- `docs/09-web-console-design.md` §19：SSE、GitHub OAuth、项目级权限等后续候选。
