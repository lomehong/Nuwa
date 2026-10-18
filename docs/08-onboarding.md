# 08 · 接入手册

状态：v0（2026-10-15，随 runner v0 落地）

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

Go 系统（`sdk/qingniao-go`）：

```go
import "example.com/nuwa/qingniao"

// panic 采集（进程仍会崩，青鸟只负责报信）：
qingniao.Capture(sink, qingniao.Meta{Repo: "example.com/your-repo", Service: "api"}, run)

// 手动上报非 panic 错误：
qingniao.Report(sink, qingniao.Meta{Repo: "…"}, "healthcheck failed", err)
```

sink 二选一：`FileSink{Path}`（JSONL，落盘后 `nuwa ingest`）或 `HTTPSink{Endpoint}`（POST 到 ingest 服务端，待建）。
信封 schema = `qingniao/v1`，是 ingest Signal 的超集；新增字段不破坏旧服务端。

## 2 · 目标仓库放指令文件

仓库根放 `nuwa.repo.json`（并建议加进该仓库 `.gitignore`——交付面已排除它，双保险）：

```json
{
  "repo": "example.com/your-repo",
  "failing_check": { "command": "go test ./..." },
  "forbidden_paths": [".github/", "deploy/", "*.pem", "go.mod", "go.sum"],
  "notes": "给修复 runner 的自由文本说明（规范、禁区原因、注意事项）"
}
```

- `repo` 必须与采集信封里的 repo 一致，runner 不匹配即拒单（ADR-004 防越权）
- `failing_check.command` 是"当前失败、修复后必须过"的命令，交付前 nuwa 会**独立重跑**它
- `forbidden_paths` 由 `before_tool_call` 硬否决，不靠模型自觉

## 3 · 启动 runner（应龙）

```bash
# 常驻轮询（串行 v0；每仓库一个 runner 实例，避免同仓库 git 竞争）
nuwa work --db shanhaijing.db \
  --instructions /path/to/your-repo/nuwa.repo.json \
  --runner-id <host:repo> --service-unit nuwa-work \
  --provider anthropic --pr --loop --interval 30
```

- `--runner-id`：控制台可见的 runner 身份，默认 `hostname:repo`；仅保留字母数字与 `.:_-`，超长截断
- `--service-unit`：记录 systemd 单元名（默认 `nuwa-work`）。控制台的「重启 unit」动作会消费它（缺失/非法时该动作禁用或 422）

## 4 · 日常运维命令

**Web 控制台（v3 SPA，0.2.0 起是唯一控制台）**：`http://<server>:7321/`，在 `/login` 用 `NUWA_UI_TOKEN` 换取 HttpOnly 会话（**不再使用 Basic 头**）。`/` 就是 SPA 控制台；`/ui*` 308 永久重定向到 `/`，`/issue/{fp}` 重定向到 `/issues/{fp}`，`/settings` 直接由 SPA 提供；`/ingest` 与 `/health` 的路径和鉴权不变。SPA 提供总览、问题单列表/详情（含会话转录）、运行器、活动与设置。

- **问题单动作**：rejected/escalated 可 `重新打开`（立即回到 `new` 并清冷却，无确认）；卡单（`triggered`/`fixing` 且超过卡单窗口）可 `重排`（破坏性确认，清归属重新入队）。动作出现在详情头部、列表与总览待处理队列；成功 toast、失败逐条显示服务端错误。
- **对账与重启**：`GitHub 对账` 汇总 checked/merged/rejected/still_open，并逐条展示 `errors[]`；runner `重启 unit` 先确认影响（显示当前修复指纹，提示同一 unit 承载的多个 runner 会一起重启）。409 表示已有重启在排程，422 指出 `service_unit` 缺失/非法，501 直接给出手动 `systemctl restart <unit>` 命令。
- **设置**：按 22 键 schema 分组编辑；密钥只显示「已设置 · 末 4 位」，留空保存表示保持不变；只提交改动键，保存写 `NUWA_ENV_FILE`（自动 `.bak` 备份）并提示 `重启后生效`。`测试 GitHub token` / `测试模型配置` 使用未保存的候选值探测（不落盘）；`重启服务` 在存在活动 run 时返回 409 `active_runs`，必须勾选「我了解这会中断正在进行的修复」后才允许 `强制重启`，202 后页面轮询 `/health` 与 `/auth/me` 等待恢复，systemd 不可用则显示手动命令。修改 `NUWA_UI_TOKEN` 后重启会使当前会话失效，页面会提示重新登录；有未保存的草稿时重启按钮禁用，需先保存或放弃。重启恢复后控制台回到 SPA 根路径 `/`。

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
- `GET /api/v1/settings`：返回 22 键 schema（分组/类型/默认/scope/是否密钥/是否只读）、掩码值、`env_path/env_exists/env_permissions`、`needs_migration`、`restart_pending`、`effective_diff`（runner 心跳的实际配置 vs 文件值，仅非密钥）、`unknown_keys`（只给键名）、`last_changes`。密钥只返回 `set/last4/updated_at_ms`，**永不回显值**；四位以内不返回 `last4`。
- `PUT /api/v1/settings`：`{"values":{"NUWA_INTERVAL":"60","NUWA_GITHUB_TOKEN":""}}`。空密钥 = 保持不变；未知键与只读键（`NUWA_ENV_FILE`）422；任一字段非法则整单 422 且文件逐字节不变。成功保留注释/顺序/未知键，更新既有行或追加新键；写前备份 `<env>.bak`，Unix 同目录临时文件 + chmod 600 + rename。每个变更键写 `config_changed`（detail=key，actor=owner），历史小写已知键先迁移并写 `config_migrated`。
- `POST /api/v1/settings/test`：`{"target":"github|llm","values":{…}}`；`values` 是未保存的候选值（不落盘、不写日志），未知键 422；**每个已知键的取值都做类型校验**——只接受字符串、数字或布尔（env 标量），对象/数组/null 一律 422（不会静默忽略后去测别的值）。github 调 `GET https://api.github.com/rate_limit`（不耗配额）；llm 用 `max_tokens=1` 的真实请求（可能产生极小费用）。空 model/base/provider 视为未设置（回退环境变量/默认值）；anthropic 的 base 固定 `https://api.anthropic.com`（忽略 `NUWA_BASE_URL`），模型取自 vendored catalog，未知名回退 catalog 首项；zai 默认 base `https://open.bigmodel.cn/api/coding/paas/v4`、默认模型 `glm-5.3-flash`；faux 完全离线。显式空密钥不回落环境变量，直接 422。网络失败/非 2xx/10s 超时 → 502，且不回显 provider 响应体。
- `POST /api/v1/settings/restart`：`{"force":false}`。有未结束 run（`runs.finished_at IS NULL`）→ 409 `conflict`，body 含 `active_runs`；`force=true` 才继续。重启目标 = `NUWA_SERVE_UNIT`（默认 `nuwa-serve`）+ `NUWA_RUNNER_UNITS`（默认 `nuwa-work`，逗号分隔），逐个白名单校验；先写 `restart_requested` 审计（`__system__`），再 `systemd-run --on-active=2`，成功 202。无 unit 配置或 `systemd-run` 不可用 → 501 并给出手动命令。改 `NUWA_UI_TOKEN` 后重启会使当前会话失效，需重新登录。

### 6.4 系统与转录

- `GET /api/v1/system`：`version/build_commit/started_at_ms/uptime_secs/db_path/db_size_bytes/local_mode/services[{unit,active}]`。systemd 不可用或 `systemctl is-active` 无输出时 `active="n/a"`，不算错误；本地模式会在页面显示无鉴权警告。
- `GET /api/v1/issues/{fp}/transcript?run_id=&page=1&page_size=20`：省略 `run_id` 时取该指纹**最近一次有 `session_dir` 的 run**；指定不属于该指纹的 run → 404 `run not found`；未知单 → 404 `issue not found`。只解析 `kind=entry,type=message`，映射 user / assistant（thinking、text、tool_call）/ tool_result；其它记录（header/record/model_change/custom）忽略。单个块（text/thinking/tool_result/序列化后的 tool_call 参数）按 UTF-8 字节上限 16KB 截断并标 `truncated=true`；**超限 tool_call 的 `arguments` 会从 JSON 对象变成字符串预览 `<truncated: N bytes>`**（前端需同时处理 object 与 string 两种类型），避免单个调用占满页面。文件只读末尾 2MB（更早内容丢弃并标 `meta.truncated=true`），按消息分页：`page_size` 默认 **20**、最大 **100**（100 是上限不是默认值），越界或非数字 422；无 run/无会话返回 `data:[]`、`meta.empty=true`。转录路径由 DB 的 run 行派生（必须是绝对路径且不含 `..`），从不接受客户端路径；`work --sessions` 建议传绝对路径，否则失败/中断的 run 可能没有可读转录。

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

## 已知边界（v0）

- runner 串行；并发池与 runner 镜像矩阵（A-3 后半）未做
- 真实 PR 未开过（等 token）；真实 LLM 修复质量未验证（等 key）
- 验证探针协议（A-4）按设计推迟到第一个环境特定 bug 出现
- ingest 的 HTTP 服务端形态未建（现用 CLI/文件投喂）
