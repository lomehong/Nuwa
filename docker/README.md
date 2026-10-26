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
