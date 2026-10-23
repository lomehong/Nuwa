# 08 · 接入手册

状态：v0.2 hardening（2026-10-20；随 A–F 可靠性加固更新）

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
- 已知噪音：ubuntu 构建的产物在旧 glibc 上有 weak-version 警告（不致命）——待办：musl 静态构建
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

// 信号 3：显式错误——异步队列（medium）
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
- `reproduce_check` 可选：交付门要求它在 **base 上失败、分支上通过**。没有配置时，先跑 `failing_check`：base 红则直接作为复现；base 绿则要求分支提交自包含的 `.nuwa/repro.sh`（Windows 为 `.nuwa/repro.cmd`），否则 `escalated` 不开 PR
- **验证模式在修复开始前探测**：runner 先确认 base 上 `failing_check` 是红还是绿，prompt 随之切换。base 绿时 prompt 会明确要求写 `.nuwa/repro.sh`（旧行为无条件告诉模型"check 当前失败"，base 绿的单必然在交付门硬失败）。配了 `reproduce_check` 的仓库跳过探测
- `forbidden_paths` 在工具调用前做子串否决（拦截诚实错误），并在交付面对 `git diff --name-status` 做 glob 终检（权威；`*.pem` 现在真的能匹配嵌套文件）
- `max_attempts`：problem-level 失败累计到阈值后自动 `ignored`，避免自动重试循环
- `draft_first_n`：该仓前 N 个 PR 以 draft 打开；0 保持 ADR-003 的非 draft 默认
- `skills` 可选：限定注入哪些修复技能（名字见下），留空则按问题语言与失败命令自动选择，最多 3 个

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

预检会校验 JSON、repo 身份、glob、`max_attempts`、`skills` 点名与 `.nuwa/skills/*.md` 的 frontmatter，并在干净 base worktree 上探测 `failing_check`/`reproduce_check`。`failing_check` 在 base 上已是绿色会给出 warning（需要复现脚本），`reproduce_check` 在 base 上绿色则是 error。退出码非 0 表示配置不可用。

## 3 · 启动 runner（应龙）

```bash
# 常驻轮询（串行 v0；每仓库一个 runner 实例，避免同仓库 git 竞争）
nuwa work --db shanhaijing.db \
  --instructions /path/to/your-repo/nuwa.repo.json \
  --runner-id <host:repo> --service-unit nuwa-work \
  --provider anthropic --pr --loop --interval 30
```

- `--runner-id`：控制台可见的 runner 身份，默认 `hostname:repo`；仅保留字母数字与 `.:_-`，超长截断。**同一 id 只允许一个活进程**（ADR-018）：第二个进程会等待旧租约过期（≤60s），仍被持有则启动失败；要并行跑多个 runner 请给每个进程不同的 `--runner-id`。被接管时进程以 0 退出（systemd 不再重启），由新持有者继续。
- `--service-unit`：记录 systemd 单元名（默认 `nuwa-work`）。控制台的「重启 unit」动作会消费它（缺失/非法时该动作禁用或 422）
- `--daily-token-limit` / `--monthly-token-limit`：每仓库每日 / 滚动 30 天 token 上限（默认 5M / 50M；失败尝试也计入）
- 密钥隔离：work 启动时把 `NUWA_GITHUB_TOKEN`/`GITHUB_TOKEN`/`ZAI_API_KEY`/`ANTHROPIC_API_KEY` 读入内存并从 env 移除；bash/验证子进程只收到白名单（PATH/HOME/GOCACHE 等）。非敏感的额外变量用 `NUWA_AGENT_ENV_ALLOW` 逗号追加。ssh-agent / credential store 仍需部署侧隔离（建议 runner 专用用户）。
- 指令文件：tracked 配置以 `origin/<base>:nuwa.repo.json` 为权威；untracked 配置首次启动复制到 `<sessions>/_trusted/`。每轮领单前校验并恢复被改写的文件，`nuwa.repo.json` 本身也在隐式禁改名单。合并到 main 的新配置会在下一轮恢复文件，但 runner 进程内的配置对象要重启后再生效。
- pr_open 自动对账：serve 每 `NUWA_SYNC_INTERVAL` 秒（默认 600）对账一次，与手动 `nuwa sync` 共用锁。
- 通知（可选）：`NUWA_WEBHOOK_URL`、`NUWA_WEBHOOK_SECRET`、`NUWA_WEBHOOK_EVENTS`；事件 JSON 带 `id`（每个事件唯一）、`ts_ms` 与 `X-Nuwa-Signature: sha256=<HMAC>`（签名覆盖整个 body，含 `id`/`ts_ms`）。默认订阅 `escalated,ignored,provider_stand_down,heartbeat_stale,ci_red,sync_error,pr_opened,runner_error`。进程退出前会等待队列清空（最多 10s），`escalated` 不再随退出丢失；**接收方必须校验 `ts_ms` 新鲜度并按 `id` 去重**，否则捕获的 body 可被重放。
- 会话保留：work 启动及运行中定期清理超过 365 天无新 run 的 `sessions/<fp>` 目录；`_trusted` 永不清理。

## 5 · 第一个真实系统接入（mindloop，进行中）

| 项 | 值 |
|---|---|
| 仓库 | `github.com/lomehong/mindloop`（私有） |
| 采集点 | `cmd/robotd`（桌面 MCP 守护进程，无人值守形态）——`qingniao.Capture` + HTTPSink |
| 指令文件 | 仓库根 `nuwa.repo.json`（failing_check 排除 web/static npm 噪音） |
| 待办 | 所有者：推送 mindloop 至 GitHub + token 仓库列表加 mindloop；runner 服务器 clone + work 指向 |

## 4 · 日常运维命令

**Web 控制台（v3 SPA，0.2.0 起是唯一控制台）**：`http://<server>:7321/`，在 `/login` 用 `NUWA_UI_TOKEN` 换取 HttpOnly 会话（**不再使用 Basic 头**）。`/` 就是 SPA 控制台；`/ui*` 308 永久重定向到 `/`，`/issue/{fp}` 重定向到 `/issues/{fp}`，`/settings` 直接由 SPA 提供；`/ingest` 与 `/health` 的路径和鉴权不变。SPA 提供总览、问题单列表/详情（含会话转录）、运行器、活动与设置。

> **控制台令牌与上报令牌完全分离**：`NUWA_UI_TOKEN` 缺失时控制台进入本地模式，且**非回环监听会拒绝启动**。`NUWA_INGEST_TOKEN` 部署在每个被采集的服务上（泄露后果只是垃圾信号），绝不能当控制台凭据用——控制台能读设置页里的 LLM key。升级后只设了上报令牌的公网部署会启动失败，需补 `NUWA_UI_TOKEN`。`/api/v1/auth/logout` 会服务端撤销会话（复制走的 cookie 立即失效）；撤销表在内存中，重启清空——要强制所有设备下线就轮换 `NUWA_UI_TOKEN`。

- **问题单动作**：escalated/rejected/ignored 可 `重新打开`（回到 `new`、清冷却并重置 attempts）；new/escalated/rejected 可 `忽略`（填写原因；ignored 后同指纹报告只计数，不再自动修复）；卡单（`triggered`/`fixing` 且超过卡单窗口）可 `重排`。动作出现在详情头部、列表与总览待处理队列；成功 toast、失败逐条显示服务端错误。CLI 对应 `nuwa reopen --reason` / `nuwa ignore --reason`，理由会写入事件并在下一轮 prompt 中作为 OWNER NOTE。
- **对账与重启**：`GitHub 对账` 汇总 checked/merged/rejected/still_open，并逐条展示 `errors[]`；runner `重启 unit` 先确认影响（显示当前修复指纹，提示同一 unit 承载的多个 runner 会一起重启）。409 表示已有重启在排程，422 指出 `service_unit` 缺失/非法，501 直接给出手动 `systemctl restart <unit>` 命令。
- **设置**：按 29 键 schema 分组编辑；密钥只显示「已设置 · 末 4 位」，留空保存表示保持不变；只提交改动键，保存写 `NUWA_ENV_FILE`（自动 `.bak` 备份）并提示 `重启后生效`。`测试 GitHub token` / `测试模型配置` 使用未保存的候选值探测（不落盘）；`重启服务` 在存在活动 run 时返回 409 `active_runs`，必须勾选「我了解这会中断正在进行的修复」后才允许 `强制重启`，202 后页面轮询 `/health` 与 `/auth/me` 等待恢复，systemd 不可用则显示手动命令。修改 `NUWA_UI_TOKEN` 后重启会使当前会话失效，页面会提示重新登录；有未保存的草稿时重启按钮禁用，需先保存或放弃。重启恢复后控制台回到 SPA 根路径 `/`。

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
| `nuwa ingest --signal s.jsonl [--db p]` | 投喂采集信号（单对象或 JSONL，离线/文件形态） |
| `nuwa status [--db p]` | 全部问题单：状态/严重度/频次/PR 链接 |
| `nuwa work …` | 领单 → 修复 → 验证 → 交付（`--no-deliver` 只修不交；`--runner-id`/`--service-unit` 见第 3 节） |
| `nuwa reopen --db p --fingerprint FP` | 人工复核升级单后重新入队（清冷却） |
| `nuwa fix --issue … --repo …` | 手工指定问题单跑一次（调试用） |
| `nuwa migrate-env [--env-file p]` | 部署期把历史小写 env 键迁移为大写；写前备份 `.bak`，Unix 原子替换 |

**v0.1.7 升级注意（env 键迁移）**：升级前停 serve/work，备份 `nuwa.env`，运行 `nuwa migrate-env --env-file /opt/nuwa/nuwa.env`，确认键为大写后启动。若直接启动新二进制，它会在启动时迁移文件，但 systemd 已把旧环境注入当前进程；需再重启一次 serve/work，并用无凭证访问 `/` 确认跳转到 `/login`（登录页）。

**同源校验与代理**：`/api/v1` 写请求在 CSRF 之外还比较 `Origin` 的 host 与请求 `Host`（无 `Origin` 时回退 `Referer`）。若前置反向代理改写 Host（如 `proxy_set_header Host $proxy_host`）或使用 HTTP/2 上游，合法 POST 会被 403；代理需保留 Host 并使用 HTTP/1.1 上游。当前直连 7321 的部署不受影响。

## 5 · 只读 API（v3 控制台）

`/api/v1` 的六个只读端点（均为 GET，响应 JSON，带 `Cache-Control: no-store` 与严格 CSP）：

| 端点 | 用途 |
|---|---|
| `GET /api/v1/overview?repo=` | 总览：状态漏斗、需要处理、活跃 run、可领队列、runner 健康、24h CI |
| `GET /api/v1/issues?state=&severity=&repo=&language=&q=&stuck=1&attention=1&sort=&order=&page=&page_size=` | 问题单列表（过滤/排序/分页） |
| `GET /api/v1/issues/{fp}` | 问题单详情：堆栈样本、时间线、最近 10 次 run、同名相似单 |
| `GET /api/v1/runners` | runner 健康与 effective 配置 |
| `GET /api/v1/events?kind=&repo=&fingerprint=&from_ms=&to_ms=&aggregate=1&page=&page_size=` | 活动流（`aggregate=1` 按 fingerprint+UTC 小时聚合 `reported`） |
| `GET /api/v1/metrics/trends?range=7d\|14d\|30d\|90d&repo=` | UTC 日桶趋势与区间合计 |

- token 模式下 GET 也要求有效 `nuwa_session` cookie（先 `POST /api/v1/auth/login`）；本地模式（未设 `NUWA_UI_TOKEN`/`NUWA_INGEST_TOKEN`）免会话。
- 非 GET 请求另需 `X-NUWA-CSRF` 头，其值须与 `nuwa_csrf` cookie 一致（双提交校验）；GET 不要求 CSRF 头。**本地模式同样强制 CSRF**（防止浏览器跨站驱动本机控制台），前端必须先 `GET /api/v1/auth/me` 拿到可读的 `nuwa_csrf` cookie。

## 6 · 动作、设置、系统与转录 API（v3 控制台 M2c）

所有写操作都在 `/api/v1` 的会话 + CSRF 中间件之后：非 GET 必须同时带 `nuwa_csrf` cookie 与同值的 `X-NUWA-CSRF` 头（缺失/不一致 → 403 `forbidden`）；token 模式还需先登录。响应统一 `Cache-Control: no-store` 与严格 CSP。以下成功/失败码与 docs/09 §9.2 一致。

### 6.1 问题单动作

| 端点 | body | 成功 | 失败 |
|---|---|---|---|
| `POST /api/v1/issues/{fp}/reopen` | `{"reason":"…"}`（可选） | 204，状态回到 `new` 并清 owner | 404 未知单；409 非 `escalated`/`rejected`；422 body 类型错误 |
| `POST /api/v1/issues/{fp}/requeue` | `{"force":true,"reason":"…"}`（可选） | 204，卡单回到 `new` 并清 owner | 404；409 非 `triggered`/`fixing` 或未卡单；422 |

- `reason` 为前端契约字段，当前**不落库**；审计事件固定为 `state_changed`（detail `reopened by owner`）与 `requeued`（detail `stuck requeued by owner`），actor 均为 `owner`。
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

- **事实源**：`NUWA_ENV_FILE`（默认 `/opt/nuwa/nuwa.env`，systemd `EnvironmentFile`）。设置页不写 shell 变量、不热更新进程；保存后 `restart_pending=true`，需重启 serve/work 才生效。
- `GET /api/v1/settings`：返回 29 键 schema（分组/类型/默认/scope/是否密钥/是否只读）、掩码值、`env_path/env_exists/env_permissions`、`needs_migration`、`restart_pending`、`effective_diff`（runner 心跳的实际配置 vs 文件值，仅非密钥）、`unknown_keys`（只给键名）、`last_changes`。密钥只返回 `set/last4/updated_at_ms`，**永不回显值**；四位以内不返回 `last4`。
- `PUT /api/v1/settings`：`{"values":{"NUWA_INTERVAL":"60","NUWA_GITHUB_TOKEN":""}}`。空密钥 = 保持不变；未知键与只读键（`NUWA_ENV_FILE`）422；任一字段非法则整单 422 且文件逐字节不变。成功保留注释/顺序/未知键，更新既有行或追加新键；写前备份 `<env>.bak`，Unix 同目录临时文件 + chmod 600 + rename。每个变更键写 `config_changed`（detail=key，actor=owner），历史小写已知键先迁移并写 `config_migrated`。
- `POST /api/v1/settings/test`：`{"target":"github|llm","values":{…}}`；`values` 是未保存的候选值（不落盘、不写日志），未知键 422；**每个已知键的取值都做类型校验**——只接受字符串、数字或布尔（env 标量），对象/数组/null 一律 422（不会静默忽略后去测别的值）。github 调 `GET https://api.github.com/rate_limit`（不耗配额）；llm 用 `max_tokens=1` 的真实请求（可能产生极小费用）。空 model/base/provider 视为未设置（回退环境变量/默认值）；anthropic 的 base 固定 `https://api.anthropic.com`（忽略 `NUWA_BASE_URL`），模型取自 vendored catalog，未知名回退 catalog 首项；zai 默认 base `https://open.bigmodel.cn/api/coding/paas/v4`、默认模型 `glm-5.3-flash`；faux 完全离线。显式空密钥不回落环境变量，直接 422。网络失败/非 2xx/10s 超时 → 502，且不回显 provider 响应体。
- `POST /api/v1/settings/restart`：`{"force":false}`。有未结束 run（`runs.finished_at IS NULL`）→ 409 `conflict`，body 含 `active_runs`；`force=true` 才继续。重启目标 = `NUWA_SERVE_UNIT`（默认 `nuwa-serve`）+ `NUWA_RUNNER_UNITS`（默认 `nuwa-work`，逗号分隔），逐个白名单校验；先写 `restart_requested` 审计（`__system__`），再 `systemd-run --on-active=2`，成功 202。无 unit 配置或 `systemd-run` 不可用 → 501 并给出手动命令。改 `NUWA_UI_TOKEN` 后重启会使当前会话失效，需重新登录。

### 6.4 系统与转录

- `GET /api/v1/system`：`version/build_commit/started_at_ms/uptime_secs/db_path/db_size_bytes/local_mode/services[{unit,active}]`。systemd 不可用或 `systemctl is-active` 无输出时 `active="n/a"`，不算错误；本地模式会在页面显示无鉴权警告。
- `GET /api/v1/issues/{fp}/transcript?run_id=&page=1&page_size=20`：省略 `run_id` 时取该指纹**最近一次有 `session_dir` 的 run**；指定不属于该指纹的 run → 404 `run not found`；未知单 → 404 `issue not found`。只解析 `kind=entry,type=message`，映射 user / assistant（thinking、text、tool_call）/ tool_result；其它记录（header/record/model_change/custom）忽略。单个块（text/thinking/tool_result/序列化后的 tool_call 参数）按 UTF-8 字节上限 16KB 截断并标 `truncated=true`；**超限 tool_call 的 `arguments` 会从 JSON 对象变成字符串预览 `<truncated: N bytes>`**（前端需同时处理 object 与 string 两种类型），避免单个调用占满页面。文件只读末尾 2MB（更早内容丢弃并标 `meta.truncated=true`），按消息分页：`page_size` 默认 **20**、最大 **100**（100 是上限不是默认值），越界或非数字 422；无 run/无会话返回 `data:[]`、`meta.empty=true`。转录路径由 DB 的 run 行派生（必须是绝对路径且不含 `..`），从不接受客户端路径；`work --sessions` 建议传绝对路径，否则失败/中断的 run 可能没有可读转录。

## 修复基准（可选）

固定种子场景验证“修复流水线 + 验证门 + 交付门”是否退化：

```bash
nuwa benchmark list                       # 内置场景目录
nuwa benchmark run --provider faux        # 确定性流水线基准（不需要 API key）
nuwa benchmark run --provider zai --model glm-5.3-flash   # 真实模型质量基准
nuwa benchmark run --provider faux --scenario node-empty-average,go-off-by-one --json
```

- `faux` 只证明流水线与门禁，不证明模型修复质量；`live` 才衡量模型，比较时固定 provider/model/场景集。
- 每次运行写 `benchmark_runs` / `benchmark_results`；控制台 `/benchmarks` 看趋势、误报与逐场景证据。
- 负对照（`expect_fixed=false`）必须保持“未修复”，否则说明门禁退化。
- 基准在独立 CLI 进程运行；serve 不启动模型循环。

## 控制台里的技能与记忆

- `/projects/<repo>/skills`：内置技能 + 目标仓 `.nuwa/skills/*.md` 的只读目录，展示来源、版本、覆盖关系与解析告警；控制台不编辑目标仓文件。
- `/projects/<repo>/memory`：验证门 + CI 证明过的修复记忆；可停用（保留来源、不再注入）或删除，操作写 owner 审计事件。
- 记忆由 runner 自动写入，模型自述不会进入记忆；每仓最多 50 条。

## 问题单状态机（运维视角）

```
new → triggered → fixing → pr_open → merged | rejected
                       └→ escalated ←──(交付/修复失败自动)
merged/rejected 再报同指纹 → 自动重开(new，回归)
escalated 只能人工 reopen → new
```

## 护栏复核（ADR-003 五件套在代码里的位置）

1. 分支隔离 `agent/fix/*`：`deliver.rs`
2. 频控/预算：每日 PR 上限 + 每问题单 token 上限 + turn 上限（`main.rs` 三闸）
3. CI 闸门：PR 模板要求；合并人工
4. 禁改路径：`guard.rs` `before_tool_call` 硬否决（编辑路径、shell 命令双检查）
5. 复现验证证据：`deliver.rs` 独立重跑 failing_check，证据落 `sessions/<fp>/verification.txt`

## 已知边界（v0.2 hardening）

- runner 串行；并发池与 runner 镜像矩阵（A-3 后半）未做
- 真实 LLM / PR 已闭环（spike #3/#5/#6；mindloop #7 已合并）
- 验证探针协议（A-4）按设计推迟到第一个环境特定 bug 出现
- ingest HTTP 服务端已建（`nuwa serve` + `POST /ingest`）
- agent 沙箱只在进程 env 层隔离；ssh-agent / credential store 仍建议用独立 OS 用户/容器隔离
- 通知是窄范围状态 webhook，不是通用告警路由/指标采集系统
