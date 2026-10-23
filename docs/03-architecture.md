# 03 · 架构共识

状态：已确认（2026-10-15）；运行器池调度等细节见 [07-open-items.md](07-open-items.md)

## 总览

```mermaid
flowchart TB
    subgraph SITES["各系统现场（资源受限）"]
        C1[Go 采集 SDK]
        C2[Java 采集 SDK]
        C3[Rust 采集 SDK]
    end
    C1 & C2 & C3 -->|"上下文包（脱敏）"| Q[服务端：问题单库 山海经<br/>指纹去重/冷却/优先级]
    Q --> R[服务端 Agent 运行器池<br/>应龙 = rpi 裁剪版常驻]
    R -->|按技术栈选 runner| V1[Go runner] & V2[Java runner] & V3[Rust runner]
    V1 & V2 & V3 --> PR[开 PR → CI 闸门]
    PR -.环境特定 bug：下发补丁到现场<br/>跑预定义验证脚本，回报结果.-> SITES
```

## 演进过程（记录被否决的中间形态）

1. **初版设想**：agent 嵌入各系统进程内，"现场修复"。→ 被否决：生产现场通常只有构建产物没有源码；资源受限环境装不下开发运行时；LLM 密钥与 shell 执行面扩散到每个现场。
2. **两模式折中**：现场具备条件则现场修复，否则上传服务端。→ 被否决："具备源码 + 工具链的现场"本质就是 runner，两套 agent 部署形态成本翻倍。
3. **最终收敛**：**现场永远只采集，修复恒在服务端**；"模式轴"切在验证位置——现场仅保留下发**预定义验证脚本**的权利（打补丁、跑复现、回报 pass/fail），绝不把自由 agent 派到现场。

## 组件职责

| 组件 | 神话名 | 职责 | 技术形态 |
|---|---|---|---|
| 现场采集 SDK | 青鸟 Qingniao | 采集运行时问题信号 → 指纹化 → 打包上下文 → 上报 | 按系统语言编写的薄库（Go/Java/…各一），无 LLM、无 shell 执行 |
| 问题单库 | 山海经 Shanhaijing | 指纹去重、冷却窗口、优先级、状态机、问题单存储 | 服务端组件（存储选型待定） |
| Agent 运行器池 | 应龙 Yinglong | 领取问题单 → 在目标仓库上定位/复现/修改/验证 → 开 PR | rpi 裁剪版（rpi-agent + rpi-ai + rpi-tools + rpi-harness） |
| 补丁单元 | 五色石 Wuse Stone | 一次修复产出的补丁及其验证证据的封装 | 补丁格式 + 元数据规范（待设计） |
| 验证探针 | （命名待定） | 环境特定 bug：服务端下发预定义验证脚本到现场执行并回报 | 现场只执行固定脚本，不运行自由 agent |

## 修复流水线（单问题单生命周期）

```
问题单就绪 → repo 过滤领取（闸门：每 issue/每日/每月 token 上限 + 每仓库每日 PR 上限 + attempt cap）
  → fetch + reset 最新 origin/<base>（失败回滚预检）
  → runner 选型（按仓库技术栈）
  → clone 目标仓库（私有，deploy key）
  → fetch/reset 后读取权威指令文件（tracked: 本轮 base commit；untracked: 已校验的 runner 私有副本；每轮校验防篡改）
  → 定位与复现（结合上下文包 + 最近 3 次 run/owner note）
  → 修改（五色石：补丁 + 修改说明）
  → 验证（base 红 / 分支绿；reproduce_check 或 .nuwa/repro.sh；failing_check 回归门；diff glob 禁改终检）
  → 开分支 agent/fix/<issue-fingerprint> → 直接开 PR（正文含 issue 链接、根因、风险画像、前序尝试、验证证据、堆栈）
  → CI 闸门：不过测试 → 自动关 PR，问题单 rejected 并累计 attempt
  → 合入/关闭由人工决定；serve 定期 reconcile pr_open，终态分支清理
  → 状态变更通过窄范围 HMAC webhook 推给所有者
```

## 安全与护栏（自主开 PR 的前提，缺一不可）

1. 分支命名空间隔离：`agent/fix/*`，永不碰 `main` 等保护分支；终态清理，失败回滚
2. 预算：每问题单 token 上限 + 每仓库每日/滚动 30 天 token 上限（失败尝试也计入）+ 每仓库每日 PR 上限 + attempt cap
3. 合并闸门交给 CI：PR 必须过测试，人工合并；serve 自动 reconcile pr_open
4. 禁改路径：工具调用前否决 + 交付面对 diff 做 glob 终检；`nuwa.repo.json` 隐式禁改
5. 复现验证证据：base 红 / 分支绿；配置 `reproduce_check` 或分支 `.nuwa/repro.sh`；base 绿且无复现则 escalate
6. 密钥隔离：work 启动清秘、bash/验证子进程 env 白名单、PR/证据/转录脱敏
7. 指令文件完整性：tracked 配置读 base commit，untracked 用 runner 私有副本，每轮校验恢复
8. 挂死可观测：心跳（liveness）与 progress_at（真实进度）分离；15 分钟无进度触发 webhook
