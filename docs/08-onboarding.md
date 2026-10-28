# 08 · 接入手册

状态：main 基线 v0.17.0；2026-10-27 Batch1/2 已完成，Batch3 已本地实现，Batch4 已本地实现并完成限定检查，Batch5 observations/schema v13 已本地实现并完成聚焦验证（ADR-027 / DES-027），Batch6 reservations/schema v14 已本地实现并完成本地最终检查（2026-10-28，ADR-028 / DES-028）。已有验证时点与限制见 docs/07 §D；完整 workspace nextest `--no-capture` 已本地通过，不代表 Linux CI/平台验证或已发布；未升版，生产迁移/部署未执行。

批次顺序：Batch1 所有权/schema v10 已完成 → Batch2 SHA 绑定 CI 与记忆/schema v11 已完成 → **Batch3 配置/schema v12 已本地实现** → **Batch4 已本地实现、限定检查完成（该批不增 schema）** → **Batch5 observations（信号观察）/schema v13 已本地实现并完成聚焦验证** → **Batch6 reservations（预留）/schema v14 已本地实现并完成本地最终检查（2026-10-28）**。最终本地检查证据见 docs/07 §D。

把一个自有系统接入 Nuwa = 下面四步。全程不改 nuwa 本体一行代码（ADR-006）。

## 0 · 服务端一次性准备

生产形态 = 一台 Linux 服务器承载 serve + work + SQLite（ADR-005）。已验证的部署（腾讯云 OpenCloudOS 9.4 / 2C7.5G）：

```
/opt/nuwa/
├── bin/nuwa              # release 产物（v0.1.2+，含 serve 鉴权/sync/ci gate）
├── repo/spike-target/    # 目标仓工作副本（含 nuwa.repo.json）
├── sessions/             # 修复会话转录
├── env/{gh-token,zai-key,ingest-token}
└── nuwa.env              # systemd EnvironmentFile（三把 token）
```

- systemd 单元：`nuwa-serve`（`serve --listen 0.0.0.0:7321`）+ `nuwa-work`（`work --loop --interval 60 --pr --runner-id <host:repo> --service-unit nuwa-work --sessions /opt/nuwa/sessions`），均 `Restart=on-failure`。**`--sessions` 必须传绝对路径**：相对路径写入 `runs.session_dir` 后，转录接口（只接受绝对路径）读不到失败/中断 run 的会话。相对路径在启动时也会被解析为绝对路径，但显式传入可避免依赖进程 cwd。
- 已知噪音：ubuntu 构建的产物在旧 glibc 上有 weak-version 警告（不致命）——release 另提供 `nuwa-linux-x64-musl` 静态产物，无 libc 依赖
- 腾讯云控制台安全组：放行 **TCP 7321**（来源限采集器网段）
- 传输注意：CN→GitHub 大文件（>12MB）下载会被掐断——release 产物下载用**分块 Range 循环**，或本机经 SSH 直传（SSH 隧道稳定）

| 项 | 说明 |
|---|---|
| 工具链 | Rust（cargo 1.93+，构建 nuwa）、Git；每个目标仓库的技术栈工具链（runner 要能跑它的测试命令） |
| `NUWA_GITHUB_TOKEN` | 细粒度 PAT：目标仓勾 **Pull requests: Read and write** + **Contents: Read-only** + **Actions: Read-only**（CI 结果轮询走 `actions/runs` 端点；细粒度 PAT 无 "Checks" 权限项，check-runs 端点对私有仓 403 不可用）。push 走本机 Git 凭证，token 无需写内容 |
| deploy key | 目标仓库的只读+写分支 deploy key，配进 runner 的 ssh-agent（ADR-004：服务端持有，最小权限） |
| `ANTHROPIC_API_KEY` | 可选；不配则只能 `--provider faux`（离线验证管道，不产生真实修复） |
| `ZAI_API_KEY` | 智谱 coding plan（`--provider zai`）：base 默认 `https://open.bigmodel.cn/api/coding/paas/v4`，默认模型 `glm-5.3-flash`；`--model`/`--base-url` 可覆盖 |

## 1 · 现场装采集 SDK（青鸟）

Go 系统（青鸟公共模块，**v0.2 五挂点**，行为规范见 qingniao-go 仓 `SPEC.md`）：

```bash
go get github.com/lomehong/qingniao-go@v0.2.0
```

> v0.2.1 补丁（兜底落盘 / 关闭不 panic / 客户端 panic 指纹）已在 SDK 仓落地，
> **待打 tag 并 push 后**把上面这行换成 `@v0.2.1`。

SDK 演进流：qingniao-go 仓改动 → 打 `vX.Y.Z` tag → 目标仓 `go get -u github.com/lomehong/qingniao-go`。服务器/CI 经公共 module proxy 零配置拉取；本机联调未发布的 SDK 改动时用 `go work`（work 文件不进 git）。

```go
import "github.com/lomehong/qingniao-go"

q := qingniao.NewFromEnv(qingniao.Meta{
    Repo:    "example.com/your-repo",
    Service: "api",
}) // Sink 缺省从 NUWA_INGEST_URL / NUWA_INGEST_TOKEN 推导
defer q.Close(context.Background())

// 信号 1：panic——捕获、同步上报后原样重抛（不改变失败语义）
defer q.Capture(func() { run() })

// 信号 2：goroutine panic——安全包装（上报后重抛）
q.Go(func() { watchTick() })

// 信号 3：显式错误——异步队列（high）
q.Error("cache warmup failed", err)

// 信号 4：错误日志——挂进任何已有 slog.Logger（Warn 级起成为信号）
logger := slog.New(q.Handler(slog.LevelWarn))

// 信号 5：失败自检——周期探针，失败上报 high
stop := q.Check("db-ping", 30*time.Second, ping)
defer stop()

// 不可恢复：同步上报 → flush → exit(1)
if corrupt { q.Fatal("config corrupt", err) }
```

投递语义（SPEC §2/§3/§6）：panic/Fatal 走**同步**通道（进程将死，上报必须在死前完成）；
其余走**有界异步队列**（默认 256，满丢最旧），绝不阻塞主流程。同指纹节流窗口默认
60s（服务端聚合是第二道，不是替代）。`Redact` 钩子做源头脱敏；`FallbackFile`/
`Stats()` 保证信号可审计。v0.2.1 起：零配置 sink 自带兜底文件（端点缺失直接落盘，
投递失败也落盘 `<缓存目录>/qingniao/fallback.jsonl`），投递失败但已落盘时 `Send`
返回哨兵 `ErrFallbackWritten`（`errors.Is` 判断，**信号没丢但没送达**），
`Stats()` 的 `Reported`/`Fallback`/`Dropped` 三者语义互斥；关闭与并发上报不再 panic。
panic 信封带客户端指纹（与服务端 Go 栈算法逐字一致，两侧 golden 互钉；服务端当前自算）。
信封 schema = `qingniao/v1`，是 ingest Signal 的超集；新增字段不破坏旧服务端。

非 Go 系统不需要 SDK，直接 POST 同一个信封：

```bash
curl -sS -X POST "$NUWA_INGEST_URL/ingest" \
  -H "Content-Type: application/json" \
  -H "X-NUWA-Token: $NUWA_INGEST_TOKEN" \
  -d '{
    "schema":"qingniao/v1","repo":"example.com/your-service","language":"python",
    "service":"worker","title":"panic: queue empty",
    "severity":"high","message":"IndexError: list index out of range",
    "stack_trace":"Traceback (most recent call last):\n  ...",
    "ts_unix_ms":1790000000000
  }'
```

字段最小集是 `repo/title/severity/message/stack_trace`；其余字段可省略。

可选字段 `log_window`（字符串数组）：采集器提供的现场日志窗口。服务端会随问题单落库（schema v7，保留前 40 行、每行 2000 字符），并在修复 prompt 的 `LOG WINDOW` 段展示给 agent——这是 agent 除堆栈外唯一的现场上下文。青鸟 SDK 目前尚未发送该字段，服务端已就绪。

### Batch5 · 信号观察与所有者授信（ADR-027，schema v13）

旧信封继续可用，指纹算法与没有观察元数据的回归行为不变。以下字段可省略/null；不要把 SDK 的其他时间字段或 HTTP 鉴权当作 `observed_at` 的可信证明。

| 字段 | 含义与限制 |
|---|---|
| `observed_at` | 采集时间，epoch 毫秒整数；与服务端记录的 `received_at_ms` 分开。缺失为 missing，非正为 invalid，晚于本次接收为 future；无效/未来整数保留但不能授权 |
| `environment` | 环境身份，最多 128 UTF-8 字节 |
| `release_sha` | 发布版本声明，最多 128 UTF-8 字节；只有完整 40/64 位十六进制 SHA 可参与精确版本证据撤销，统一小写，不接受短 SHA/分支名代替完整身份 |
| `source_event_id` | 来源事件身份，最多 256 UTF-8 字节 |

提供的字符串不得为空白、含控制字符或超长；**拒绝而不截断身份**。调用方授信来源 `source-id` 同样限 128 UTF-8 字节且适用上述规则，不是秘密或 bearer token，不要填凭证。

**默认不授信**。只有所有者核验导入时钟、来源事件 ID，以及环境与精确发布版本关系后，才显式使用以下入口。这是 **OWNER ATTESTATION（所有者确认）**，不是“让导入成功”的开关，也不是密码学来源证明：

<augment_code_snippet mode="EXCERPT">
````text
nuwa ingest --signal /path/to/reviewed.jsonl --db /path/to/shanhaijing.db --trust-observations owner-reviewed-import
````
</augment_code_snippet>

- **HTTP 永不授信观察**：即使 bearer/token 鉴权通过，`POST /ingest` 仍按未授信来源处理；JSON 自报 `trusted_source` 等字段不能建立权威，不能靠 payload 或令牌阻止正常回归。合法采集时间加调用方授信才为 trusted，否则为 untrusted（或 missing/invalid/future）。
- **迟到不等于回归**：merged/rejected 只有合法可信采集时间**严格早于**已知正值终态 `state_changed_at` 才记 delayed、不重开。时间相等、终态时间未知、缺失/无效/未来或未授信时间仍沿用原回归规则；ignored/escalated 不自动重开。
- **有限去重**：仅保留历史内同 fingerprint、可信 source/event 且采集时间/环境/release 元数据精确匹配才记 duplicate，元数据冲突拒绝。每条成功接收（包括 delayed/duplicate）仍累计 occurrences 与接收日 daily；没有全局 exactly-once 保证，裁剪后重放可能不再被识别。
- **回执不是无限审计库**：每问题单按 `received_at_ms DESC, id DESC` 保留最近 100 条，另按接收时间清理超过 30 天记录；不是按采集时间保留。旧问题单不补造回执；空历史不证明从未上报。
- **元数据错误与部分接收**：HTTP 返回有界通用 400，不回显 payload 或元数据值。解析/元数据预校验不等于 JSONL 批原子；逐回执事务提交，后续运行期失败可能留下已提交部分，响应会说明数量。先复核接收结果再决定重试，不假定整批失败即零写入。
- **证据撤销独立于人工停用**：适用的可信有效回归观察有 environment、精确完整 40/64 hex release，且采集时间严格晚于 evidence.recorded_at，撤销匹配 repo/fingerprint/SHA 的证据；delayed/duplicate/muted 不作为新的撤销依据。记忆已手动 disabled 也仍撤销，不清除此开关、不影响独立提交；重新启用不复活证据。
- **部署/恢复未知**：详情 API/UI 的 `deployment_status`、`recovery_status` 均为 unknown；未接入部署或健康系统，merged、CI green、上报 release 都不等于 deployed/recovered。手动 disabled 撤销修正后的本地聚焦验证已完成，证据与完整套件未重跑的边界见 docs/07 §D；本次仅文档、未运行测试。

## 2 · 目标仓库放指令文件

仓库根放 `nuwa.repo.json`（并建议加进该仓库 `.gitignore`——交付面已排除它，双保险）：

```json
{
  "repo": "example.com/your-repo",
  "failing_check": { "command": "go test ./...", "timeout_secs": 600 },
  "reproduce_check": { "command": "sh .nuwa/repro.sh", "timeout_secs": 300 },
  "forbidden_paths": [".github/", "deploy/", "*.pem", "go.mod", "go.sum"],
  "max_attempts": 3,
  "draft_first_n": 0,
  "skills": ["rust-cargo"],
  "notes": "给修复 runner 的自由文本说明（规范、禁区原因、注意事项）"
}
```

- `repo` 必须与采集信封里的 repo 一致；runner 只领自己 repo 的单（多 runner 共享库时也不会互抢）
- `failing_check.command` 是"当前失败、修复后必须过"的回归门；`timeout_secs` 默认 600
- `reproduce_check` 可选：**仅无 owner oracle 的 compatibility 路径**要求它在 base exit 1、分支 exit 0。没有配置时，`failing_check` 的 base 红可作旧式复现；base 绿则要求候选自包含 `.nuwa/repro.sh`（Windows `.nuwa/repro.cmd`）。这是较弱 `legacy_untrusted` 证据，不可作为 owner-trusted 证明；失败不交付，issue 重试/忽略策略见下
- **验证模式在模型前确定**：配置 owner oracle 时先冻结并证明 owner baseline exit 1，不要求模型建立权威；仅 compatibility 无 oracle 时探测 base `failing_check`，base 绿提示写 reproducer，配置了 `reproduce_check` 则用旧式 configured 模式。缺命令、超时与 setup exit 2+ 不算已复现
- **Go 深检三角**：问题语言为 `go` 且 base 根目录有 `go.mod` 时，验证门在 red/green 之外再跑 `go vet ./...` 与 `go build ./...`。base 绿则分支必须绿；base 红/超时/工具缺失则跳过并写入证据。深检不能替代复现门（ADR-021）
- `forbidden_paths` 为禁写 glob，内置文件工具仍可读；真实文件环境按 checkout 范围和每个路径祖先授权，交付再对完整最终 diff 做 glob 终检。`denied_read_paths` 另设禁读且禁写 glob，内建凭证/Git/私有验证/会话/DB 禁区不可因此取消；递归 list/find/grep 也受约束，不是只检查顶层工具参数。shell/候选代码不是 OS 沙箱
- `max_attempts`：problem-level 失败累计到阈值后自动 `ignored`，避免自动重试循环
- `draft_first_n`：该仓前 N 个 PR 以 draft 打开；0 保持 ADR-003 的非 draft 默认
- `skills` 可选：限定注入哪些修复技能（名字见下），留空则按问题语言与失败命令自动选择，最多 3 个

### Batch4 · 所有者可信验证与路径护栏（ADR-026）

oracle 是由所有者维护、独立判断目标缺陷的检查入口，而不是模型刚写的复现脚本。现有配置不自动升级：`verification_policy` 省略时为 `compatibility`，无 oracle 的证据明确为 **`legacy_untrusted`**。要启用严格门禁，由所有者准备完整 oracle 后**显式**配置 `strict`；缺 oracle 会在模型、创建本地交付分支、staging、push、PR 前拒绝。无论 strict/compatibility，只要配置了 oracle 都必须通过，失败不降级。

以下是最小 Python 配置示例（不包含 oracle 实现）；先由所有者创建列出的文件，再运行预检（Windows 通常将 `python3` 换为 `python`）：

<augment_code_snippet path="nuwa.repo.json" mode="EXCERPT">
````json
{
  "repo": "example.com/your-repo",
  "failing_check": { "command": "python3 smoke.py", "timeout_secs": 60 },
  "verification_policy": "strict",
  "trusted_verification": {
    "check": { "command": "python3 -I -B checks/owner_oracle.py", "timeout_secs": 60 },
    "protected_inputs": ["checks/owner_oracle.py", "checks/expected.json"]
  }
}
````
</augment_code_snippet>

| 配置项 | 精确契约 |
|---|---|
| `verification_policy` | `compatibility`（默认）或 `strict`；不是 env/控制台热更新开关 |
| `trusted_verification.check` | 与 failing_check 同型：`command`、可选 `target`、`timeout_secs`（默认 600，必须大于 0）。当前 trusted 启动不使用 `target` 字段；入口用 `NUWA_VERIFY_ROOT` 定位被测树 |
| `check.command` | 字面量受保护可执行文件，或 `python/python3`（含 `.exe`）、`node`（含 `.exe`）、`sh/bash/ruby/perl/cmd`（cmd 含 `.exe`）启动列在 protected_inputs 中的入口。不能是 `go test`/测试发现、管道/重定向、shell 拼接或 `-c`/`-m`/`-e`/eval 内联程序。准备/断言放进 owner harness 文件；Unix 直接入口须有可执行 mode |
| `trusted_verification.protected_inputs` | 必填、非空、无大小写重复的**完整文件清单**，含入口及全部 helper/fixture/config 依赖。仅 `/` 分隔的仓库相对字面路径，不接受 glob、绝对路径、`..`、`.git`、链接/reparse 或特殊文件；不会自动扫描补齐依赖 |
| `denied_read_paths` | 可选 glob 列表，默认空；与内建禁区合并，禁读且禁写。`forbidden_paths` 仅禁写；protected_inputs 自动禁改，**不自动保密**，私有内容还需显式加入禁读路径或采用内建私有目录 |

1. **建立权威与闭包**：tracked 配置/输入固定到 repair base；私有 untracked 配置来自 owner 权威副本，所需 untracked 输入必须由 owner 在权威配置根预置，不能由模型补齐。所有文件模型前冻结 bytes/type/native 与 Git mode/manifest 到 checkout 外。owner 必须穷举所有验证依赖；入口不得从候选加载可变断言/helper。闭包完整性不能自动证明。
2. **入口如何运行**：工作目录是外部冻结 oracle 目录；从环境 `NUWA_VERIFY_ROOT` 取得候选根，使用绝对文件路径启动被测程序。base 与候选轮流导出到**同一物理 target 路径**，每侧重建 target/home/tmp/cache，无 Git 元数据、branch/base 侧别标签；入口不从候选目录启动。最小环境仅保留经过筛选的绝对工具链 PATH、必要 Windows 系统项、私有 HOME/temp/cache 与固定 locale 等，不继承代理、Git/模型凭证、`PYTHONPATH`、候选 env 或 `NUWA_AGENT_ENV_ALLOW` 扩展；这仍不是 OS 隔离。
3. **退出码必须由 owner 设计**：预期缺陷/断言不符返回 **1**，全部通过返回 **0**，缺依赖、配置/解析、启动及意外异常返回 **2+**。模型前 baseline 必须 exit 1；候选 owner 必须 exit 0，候选 `failing_check` 和适用的 base-green 深检也须绿；缺输入/命令/超时均拒绝。任意错误程序也可能退出 1，因此 red→green 不是业务正确性的数学证明，oracle 质量仍由 owner 负责。
4. **pool/移动基线**：先从权威配置根冻结全部输入，再在模型前向工作树补缺失私有文件，不重读候选为权威、不覆盖已有不一致输入。合并/移动 base 后用**原 VerificationContext/原 oracle**重新验证新 base 与候选；新 base 改动 tracked oracle 内容/mode 或改为跟踪私有输入均拒绝，不自动采用新 oracle。
5. **交付与保密**：staging、candidate tree、diff 一致精确排除私有 untracked protected 文件，不扩大为整目录排除，保留 `:(exclude)nuwa.repo.json`。tracked oracle 不被排除，篡改仍拒绝；不得靠排除隐藏测试改写。内置工具授权和导出拒绝链接/junction/reparse；冻结内容及私有原始检查输出不进入公开证据。
6. **如何读证据**：`nuwa check --json` 顶层 `verification` 为 `owner_trusted: baseline red`、`legacy_untrusted` 或 `invalid`（错误仍非零退出；解析/加载失败可先退出而无报告）。这只是 base 预检，不证明候选通过。交付 `verification.txt`/PR 与问题详情 API `GET /api/v1/issues/{fp}` 的 `runs[].verification_detail` 文本含 `MODE`、`TRUST`；TRUST 包含 `base`、`candidate_tree`、`owner_config`、`manifest`、`source`（`tracked_base`/`owner_seeded`）、`owner_exits`。没有新增结构化 oracle 管理 API，也不将模型 `OUTCOME` 当证据。

**安全要求与未实现部分**：内置文件工具由真实 `LocalExecutionEnv` 执行授权（含递归读取、目录改动），不只是 hook；但 shell、检查命令与候选代码仍有宿主用户权限。路径复核有 TOCTOU，hardlink 也不能靠 symlink/reparse 检查实现 inode 隔离。现有 runner Docker 不等于执行/交付权限分离；独立低权限执行用户、无宿主 HOME/发布密钥挂载、严格可写范围/网络出口需要平台授权，**尚未实现/验证**。不要把 env 白名单或外部冻结目录称为沙箱。

### 修复技能（可选）

内置技能随二进制分发（`rust-cargo` / `node-toolchain` / `go-test` / `python-pytest`），按问题单的 `language` 与 `failing_check.command` 确定性选择后追加到 runner 系统提示，单段总长有上限。仓库可放自己的技能覆盖或补充：

```
<repo>/.nuwa/skills/<name>.md
```

```markdown
---
name: rust-cargo
version: 2
description: 本仓 Rust 修复路径
languages: rust
matches: cargo, nextest
---

# 正文（Markdown，最多 3000 字符进入提示）
```

- 同名仓库技能覆盖内置版本；`version` 必须是 ≥1 的整数，会随 `name@version` 记录到决策 trace
- frontmatter 只接受 `name/version/description/languages/matches`，未知键报错；`nuwa check` 会把坏技能文件升级为 error，运行时则跳过并告警
- 技能是排查建议，不改变护栏：禁改路径、验证门、分支命名空间仍然强制

接入前先跑预检（不消耗 LLM、不创建分支）：

```bash
nuwa check --instructions /path/to/your-repo/nuwa.repo.json
nuwa check --instructions /path/to/repo/nuwa.repo.json --json
```

预检会校验 JSON、repo 身份、禁改/禁读 glob、`max_attempts`、`skills` 点名与 frontmatter，并采用修复路径同一 owner 权威准备 oracle/证明 baseline。strict 缺 oracle 或 baseline 不合契约为 error；配置 oracle 时不拿 legacy reproducer 当权威。无 oracle 时仍探测 `failing_check`/`reproduce_check`，前者 base 绿给 warning，后者 base 绿为 error；超时/setup 错误不是 bug red。它会执行 owner/仓库命令，仍有宿主用户权限，不消耗 LLM、不创建交付分支。退出非 0 表示预检失败，不能证明部署隔离已完成。

## 3 · 启动 runner（应龙）

```bash
# 常驻轮询（串行 v0；每仓库一个 runner 实例，避免同仓库 git 竞争）
nuwa work --db shanhaijing.db \
  --instructions /path/to/your-repo/nuwa.repo.json \
  --runner-id <host:repo> --service-unit nuwa-work \
  --provider anthropic --pr --loop --interval 30
```

- `--runner-id`：控制台可见的 runner 身份，默认 `hostname:repo`；仅保留字母数字与 `.:_-`，超长截断。**同一 id 只允许一个活进程**（ADR-018）：第二个进程会等待旧租约过期（至多 TTL+10s，约 70s），仍被持有则启动失败；要并行跑多个 runner 请给每个进程不同的 `--runner-id`。被接管时进程以 0 退出（systemd 不再重启），由新持有者继续。
- **Batch1 checkout 权限（ADR-023）**：schema v10 另有 canonical checkout claim，使用 token/代际/有效期；runner 必须匹配实际存活的 `instance_id`，仅同名 `--runner-id` 不能借用后继实例。独立 fix 与 work 必须使用同一 Store/数据库才能互斥；不同 DB 不能协调同一 checkout。checkout/delivery claim TTL 60s，等待期间每 10s 续租；失权后停止后续副作用和写回，已发出的远端请求不能据此保证撤回。
- **多 worker（A-3 拓扑）**：一个 DB 就是队列；每个 worker 是独立的 `nuwa work --loop` 进程，必须使用**不同**的 `--runner-id` 和**不同**的仓库 checkout（各自 clone）。不同 id 共用同一 checkout 会被租约直接拒绝（错误里带占用者 id），避免两个进程互相 reset/fetch。每个 worker 按目标栈准备工具链（Go/Rust/Node/Python），这就是 runner 镜像矩阵的部署侧含义；当前发布单一二进制，镜像/unit 由部署方按栈组合。PR 准入槽位/token 用量在同一 DB 按仓库共享；阈值由各创建进程解析，配置不同不构成中心统一限额，须由所有者保持一致。
- **单进程池 `--pool N`（1–16，默认 1）**：一个 `nuwa work` 进程内起 N 个逻辑 runner；worker #1 用主 checkout，#2..N 用 detached git worktree，保留逻辑 PR base，交付后安全恢复到预先固定的 detached HEAD，不切到 base 分支。每个 worker 有独立 runner id（`<id>-2`…）、租约、心跳与 trusted 配置副本；共享 DB 队列、配额与一把 fetch/清理互斥锁。控制台显示 N 个 runner。SIGTERM 释放全部 worker 租约；pool teardown 保留脏树或附着分支的 worktree 证据，只对干净且 detached 的 worktree 调用不带 `--force` 的 `git worktree remove`。进程骤停可能留下 worktree，须由所有者人工复核，不自动清除现场。单进程内是异步并发而非 OS 进程隔离。
- **预构建镜像矩阵**：`docker/` 提供 base + go/rust/node/python 五种 runner 镜像（[docker/README.md](../docker/README.md) 有构建/运行/缓存/安全说明）。CI 的 `docker` job 每次用 musl 产物构建验证；registry 推送由所有者执行，本仓不自动发布镜像。容器内每个 worker 的 checkout 必须挂到不同路径（checkout 互斥按 `repo_path` 比较）。
- `--service-unit`：记录 systemd 单元名（默认 `nuwa-work`）。控制台的「重启 unit」动作会消费它（缺失/非法时该动作禁用或 422）
- `--daily-token-limit` / `--monthly-token-limit`：每仓库 UTC 日 / 滚动 30 天 token 观察闸（默认 5M / 50M；不是自然月，失败尝试也计入）；不是费用硬封顶，详见下方 Batch6。
- 密钥处理：work 启动时把 `NUWA_GITHUB_TOKEN`/`GITHUB_TOKEN`/`ZAI_API_KEY`/`ANTHROPIC_API_KEY` 读入内存并从 env 移除；bash/legacy 检查收到白名单（PATH/HOME/GOCACHE 等），可用 `NUWA_AGENT_ENV_ALLOW` 追加非敏感项；**trusted 验证不接受该扩展**，用上方独立最小环境/私有 HOME。两者都不是 OS 权限隔离，ssh-agent / credential store 等仍须部署侧隔离。
- 指令文件：tracked 配置以 `origin/<base>:nuwa.repo.json` 为权威；untracked 配置首次启动复制到 `<sessions>/_trusted/`。每轮 fetch/reset 后、领单前刷新权威配置，修复准备前可恢复配置；一旦冻结 VerificationContext，改动/删除配置或 protected inputs 均拒绝，不能先恢复后接受。新 main 配置在**下一轮**可采用，tracked 来源优先于旧私有副本；当前 run 移动 base 重验不更换原 oracle。
- pr_open 自动对账：serve 每 `NUWA_SYNC_INTERVAL` 秒（默认 600）对账一次，与手动 `nuwa sync` 共用锁。
- 通知（可选）：`NUWA_WEBHOOK_URL`、`NUWA_WEBHOOK_SECRET`、`NUWA_WEBHOOK_EVENTS`；事件 JSON 带 `id`（每个事件唯一）、`ts_ms` 与 `X-Nuwa-Signature: sha256=<HMAC>`（签名覆盖整个 body，含 `id`/`ts_ms`）。默认订阅 `escalated,ignored,provider_stand_down,heartbeat_stale,ci_red,sync_error,pr_opened,runner_error`。进程退出前会等待队列清空（最多 10s），`escalated` 不再随退出丢失；**接收方必须校验 `ts_ms` 新鲜度并按 `id` 去重**，否则捕获的 body 可被重放。
- 会话保留：work 启动及运行中定期清理超过 365 天无新 run 的 `sessions/<fp>` 目录；`_trusted` 永不清理。

### Batch6 · PR 准入槽位与 token 观察闸（ADR-028，schema v14）

- work/pool/独立 fix 的 PR 创建共用真实交付闸：任何 push 尝试前在 `BEGIN IMMEDIATE` 中按 repo + UTC 准入日预留槽位，请求前最终幂等检查并写 requested。权限沿用 delivery claim 的 token/generation/expiry；reserved/requested/ambiguous/consumed 占额，released 不占，branch-only 无需 PR 槽位。
- claim 释放/过期、重启、prune **不退款**；午夜后或接管重试仍用同槽位/原准入日。只有请求前安全 supersede 可原子转移预留；确认同 repo + PR URL 的交付归并。预 push PR 拒绝释放问题单、冷却/待命，不计 attempt；postpush 门禁拒绝保留分支、intent/run/槽位，不做破坏性补偿。响应丢失/取消不代表无 PR，须查精确 head/base；只有明确确认不存在才能终结释放/条件清理，不确定保留 ambiguous。不能保证 GitHub 外部 exactly-once。
- 准入日与创建/确认时间分离：API `created_at` 是可选 GitHub 创建证据，本地确认在 `confirmed_at_ms`；缺失/无效创建时间保持 unknown，旧 `pr_opened_at` 仅历史本地观察，不用恢复时间补造创建时间。
- token 模型前已知仓库用量 `>=` 日/滚动 30 天限额拒绝，最终个体/仓库用量仅 `>` 拒绝。当前 run 的 cached/uncached 分量各取持久值与报告最大值，避免重复计入且含跨窗口当前修复；失败 work/独立 fix 也单调记账。聚合按 run started_at，不是按 token 实际发生时间精确分桶；运行中/并发、provider 部分/未知 usage 仍会超支，**没有 token 预留或硬 cap，不能撤回已花 token**。独立 fix 通过同一 `--db` 留下真实 run 用量，不伪造 runner/领取已有 issue。
- 控制台/API projects/runners 的 `pr_slots_today` 表示 UTC 今日准入槽位；`prs_today` 保留创建/历史本地观察指标，不作准入依据。runner `daily_pr_limit` 为本进程上报，project `pr_limit` 为上报观测最大值，分母不是中心统一硬上限；不同进程配置会影响准入策略。
- 当前仅有 refinements 前 Store 186 / quota 3 通过；delivery 42 项及全部最终 gates 待确认，完整证据见 docs/07 §D。此处是本地实现契约，不是生产操作、付费模型或平台隔离验证。

### Batch1 · 中断恢复与保留分支（当前策略）

- **脏树拒绝**：preflight 发现脏工作树即停止本轮，保留中断修复，不再自动 `reset --hard`/`clean`。所有者先停对应写者、检查 diff/未跟踪文件并保存证据，再决定恢复；不要用破坏性重置绕过拒绝。变更判定与 staging 仍排除 `nuwa.repo.json`。
- **不再自动扫除 stale/terminal 分支**：问题单终态或 `agent/fix/*` 前缀不是删除授权；本地/远端分支保留给所有者复核。此策略不等同于禁止交付自身的安全局部清理：后者必须仍拥有 checkout，且当前分支/tip 均匹配预期；失权或不匹配时保留现场。
- **交付恢复**：pending intent 原子领取，每次调用最多处理一条；只采用 open 且 head SHA/base 精确匹配的 PR。基线合并改变 head 时，用 `superseded` 保留旧 intent 并原子领取新 head 身份。PR 完成时 delivery、关联 issue/run 与 CI watch 同事务写入；独立 fix 可以没有 issue/run。
- **仅推分支的恢复边界**：branch-only 路径在 push 前持久写入 `branch_only`，排除在 PR 自动恢复之外；崩溃后由所有者人工复核，不因没有 PR 自动删除分支。只有 push 成功且经验证、checkout 安全恢复后，才进入终态 `branch_pushed`。
- **孤儿远端分支**：仅 GitHub 查询明确证明零 PR 后，才允许按 expected SHA + `force-with-lease` 条件删除。head/base 不符、已关闭 PR、畸形响应、分页不完整或任何未知结果都保留分支；远端 tip 改变/删除结果不明同样保留。出现 ambiguous 先复核 PR 与分支，不把“查询失败”当“没有 PR”。
- **独立 fix**：`nuwa fix --db /path/to/shanhaijing.db --issue … --repo …` 使用与 work 相同的数据库；`--db` 默认 `shanhaijing.db`（相对当前目录，部署时应传同一绝对路径）。fix 使用自己的 checkout 租约，不登记假的 runner，租约覆盖修复、交付及网络等待；有未完成交付时先对账，不绕过 Store。
- **Batch1 历史限定范围验证**：PR 正文在内存中直接传递，不依赖 `.git` 为目录；回归使用真实本地 remote/linked worktree + 实际交付 GitHub 接口 fake。该批完成时 `cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；`cargo test -p shanhaijing --offline` **132 通过**；`cargo test -p nuwa --bin nuwa --offline -- --test-threads=4 --skip benchmark::tests` **388 通过、1 ignored、9 个 benchmark 测试被过滤**。不是当前 Batch3 完整套件的复测，不构成 workspace、web、完整验证或真实 GitHub/生产部署验证结论，也不保证外部副作用 exactly-once。

### Batch2 · SHA 绑定 CI 观察（schema v11 引入，契约继续有效）

- **共享观察而非两套闸门**：`work` 前台只等待交付的 `(slug, PR 号, head SHA)`；与 serve 共用同一 Store claim、持久检查计划和处理器，不越过 `next_check_at` 或其他活跃持有者。前台预算耗尽/取消不当作 CI 红灯，watch 保留；前台结束后的持续观察需要 serve 常驻。缺 token 时 watch 持久保留但不会推进。
- **租约与排程独立**：CI claim 使用 token/代际/有效期，TTL 120s、每 10s 续租；后台每轮最多 20 条、逐条领取，失权停止后续请求与写回。pending 30s、green 5min 后复查，错误指数退避至多 1h，超过 30 天过期；不能靠手改下次检查时间代替所有权或推进状态。
- **只消费交付 SHA 的事实**：PR 快照严格解析 state/merged/head；Actions 未注册/未完成、API 不可读/畸形或超时不是失败证据。head 改变则终态 `head_changed`，不自动跟随替换 head。green+open 继续观察，green+merged 可记录记忆，closed 即使 CI green 也不发布记忆；外部关闭不自动重开、合并后红灯不自动回滚。
- **红灯关 PR 的边界**：关闭前重读 open/同 SHA，并核验仍归属当前 issue/run/delivery；关闭后确认 closed/同 SHA，才允许该轮 issue 失败计数与冷却/重试。watch、精确 run 结论、适用的 issue 更新、记忆证据和审计同事务；失权/未知结果不猜测拒绝新一轮交付。
- **GitHub TOCTOU 限制**：GitHub close 没有原子 expected-head 条件，关前/关后复核无法消除与 push/merge 的检查后使用竞态，已发出的请求也不能保证撤回。只有不执行自动 close 才能消除此残余风险；本批没有新增禁用自动 close 的配置开关，不承诺绝不误关或外部 exactly-once。
- **Batch2 历史本地聚焦证据，本次文档更新未重跑**：`cargo test -p shanhaijing --offline` **148 通过**；`cargo clippy -p nuwa -p shanhaijing --all-targets --no-deps --offline -- -D warnings` 通过；Nuwa `ci` 过滤 **50 通过、371 filtered**（最终 summary 修正前，不是修正后复测）；`export_bindings` **72 通过**；`npm.cmd run typecheck` 通过；memory 页面 **6 通过**。不是完整 Nuwa/workspace 或完整 web/lint/build 通过的结论；未执行生产迁移、部署、真实 GitHub/API 或付费模型测试。

### Batch3 · 保存后如何确认启动版本（schema v12）

1. 在设置页保存候选；这只写文件及版本元数据，**不改启动 `env_store`、Secrets 或 serve 鉴权/会话**。全部 schema 项 `requires_restart=true`。非空 PUT 即使值未变也可为旧/不一致文件建立新随机 UUID revision；一致 no-op 保留原 revision，空 PUT 不建立版本。
2. 确保服务管理器把**同一文件（含 `NUWA_CONFIG_REVISION`）**加载到新进程环境，例如现有 systemd `EnvironmentFile`。Nuwa 不把“当前读到带 marker 的文件”当作它已被加载的证据；单独设置 `NUWA_ENV_FILE` 也不等于加载文件。启动环境 marker、文件 marker、DB revision 三方一致，且该角色全部适用可写 schema 输入精确相等（含缺失/存在与密钥，仅内存比较），才能证明 `loaded_revision`。缺环境 marker 的外部注入即使值一致也仍是 unknown。
3. 受控重启相关 serve/work/pool；设置/system/runner 页面逐实例查看保存版本、加载版本、实际非密钥值与来源。`active` 只证明加载了当前 managed input revision，**不保证 CLI 最终值等于文件值**；显式 `cli` 覆盖须改启动命令才会消失，同命令重启仍覆盖。provider/model/base 来自实际 resolver，Anthropic 缺省 catalog 首项、显式未知模型不冒充默认；zai 缺省 `glm-5.3-flash` / coding endpoint，faux 使用实际离线默认。
4. 分别看三轴：persistence `saved/unversioned/unknown`、activation `active/stale/unknown`、liveness `online/offline/unreported`。离线 active 只是历史证据，不代表进程仍在运行；未上报 unit、旧 runner 无快照均不能猜测已生效。一个 OS 进程一个 UUID，pool worker 共享，但 runner 租约实例独立；快照按有效实例首次写入、不可变，心跳不更新它。不能选最新 runner 为所有进程代言，也不再用兼容字段 `restart_pending/effective_diff` 判断生效。

**所有者手工改文件时必须先移除/使 `NUWA_CONFIG_REVISION` 失效**，再通过非空 PUT 重新建立版本。UUID 既不是秘密也不是内容/密钥 hash；保留 marker 的手工编辑不能被此机制认证或可靠检测，这是所有者控制的来源契约，不是密码学防篡改。不要手工伪造 marker/DB 行来“变绿”。`migrate-env` 真正重写时会移除 marker，且不参与保存事务；只能停止所有写者后离线维护，**不得与设置保存并发**。

若保存返回 500 `config_save_indeterminate`，文件可能已经替换但 DB 提交结果不明；**先重新 GET/刷新并复核，不当作成功，也不盲目回滚/重试**。GET 的 values 与 saved revision 共用事务读快照，file/DB 不一致显示 unknown。文件和 `.bak` 经同目录受保护 tempfile + sync + persist，Windows 也原子替换；Unix 写入前设 0600，Windows ACL 仍由所有者负责。该文件原子替换不代表文件与 DB 原子提交。

候选探测明确选择 saved（文件+默认）或 draft（同次文件读取+草稿），从不借用 live env/key；更改 zai base URL 必须显式提供新草稿 key。探测不保存、不证明激活。已有本地结果及最后一致读取 helper 之后尚待父任务记录的复测见 docs/07 §D；本次没有执行测试或 systemd/平台/生产验证。

## 5 · 第一个真实系统接入（mindloop，进行中）

| 项 | 值 |
|---|---|
| 仓库 | `github.com/lomehong/mindloop`（私有） |
| 采集点 | `cmd/robotd`（桌面 MCP 守护进程，无人值守形态）——`qingniao.Capture` + HTTPSink |
| 指令文件 | 仓库根 `nuwa.repo.json`（failing_check 排除 web/static npm 噪音） |
| 待办 | 所有者：推送 mindloop 至 GitHub + token 仓库列表加 mindloop；runner 服务器 clone + work 指向 |

## 4 · 日常运维命令

**Web 控制台（v3 SPA，0.2.0 起是唯一控制台）**：`http://<server>:7321/`，在 `/login` 用 `NUWA_UI_TOKEN` 换取 HttpOnly 会话（**不再使用 Basic 头**）。`/` 就是 SPA 控制台；`/ui*` 308 永久重定向到 `/`，`/issue/{fp}` 重定向到 `/issues/{fp}`，`/settings` 直接由 SPA 提供；`/ingest` 与 `/health` 的路径和鉴权不变。SPA 提供总览、问题单列表/详情（含会话转录）、运行器、活动与设置。

> **控制台令牌与上报令牌完全分离**：`NUWA_UI_TOKEN` 缺失时控制台进入本地模式，且**非回环监听会拒绝启动**。`NUWA_INGEST_TOKEN` 部署在每个被采集的服务上（泄露后果只是垃圾信号），绝不能当控制台凭据用——控制台能读设置页里的 LLM key。升级后只设了上报令牌的公网部署会启动失败，需补 `NUWA_UI_TOKEN`。`/api/v1/auth/logout` 会服务端撤销会话（复制走的 cookie 立即失效）；撤销表在内存中，但每次 serve 重启都会更换会话签名代际，因此所有旧 Cookie 都会失效并要求重新登录。

- **问题单动作**：escalated/rejected/ignored 可 `重新打开`（回到 `new`、清冷却并重置 attempts）；new/escalated/rejected 可 `忽略`（填写原因；ignored 后同指纹报告只计数，不再自动修复）；卡单（`triggered`/`fixing` 且超过卡单窗口）可 `重排`。动作出现在详情头部、列表与总览待处理队列；成功 toast、失败逐条显示服务端错误。CLI 对应 `nuwa reopen --reason` / `nuwa ignore --reason`，理由会写入事件并在下一轮 prompt 中作为 OWNER NOTE。
- **对账与重启**：`GitHub 对账` 汇总 checked/merged/rejected/still_open，并逐条展示 `errors[]`；runner `重启 unit` 先确认影响（显示当前修复指纹，提示同一 unit 承载的多个 runner 会一起重启）。409 表示已有重启在排程，422 指出 `service_unit` 缺失/非法，501 直接给出手动 `systemctl restart <unit>` 命令。
- **设置**：按 29 键 schema 分组编辑；密钥只显示「已设置 · 末 4 位」，留空保持；只提交改动键，写 `NUWA_ENV_FILE` 并备份，不改 live 配置。需重启相关进程并逐实例检查启动证据，active 不代表 CLI 值与文件一致。连接测试明确是 saved/draft 候选、不借 live key、不证明激活。`/system` 的 `重启服务` 遇活动 run 返回 409 `active_runs`，勾选影响确认才允许 force；202 后轮询 `/health`、`/auth/me`，systemd 不可用则手动操作。保存 UI token 不改变当前鉴权，重启 serve 后所有旧会话失效；未保存草稿须先处理。恢复可达后回 SPA 根路径，不把可达当作所有 work 已激活。

**API 登录（v3 控制台）**：`POST /api/v1/auth/login`，必须带 `Content-Type: application/json`，body `{"token":"<NUWA_UI_TOKEN>"}`；成功 204 并设置 `nuwa_session`（HttpOnly）与 `nuwa_csrf`。后续非 GET 请求必须带 `X-NUWA-CSRF` 头，且其值须与 `nuwa_csrf` cookie 值一致（双提交校验，仅"带头"不够）。会话与 CSRF cookie 的 `Max-Age` 由 `NUWA_SESSION_TTL_SECS` 控制（默认 86400，允许 300–2592000）；`NUWA_COOKIE_SECURE=always` 强制加 `Secure`（TLS 代理下也可由 `X-Forwarded-Proto: https` 自动触发），`=never` 显式关闭且优先级最高（即使同时配置 `always` 或代理上报 https 也不加 `Secure`）。

**前端本地开发（v3 SPA）**：源码在 `web/`（Node ≥24.15）；`cd web && npm ci` 后可用 `npm run dev`（Vite 5173，把 `/api` 与 `/ingest` 代理到 `http://127.0.0.1:7321`，`changeOrigin: false` 保留 Host/Origin，否则写请求会被同源校验拒绝）、`npm run typecheck`、`npm test`、`npm run build`（typecheck + Vite）。生产构建 `web/dist` 由 `nuwa serve` 嵌入并在站点根路径 `/` 提供；`/login` 用 `NUWA_UI_TOKEN` 换取 HttpOnly 会话，未配置令牌时进入本地模式。旧控制台路由已移除：`/ui*` → `/`、`/issue/{fp}` → `/issues/{fp}`，`/settings` 由 SPA 直接提供。

**控制台 E2E（本地 UAT）**：`web/e2e/seed.py` 用 stdlib 造 7 状态问题单、3 runner、4 run、14 天事件与一份真实转录（`--issues N` 可批量造 M6 压测数据）；`web/e2e/smoke.mjs` 用 `playwright-core` + 本机 Chrome（不下载浏览器）跑登录、rejected 重新打开、runner 重启确认（只取消）、审计预设、设置保存，并在 390/1024/1440 截图断言无横向溢出/内联样式/console error。示例（临时 DB/env/sessions 建议放 `.superpowers/tmp-m5-e2e/`，均 gitignored）：

```bash
python web/e2e/seed.py --db .superpowers/tmp-m5-e2e/e2e.db \
  --bin "$HOME/.cargo/shared-target/debug/nuwa.exe" \
  --sessions .superpowers/tmp-m5-e2e/sessions
NUWA_UI_TOKEN=dummy NUWA_ENV_FILE=$PWD/.superpowers/tmp-m5-e2e/nuwa.env \
  nuwa serve --db .superpowers/tmp-m5-e2e/e2e.db --listen 127.0.0.1:7404
cd web && NUWA_E2E_BASE_URL=http://127.0.0.1:7404 NUWA_E2E_TOKEN=dummy npm run test:e2e
```

`NUWA_E2E_CHROME` 可指定 Chrome/Chromium 可执行文件，`NUWA_E2E_ARTIFACTS` 可改截图目录（默认 `web/e2e/artifacts/`，gitignored）。

| 命令 | 用途 |
|---|---|
| `nuwa serve --db p [--listen 127.0.0.1:7321]` | HTTP ingest 服务端：`POST /ingest`（单信封或 JSONL，`application/json`），`GET /health`。青鸟 `HTTPSink` 直连 |
| `nuwa ingest --signal s.jsonl [--db p] [--trust-observations <source-id>]` | 投喂单对象或 JSONL；可选授信仅限所有者已核验时钟/事件/环境与精确版本关系，不从 HTTP 鉴权或 JSON 推断 |
| `nuwa status [--db p]` | 全部问题单：状态/严重度/频次/PR 链接 |
| `nuwa work …` | 领单 → 修复 → 验证 → 交付（`--no-deliver` 只修不交；`--runner-id`/`--service-unit` 见第 3 节） |
| `nuwa reopen --db p --fingerprint FP` | 人工复核升级单后重新入队（清冷却） |
| `nuwa fix [--db p] --issue … --repo …` | 手工指定问题单跑一次；默认 DB 为 `shanhaijing.db`，须与 work 共库并取得自己的 checkout 租约 |
| `nuwa migrate-env [--env-file p]` | 停写离线维护：小写键迁大写，写前备份 `.bak`、受保护临时文件原子替换；实际改写移除 revision marker，不得并发保存 |

### Batch1–Batch6 · schema v9–v14 停写升级（所有者授权执行，尚未执行）

这是**停写升级**，不是滚动升级。当前目标是 schema v14：v9 库顺序迁移 v10 → v11 → v12 → v13 → v14，v10–v13 库执行剩余迁移，已有 v14 重开也须核对 pending；此前目标为各批历史记录，Batch4 不新增 schema。v12 → v13 不编造旧 observations；v13 → v14 的表/索引、历史槽位归并与版本同事务，失败保留 v13。历史 PR 按 repo + PR URL 去重，无 URL 保留独立证据；旧 GitHub 创建时间保持 unknown，未确认 requested/ambiguous 保守占位。main 基线 v0.17.0 未升版/发版；Batch5 已有聚焦结果不变，Batch6 refinements 后所有最终 gates 待完成。以下须所有者明确授权后执行，不是迁移或部署成功记录：

1. 盘点同一生产 DB 与 checkout 的全部旧写者：serve（含 CI watch/对账）、所有 work/pool worker、独立 fix、会写 Store 的 CLI/定时脚本。停止这些进程，并暂停 systemd/容器重启、定时任务等自动拉起入口；停止一个 work unit 不代表全部停写。确认没有在途修复、网络交付或其他写者后再继续。
2. **先备份、后让新版打开 Store**：一致备份数据库、`nuwa.env`/已有 `.bak`、部署配置及必要会话/checkout 证据。SQLite WAL 可能有尚未 checkpoint 的已提交事务；不能仅拷贝 `.db` 或手工删除 `-wal`/`-shm`。所有连接关闭且无写入后，可保留 `.db` 及当时存在的 `-wal`/`-shm` 一致文件集；也可使用 SQLite 一致性备份，并检查 checkpoint/完整性结果。env marker 与 DB revision 也是配套恢复材料；备份含敏感值，限制权限，不输出凭证。需要 `migrate-env` 时在此停写窗口操作、保留其改写前备份，不得与保存并发。
3. 记录源库 `user_version`，保留对应旧二进制与一致升级前备份，在隔离副本上先验证迁移与恢复。经所有者确认后，仅由新版首次打开生产 Store 升至 v14，核对 `user_version=14` 与完整性，再受控恢复新版服务；**不得重新启动旧写者混跑**。旧 intent/watch 无 claim 只在全部旧写者停止前提下安全，不证明无人执行。新版拒绝更高 schema，不能据此假设历史二进制有同样保护。
4. 核对 v11 CI claims/唯一身份/`repair_memory_evidence`、v12 `config_saved`/`runners.startup_config_json`、v13 `signal_observations` 与 v14 `pr_slots`/`pr_slot_deliveries` 表/索引及 repo/PR URL 归并。旧记忆为 `legacy_unknown`、不可注入；不编造旧配置 revision、runner 快照、observations 或 PR 创建时间，按上方 Batch3 受控保存/加载 marker/重启后确认每实例。逐一检查 pending delivery/槽位（尤其 requested/ambiguous）、原准入日、PR head/base、checkout、CI watch 与遗留分支；冲突停在副本复核，不删记录绕过、不自动 reset/clean/删分支。**不得按 TTL、午夜或人工猜测释放槽位**；远端未确定不存在就保留恢复证据。不能把本地测试通过当作此步完成。
5. 如需回滚，先停全部新版写者及自动重启入口；保全升级后的 DB/WAL/env 与现场，再恢复一致的**升级前源 schema 备份、配套 env 及对应旧二进制**，不能让旧版直接写 v14 库或仅改 `user_version` 假降级。回滚不会撤销已经发生的 GitHub PR/push/close，恢复写入前须人工对账备份时点之后的远端副作用。

**生产操作未执行**：上述停写、备份、副本验证、迁移、恢复/回滚均是规程，不是本地文档更新已完成的动作。

**v0.1.7 历史升级注意（env 键迁移；当前须遵守 Batch3 停写规程）**：升级前停全部写者，备份 `nuwa.env`，运行 `nuwa migrate-env --env-file /opt/nuwa/nuwa.env`，确认键为大写后启动。serve 启动兼容迁移会在真正改写时移除 revision marker，但 systemd 已把旧环境注入当前进程，需重新受控保存/加载并重启 serve/work；不要并发启动另一个会保存的进程。访问登录页只验证鉴权入口，不证明配置 revision 已激活。

**同源校验与代理**：`/api/v1` 写请求在 CSRF 之外还比较 `Origin` 的 host 与请求 `Host`（无 `Origin` 时回退 `Referer`）。若前置反向代理改写 Host（如 `proxy_set_header Host $proxy_host`）或使用 HTTP/2 上游，合法 POST 会被 403；代理需保留 Host 并使用 HTTP/1.1 上游。当前直连 7321 的部署不受影响。

## 5 · 只读 API（v3 控制台）

`/api/v1` 的六个只读端点（均为 GET，响应 JSON，带 `Cache-Control: no-store` 与严格 CSP）：

| 端点 | 用途 |
|---|---|
| `GET /api/v1/overview?repo=` | 总览：状态漏斗、需要处理、活跃 run、可领队列、runner 健康、24h CI |
| `GET /api/v1/issues?state=&severity=&repo=&language=&q=&stuck=1&attention=1&sort=&order=&page=&page_size=` | 问题单列表（过滤/排序/分页） |
| `GET /api/v1/issues/{fp}` | 问题单详情：堆栈样本、时间线、最近 10 次 run、同名相似单；新增最多 100 条信号回执与固定 unknown 的部署/恢复状态 |
| `GET /api/v1/runners` | runner 健康与 effective 配置 |
| `GET /api/v1/events?kind=&repo=&fingerprint=&from_ms=&to_ms=&aggregate=1&page=&page_size=` | 活动流（`aggregate=1` 按 fingerprint+UTC 小时聚合 `reported`） |
| `GET /api/v1/metrics/trends?range=7d\|14d\|30d\|90d&repo=` | UTC 日桶趋势与区间合计 |

- `GET /api/v1/projects` 与 `/runners` 均新增 `pr_slots_today`；准入/历史观察及非中心统一的上报限额语义见第 3 节 Batch6，不能把 `prs_today` 或分母显示当作当前准入承诺。
- token 模式下 GET 也要求有效 `nuwa_session` cookie（先 `POST /api/v1/auth/login`）；本地模式（未设 `NUWA_UI_TOKEN`）免会话，`NUWA_INGEST_TOKEN` 是否配置不影响控制台鉴权。
- 非 GET 请求另需 `X-NUWA-CSRF` 头，其值须与 `nuwa_csrf` cookie 一致（双提交校验）；GET 不要求 CSRF 头。**本地模式同样强制 CSRF**（防止浏览器跨站驱动本机控制台），前端必须先 `GET /api/v1/auth/me` 拿到可读的 `nuwa_csrf` cookie。

## 6 · 动作、设置、系统与转录 API（v3 控制台 M2c）

所有写操作都在 `/api/v1` 的会话 + CSRF 中间件之后：非 GET 必须同时带 `nuwa_csrf` cookie 与同值的 `X-NUWA-CSRF` 头（缺失/不一致 → 403 `forbidden`）；token 模式还需先登录。响应统一 `Cache-Control: no-store` 与严格 CSP。以下成功/失败码与 docs/09 §9.2 一致。

### 6.1 问题单动作

| 端点 | body | 成功 | 失败 |
|---|---|---|---|
| `POST /api/v1/issues/{fp}/reopen` | `{"reason":"…"}`（可选） | 204，状态回到 `new` 并清 owner | 404 未知单；409 非 `escalated`/`rejected`/`ignored`；422 body 类型错误 |
| `POST /api/v1/issues/{fp}/requeue` | `{"force":true,"reason":"…"}`（可选） | 204，卡单回到 `new` 并清 owner | 404；409 非 `triggered`/`fixing` 或未卡单；422 |

- `reopen` 的 `reason` 写入 owner 审计事件并进入下一轮 OWNER NOTE；`requeue` 的 `reason` 当前仅接受、不落库。事件分别为 `state_changed` 与 `requeued`，actor 均为 `owner`。
- 卡单窗口 = `max(3 × owner runner interval_secs, NUWA_STUCK_AFTER_SECS)`；没有 owner 时按默认 30s interval、300s floor。`force=true` 跳过卡单判定，仍要求状态为 `triggered`/`fixing`。

### 6.2 对账与重启

`POST /api/v1/sync`：serve 进程读取 `NUWA_GITHUB_TOKEN`（回退 `GITHUB_TOKEN`）逐个查询 `pr_open` 单的 PR 实态并写回 `merged`/`rejected`。成功 200：

```json
{ "checked": 7, "merged": 1, "rejected": 0, "still_open": 5,
  "errors": [{ "fingerprint": "…", "message": "pulls/12 502: Bad Gateway" }] }
```

- **单个 PR 的 GitHub 查询失败不影响整次对账**：该单计入 `errors[]`（结果行 `error : <fp> — <message>`），其他单继续处理，HTTP 仍是 200。前端必须渲染 `errors[]`，不能只看计数。
- 缺 token → 502 `upstream_error`；已有 sync 在跑 → 409 `conflict`；30s 超时 → 502。`reconcile_all` 的本地存储失败是 500 `internal`（不是 GitHub 上游故障）。
- 通过设置页写入 `NUWA_GITHUB_TOKEN` 后，**serve 进程要重启才读得到**（env 在进程启动时注入）；未重启前 sync 仍按旧值/缺 token 处理。

`POST /api/v1/runners/{id}/restart`：重启该 runner 存储的 `service_unit`（`nuwa work --service-unit` 上报）。成功 202（`systemd-run --on-active=2 systemctl restart <unit>` 已编排，2 秒后执行）。runner 不存在 404；`service_unit` 缺失/空白或不在白名单（字母数字与 `@ . _ : -`，且不得以 `-` 开头）→ 422 `validation_failed`（`fields.service_unit`）；`systemd-run` 不可用或 3 秒内未返回 → 501 `not_supported` 并给出 `systemctl restart <unit>` 手动命令。`/api/v1/settings/restart` 与 `/runners/{id}/restart` 共享进程内重启互斥：已有重启在编排时 → 409 `conflict`（不写审计、不再起子进程）。同一 unit 承载多个 runner 时会一起重启，部署建议一 runner 一 unit。

### 6.3 设置

- **保存输入源**：`NUWA_ENV_FILE`（默认 `/opt/nuwa/nuwa.env`，通常由 systemd `EnvironmentFile` 加载）。全部 schema 项需重启；保存不改启动 `env_store`、Secrets 或 live auth。运行事实仅由各进程不可变启动证据报告，不能把文件当作活值。配置值不得含单/双引号、反斜杠或控制字符。手工编辑必须移除/失效 `NUWA_CONFIG_REVISION`；保留 marker 的编辑不能认证，这是所有者来源约定，不是密码学证明。
- `GET /api/v1/settings`：29 键 schema、掩码值、文件元信息、`saved{env_id,revision,state,reason}` 与 `processes[]`（snapshot/activation/freshness/reason）、`unknown_keys` 名称、`last_changes`。文件 values 与 saved revision 共用事务读快照。snapshot 含进程 UUID、runner 实例、启动时间、loaded_revision、非密钥实际值/来源；三轴规则见 Batch3。密钥只返回 `set/last4/updated_at_ms`，四位以内无 last4。兼容 `restart_pending/effective_diff` additive 保留但弃用，**不能作为 UI 激活权威**。
- `PUT /api/v1/settings`：`{"values":{"NUWA_INTERVAL":"60","NUWA_GITHUB_TOKEN":""}}`。空密钥保持；未知/只读键 422，任一字段非法则整单不写。成功保留注释/顺序/未知键并备份，跨 Store 保存由 SQLite 事务串行；新版本写随机 UUID marker/DB revision 与逐变更（含迁移）键名 `config_changed` 审计。非空无值变化也可采用未版本化/不一致文件；一致 no-op 保留版本，空 PUT 不建立版本。跨文件/DB 不原子，替换后失败为 500 `config_save_indeterminate`，重新 GET 复核；不一致 unknown。文件/备份均用受保护同目录 tempfile + sync + persist 原子替换，Unix 写前 0600，Windows ACL 所有者负责。
- `POST /api/v1/settings/test`：`{"target":"github|llm","candidate":"saved|draft","values":{…}}`；saved 不允许 draft values，draft 为文件+草稿，旧请求默认 draft；**从不回退 live env/key**。未知键或非标量 422；只接受字符串/数字/布尔，对象/数组/null 422。换 zai base URL 必须显式提供草稿 key。github 调 rate_limit，llm 用 `max_tokens=1`（可能有极小费用）；空 model/base/provider 用 provider 默认。Anthropic 探测用固定 `https://api.anthropic.com` 与 catalog（未知名的探测仍回退首项，**不同于实际运行拒绝显式未知模型**）；zai 默认 coding endpoint / `glm-5.3-flash`，faux 离线。显式空密钥 422；网络失败/非 2xx/10s 超时 502，不回显响应体。成功只说明候选连接可用，不证明保存或激活。
- `POST /api/v1/settings/restart`：`{"force":false}`。有未结束 run（`runs.finished_at IS NULL`）→ 409 `conflict`，body 含 `active_runs`；`force=true` 才继续。重启目标 = `NUWA_SERVE_UNIT`（默认 `nuwa-serve`）+ `NUWA_RUNNER_UNITS`（默认 `nuwa-work`，逗号分隔），逐个白名单校验；先写 `restart_requested` 审计（`__system__`），再 `systemd-run --on-active=2`，成功 202。无 unit 配置或 `systemd-run` 不可用 → 501 并给出手动命令。每次重启 serve 都会使旧会话失效，即使 `NUWA_UI_TOKEN` 未改变也需重新登录，避免已登出的 Cookie 在重启后恢复有效。

### 6.4 系统与转录

- 转录只展示 runner 完成脱敏后发布的快照，不读取原始 JSONL。运行中、被中断、发布失败或历史未发布的 run 显示空态，不回退展示原始内容；成功和失败结束都会尝试发布。发布故障单独告警，不否决已经成功的修复，也不额外消耗 attempts。原始日志仍属于 runner 私有取证材料，不要直接通过静态服务公开。
- `GET /api/v1/system`：`version/build_commit/started_at_ms/uptime_secs/db_path/db_size_bytes/local_mode/services[{unit,active}]`，additive `configuration` 只报告**此 serve 实例**的启动快照/activation/freshness，不代表所有 work 已激活；runner 的 additive `configuration` 在 `/runners` 每行分别报告，设置的 `processes[]` 汇总相关实例与未上报 unit。systemd 不可用或无输出时 unit `active="n/a"`，本地模式显示无鉴权警告；unit 状态不替代配置证明。
- `GET /api/v1/issues/{fp}/transcript?run_id=&page=1&page_size=20`：省略 `run_id` 时取该指纹**最近一次有 `session_dir` 的 run**；指定不属于该指纹的 run → 404 `run not found`；未知单 → 404 `issue not found`。只解析 `kind=entry,type=message`，映射 user / assistant（thinking、text、tool_call）/ tool_result；其它记录（header/record/model_change/custom）忽略。单个块（text/thinking/tool_result/序列化后的 tool_call 参数）按 UTF-8 字节上限 16KB 截断并标 `truncated=true`；**超限 tool_call 的 `arguments` 会从 JSON 对象变成字符串预览 `<truncated: N bytes>`**（前端需同时处理 object 与 string 两种类型），避免单个调用占满页面。文件只读末尾 2MB（更早内容丢弃并标 `meta.truncated=true`），按消息分页：`page_size` 默认 **20**、最大 **100**（100 是上限不是默认值），越界或非数字 422；无 run/无会话返回 `data:[]`、`meta.empty=true`。转录路径由 DB 的 run 行派生（必须是绝对路径且不含 `..`），从不接受客户端路径；`work --sessions` 建议传绝对路径，否则失败/中断的 run 可能没有可读转录。运行中每 3s 刷新一版脱敏快照（首帧立即），控制台可看到进行中的已脱敏转录；serve 永不读取原始 JSONL。

## 修复基准（可选）

固定种子场景验证“修复流水线 + 验证门 + 交付门”是否退化：

```bash
nuwa benchmark list                       # 内置场景目录
nuwa benchmark run --provider faux        # 确定性流水线基准（不需要 API key）
nuwa benchmark run --provider zai --model glm-5.3-flash   # 真实模型质量基准
nuwa benchmark run --provider faux --scenario node-empty-average,go-off-by-one --json
```

- `faux` 只证明流水线与门禁，不证明模型修复质量；`live` 才衡量模型，比较时固定 provider/model/场景集。
- 场景可声明 `base_green: true` + 平台复现脚本：configured check 在 base 已绿、bug 只在运行时触发；基准用与交付门相同的 reproducer 计分（内置 `go-main-panic` / `rust-main-panic` 即此路径），不再拿已绿的 failing_check 冒充 repaired。
- 内置 14 个场景覆盖：编译/类型错误、`go vet`、语法/缺失导入、运行时复现门、多文件修复、两种负对照（node/python）。CI 的 `benchmark (faux scenarios)` 跑全场景并断言 expect_fixed 必须 repaired+delivered、负对照必须两者皆否。
- 最终报告的 `OUTCOME: fixed|not_fixed|unverified` 是明确声明；仅唯一合法的 `OUTCOME: fixed` 计入 `claimed_fixed`。`VERIFICATION:` 的存在不代表成功，实际 `repaired` 由独立检查决定。
- 普通 `benchmark run` 写 `benchmark_runs` / `benchmark_results`；控制台 `/benchmarks` 看趋势、误报与逐场景证据。下面独立 Batch4 sidecar 不走该存储路径。
- 负对照（`expect_fixed=false`）必须保持“未修复”，否则说明门禁退化。
- 基准在独立 CLI 进程运行；serve 不启动模型循环。

### Batch4 faux 验收（独立 sidecar）

仅使用**尚不存在**的显式临时文件，例如 `nuwa benchmark batch4 --output /path/to/new-temp/batch4-results.json`；父目录须已存在，Windows 可用新建临时目录下的绝对路径。已有文件直接拒绝，不覆盖、不打开默认业务 DB；不接受 provider/model/db 参数，不使用 live 模型或真实 GitHub。需 Git 与 Python（Windows `python`，其他平台 `python3`）；继承的 Git 重定向/配置覆盖及 `NUWA_AGENT_ENV_ALLOW` 会被拒绝，应由所有者在干净环境运行。

- 密封 `batch4-manifest.json` 固定 fixture/oracle 的计算 SHA-256、base recipe、faux/model=faux、turn cap 12；**15 场景 × 3 trials = 45 条**。与普通 14 场景 catalog 分开，不是模型能力基线，也不代表 live benchmark 已执行。
- 场景包括 genuine fix、删除/弱化断言/替换 helper/模式篡改、吞异常、仅改 reproducer、branch/SHA 投机、noop、always-pass、缺 oracle/input/command、setup failure 与 timeout；断言实际 payload 效果，不拿 faux 声明冒充执行。交付只在临时本地 fixture，push/PR 关闭。
- sidecar 顶层含 `schema_version`、`fixture_version`、`purpose`、`provider/model/turn_cap/trials`、`manifest_sha256`、`fixture_assets_sha256`、`owner_oracle_sha256`、`environment` 与 `results[]`。每条含场景/预期/`outcome/expectation_met`、fixture/base/tree 与 owner/manifest 摘要、`trust_level`、`scripted_claim_fixed/claimed_fixed`、`faux_executed/payload_applied/turns`、`base_red/candidate_green/integrity`、交付结果/零副作用、`refusal/infrastructure_error/stage`；只存安全元数据/摘要/声明与结果，不存源码、原始命令输出、会话、凭证或环境值。声明不是可信结果；基础设施故障不能算预期拒绝。
- 契约不符时先写 sidecar 再返回失败；首次 CLI 在 trial 前 main 栈溢出，装箱后 sidecar **45/45 预期满足**，早于最后私有 seed cleanup 修正（另有真路径聚焦回归）。完整 Nuwa **483 通过、3 失败、2 ignored** 及后续聚焦结果见 docs/07 §D，不能据 faux 或单独重跑称完整套件全绿；本次不运行验收/测试/构建。

## 控制台里的技能与记忆

- `/projects/<repo>/skills`：内置技能 + 目标仓 `.nuwa/skills/*.md` 的只读目录，展示来源、版本、覆盖关系与解析告警；控制台不编辑目标仓文件。
- `/projects/<repo>/memory`：带验证来源的记忆台账，也展示未验证的历史记录和已撤销证据；分别显示手动停用、证据有效性、来源、commit/PR、撤销时间/原因，并支持生效/停用/证据无效筛选。记忆只是提示参考，不是权威。
- `GET /api/v1/projects/{repo}/memory?limit=&offset=` 默认 20、最大 100，响应每条包含 `disabled`、`evidence_valid`、`verification_source`、`revoked_at_ms`、`revocation_reason`。有效来源为 `ci_green` 或 `local_verification`；迁移的 `legacy_unknown` 可见但不可注入。
- `PATCH .../memory/{fp}` 只设置 `disabled`：停用保留来源、下一轮不再注入；启用只清除人工偏好，**不恢复已撤销或历史未验证证据**。自动写入同样不清除人工停用。`DELETE .../memory/{fp}` 则同事务删除摘要与全部关联证据（含 tombstone）；要保留撤销历史请用停用而不是删除。写操作要求 CSRF 并记录 owner 审计。
- PR 记忆由同 SHA 的 green+open/merged 观察发布，branch-only 的本地验证来源绑定 run；模型自述不会成为验证证据。每条证据按 `(repo, fingerprint, source_kind, source_id, commit_sha)` 独立保存并校验 PR URL，`ci_green` 的 source id 为 watch id。red/closed/head_changed 只撤销精确 watch/SHA/PR 来源并留 tombstone（即使未曾绿），不抹除其他提交/独立来源；迟到 green 不会复活该来源、刷新摘要或挤掉其他摘要。
- Batch5 的可信运行期回归观察按 repo/fingerprint/精确完整 release SHA 撤销匹配证据，即使摘要已经手动 disabled；采集时间须有效且严格晚于证据 recorded_at，环境/版本关系须由所有者确认。此动作不清除 disabled、不影响独立提交，启用不恢复证据；授信与 delayed/duplicate/muted 排除规则见第 1 节 Batch5。
- 每仓保留最近 50 条未停用摘要（停用摘要另保留），普通裁剪不删证据/tombstone。注入最多最近 3 条同仓其他问题单记忆，只选择有效且未撤销、未手动停用的验证来源；当前问题单仍用 PRIOR ATTEMPTS。已有 run 的提示不会因现在停用而被追溯修改。

## 问题单状态机（运维视角）

以下为原回归流程；Batch5 的可信 delayed 与保留历史内 duplicate 不重开，但仍累计接收次数，判定边界见第 1 节。PR merged 不证明部署或恢复。

```
new → triggered → fixing → pr_open → merged | rejected
                       └→ escalated ←──(交付/修复失败自动)
merged/rejected 再报同指纹 → 自动重开(new，回归)
escalated 只能人工 reopen → new
```

## 护栏复核（ADR-003 五件套在代码里的位置）

1. 分支隔离 `agent/fix/*`：`deliver.rs`
2. 频控/预算：`pr_slots.rs` 原子 UTC PR 准入，`quota.rs` / `main.rs` / `deliver.rs` 模型前与最终 token 闸，及 turn 上限；token 闸不是费用硬 cap
3. CI 闸门：PR 模板要求；合并人工
4. 路径护栏：`guard.rs` hook 早拒绝 + `guard/local_env.rs` 实际 I/O 授权（含递归读与目录变更）；`verify.rs` 完整最终 diff 终检；不是 OS 沙箱
5. 验证证据：`verify.rs` / `verify/trusted.rs` 冻结 owner oracle 或明确 legacy_untrusted，`deliver.rs` 绑定 base/candidate tree 重验；证据落每次 run 的 `verification.txt`，API 沿用 `verification_detail`

## 已知边界（含 Batch1–Batch6，2026-10-27）

- 并发拓扑与镜像定义见第 3 节；单进程池是异步并发而非 OS 进程隔离，registry 发布与生产运维验证不由本地 Batch1 测试证明
- 持续 CI 由 serve 后台调度（`ci_watches`，schema v11 引入，当前 schema v14）；`work` 的有界前台等待共用租约/排程而非独立轮询，结束后若无 serve 常驻则不会继续推进。PR 已合并后 CI 转红只告警，不自动回滚；GitHub close 残余 TOCTOU 限制见 Batch2 小节
- issue-level 修复/验证失败自动重试到 `max_attempts` 后 ignored；provider/环境故障不计数；人工关闭 PR 不自动重试
- 历史玩具仓 spike 闭环不等于真实系统或本批部署成功；mindloop 结论以 docs/07 §D 的 2026-10-20 勘误为准，不能当作 Batch1 验证证据
- 验证探针协议（A-4）按设计推迟到第一个环境特定 bug 出现
- ingest HTTP 服务端已建（`nuwa serve` + `POST /ingest`）
- env 白名单与 claim **不是 OS 沙箱**；绕过 Store 的进程不受 claim 协调，ssh-agent / credential store 仍须部署侧用独立 OS 用户/容器等措施隔离
- 通知是窄范围状态 webhook，不是通用告警路由/指标采集系统
- Batch1/schema v10、Batch2/schema v11 已完成，Batch3/4 已本地实现，Batch5/schema v13 已完成手动 disabled 撤销修正后的本地聚焦验证，Batch6/schema v14 已本地实现并完成本地最终检查（2026-10-28）；证据见 docs/07 §D。Batch5 未重跑完整 workspace/web 套件，不覆盖 Batch4 历史失败，不宣称完整 Nuwa 全绿。生产迁移/部署、真实 GitHub、systemd、付费模型、平台隔离和本批 Unix 测试未执行。
