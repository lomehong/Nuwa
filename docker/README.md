# Runner 镜像矩阵

A-3 的部署侧产物：每个目标技术栈一个 runner 镜像，内含静态 `nuwa` 与对应工具链。CI 的 `docker` job 每次构建全部镜像做验证；**推送到 registry 由所有者执行**（本仓不自动发布镜像）。

| 镜像 | 工具链 | 适用目标仓 |
|---|---|---|
| `nuwa-runner-base:local` | git、bash、ssh、CA | 只需要预置检查命令的仓库 |
| `nuwa-runner-go:local` | Go | Go |
| `nuwa-runner-rust:local` | rustc、cargo、build-base、openssl-dev | Rust |
| `nuwa-runner-node:local` | node、npm | Node/TypeScript |
| `nuwa-runner-python:local` | python3、pip | Python |

## 构建

```bash
# 1) 静态二进制（musl）
cargo build --release -p nuwa --target x86_64-unknown-linux-musl
# 2) 基镜像 + 四种语言镜像
bash docker/build.sh target/x86_64-unknown-linux-musl/release/nuwa
```

也可以直接用发布产物：`bash docker/build.sh /path/to/nuwa-linux-x64-musl`。

## 运行

一个容器 = 一个 runner。**每个容器的 checkout 必须挂到不同路径**（例如 `/work/w1`、`/work/w2`）：runner 的 checkout 互斥按 `repo_path` 比较，两个容器都挂 `/work` 会被当成同一份 checkout 拒绝。

**下面是现有整套 runner 的运行示例，不是执行/交付权限分离方案**：它把修复、验证与持有发布权限的交付放在同一 runner 中。非 root、env 白名单、Batch4 路径护栏均不能把任意 shell/候选代码变成 OS 沙箱；不要把该示例用于运行不受信任代码的安全承诺。

```bash
docker run --rm -it \
  -v /srv/repos/myapp-w1:/work/w1 \
  -v /srv/nuwa/data:/data \
  -v /srv/nuwa/sessions:/sessions \
  -v /srv/nuwa/cache/go:/home/nuwa/go \
  -e NUWA_GITHUB_TOKEN -e ANTHROPIC_API_KEY \
  nuwa-runner-go:local work \
    --db /data/shanhaijing.db \
    --instructions /work/w1/nuwa.repo.json \
    --sessions /sessions \
    --provider anthropic --pr --loop \
    --runner-id go-1
```

- `--runner-id` 每个容器唯一；`--pool N` 可在一个容器内再起 N 个 worker（#1 用 `/work/...` 主 checkout，#2..N 用临时 worktree）。
- `NUWA_GITHUB_TOKEN`/provider key 只从环境变量传入，镜像内不烘焙任何凭证。
- 容器以 uid 10001 的非 root 用户运行；宿主 checkout 需对该 uid 可写（或运行时 `--user` 对齐宿主 uid）。
- 每个问题单的修复/验证都在 runner 内执行；生产宿主不应把该容器当通用 shell。

## Batch4 · 验证来源与容器安全边界

- 当前代码基线 v0.17.0，Batch4 已本地实现、最终检查待完成，**DB schema 仍为 v12**；没有本批镜像部署、Unix、真实 GitHub、systemd 或付费模型验证。Batch5 observations/schema v13 下一批，Batch6 reservations/schema v14 后续，均未实现。
- 目标仓 `nuwa.repo.json` 的 `verification_policy` 默认 `compatibility`，无 oracle 证据为较弱 `legacy_untrusted`，**不会静默切 strict**。显式 `strict` 缺 `trusted_verification` 会在模型、本地交付分支、staging/push/PR 前拒绝；任何策略配置了 owner oracle 都必须通过，不能失败后降级。配置与操作见 [接入手册 Batch4](../docs/08-onboarding.md#batch4--所有者可信验证与路径护栏adr-026)。
- owner 维护字面量入口与显式完整 `protected_inputs` 依赖闭包，修复前从 tracked base 或 owner 预置私有文件冻结 bytes/mode/manifest 到 checkout 外。入口从冻结目录启动，base/candidate 重置到同一物理 target，不带 Git 元数据/侧别标签，使用最小环境；预期 bug exit 1、通过 0、意外 setup 故障 2+。闭包完整性由 owner 负责，任意故障 oracle 退出 1 不构成数学证明。
- 内置文件工具区分禁写与禁读禁写，真实环境适配器覆盖递归读取，拒绝 symlink/junction/reparse。**shell、检查命令、候选程序仍享有运行用户的宿主权限**；路径检查仍有 TOCTOU/hardlink 局限，外部冻结目录与私有 HOME 不是权限隔离。
- **待平台授权、尚未实现/验证**：将执行放到独立低权限用户/环境，和持有 push/PR 权限的交付分离；不挂宿主 HOME、ssh-agent/credential store 或发布密钥，严格限制可写挂载与网络出口。现有缓存卷/同容器 runner 不是该能力；不要自行把发布凭证搬进候选执行环境。
- 本地 faux 验收入口 `nuwa benchmark batch4 --output <新建临时 JSON>` 为 15 场景 × 3 trials，只写安全元数据/摘要与声明/结果 sidecar；无 live 模型、无默认业务 DB。首次 CLI 在 trials 前栈溢出，future 装箱后重跑仍进行中，不能宣称验收通过；分时点测试证据与最终待办见 [docs/07 §D](../docs/07-open-items.md#d--进度快照2026-10-15--阶段-01-完成)。

## 依赖缓存（离线/提速）

按栈挂载缓存目录，跨容器复用：

| 栈 | 容器内路径 |
|---|---|
| Go | `/home/nuwa/go` |
| Rust | `/home/nuwa/.cargo` |
| Node | `/home/nuwa/.npm` |
| Python | `/home/nuwa/.cache/pip` |

## 发布（所有者执行）

```bash
docker tag nuwa-runner-go:local ghcr.io/<owner>/nuwa-runner-go:v0.15.0
docker push ghcr.io/<owner>/nuwa-runner-go:v0.15.0
```

镜像只包含静态 `nuwa`、工具链与基础工具；数据库、仓库 clone、sessions 与密钥都通过挂载/环境变量进入。
