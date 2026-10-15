# 08 · 接入手册

状态：v0（2026-10-15，随 runner v0 落地）

把一个自有系统接入 Nuwa = 下面四步。全程不改 nuwa 本体一行代码（ADR-006）。

## 0 · 服务端一次性准备

| 项 | 说明 |
|---|---|
| 工具链 | Rust（cargo 1.93+，构建 nuwa）、Git；每个目标仓库的技术栈工具链（runner 要能跑它的测试命令） |
| `NUWA_GITHUB_TOKEN` | 细粒度 PAT：目标仓勾 **Pull requests: Read and write** + **Contents: Read-only**（开 PR 需读 head/base commit；push 走 deploy key，token 无需写内容）。缺 Contents:Read 时 API 422 `not all refs are readable` |
| deploy key | 目标仓库的只读+写分支 deploy key，配进 runner 的 ssh-agent（ADR-004：服务端持有，最小权限） |
| `ANTHROPIC_API_KEY` | 可选；不配则只能 `--provider faux`（离线验证管道，不产生真实修复） |

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
  --provider anthropic --pr --loop --interval 30
```

## 4 · 日常运维命令

| 命令 | 用途 |
|---|---|
| `nuwa serve --db p [--listen 127.0.0.1:7321]` | HTTP ingest 服务端：`POST /ingest`（单信封或 JSONL，`application/json`），`GET /health`。青鸟 `HTTPSink` 直连 |
| `nuwa ingest --signal s.jsonl [--db p]` | 投喂采集信号（单对象或 JSONL，离线/文件形态） |
| `nuwa status [--db p]` | 全部问题单：状态/严重度/频次/PR 链接 |
| `nuwa work …` | 领单 → 修复 → 验证 → 交付（`--no-deliver` 只修不交） |
| `nuwa reopen --db p --fingerprint FP` | 人工复核升级单后重新入队（清冷却） |
| `nuwa fix --issue … --repo …` | 手工指定问题单跑一次（调试用） |

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
