# 09 · Web 控制台设计（v2 定稿）

状态：已评审定稿（2026-10-16）。决策：D1 挂 serve 进程内服务端渲染；D2 独立 `NUWA_UI_TOKEN`（fallback 上报令牌）；D3 合并动作为 GitHub 深链；设置页存储模型 A（env 文件编辑器 + 重启编排）。

## 1 · 定位

控制台是自愈系统的**地面站**：让无人值守可信（可见）、在系统承认需要人时接手（处置）、边界配置集中（治理）。

不做：PR inline 审查与合并（闸门在 GitHub，单一事实源）；多用户/RBAC；指标采集与告警路由。

## 2 · 用户与三个打开时刻

唯一用户（所有者）。例行巡视 → 总览页；事件响应 → 需处理队列 + 详情页；审计回溯 → 详情页时间线。

## 3 · 信息架构

```
总览页 /ui
  ├─ 状态机漏斗（7 态计数，点击过滤；new/triggered/fixing/pr_open/merged/rejected/escalated）
  ├─ ① 需要你处理（escalated + rejected 卡片，一键 reopen）
  ├─ ② 全量表格（五色石等级 / 相对时间 / PR 链接 / repo 筛选 chips）
  ├─ ③ 活动流（events 最近 12 条）
  └─ 底栏：sync 对账 + 合并闸门提示
详情页 /issue/{fp}
  ├─ 头部（指纹/标题/等级石/状态/次数/首现末现）
  ├─ 生命周期时间线（该指纹的 events）
  ├─ 堆栈样本（pre）
  ├─ PR 卡（链接）+ 修复会话路径
  └─ 处置：escalated/rejected → reopen
设置页 /settings
  ├─ ① 凭据（GitHub PAT / 智谱 Key / 上报令牌 / UI 令牌）— 掩码，留空保持
  ├─ ② LLM 修复模型（provider / base_url / model）
  ├─ ③ Runner 参数（interval / ci_timeout / daily_pr_limit / max_turns / max_tokens）
  └─ ④ 服务控制（重启 serve+work）
```

## 4 · 数据与接口缺口补齐

| # | 缺口 | 补法 |
|---|---|---|
| G1 | CI 结论未入库 | ci gate 结论写 events（ci_green / ci_red / ci_timeout，detail 含 head sha） |
| G2 | 升级原因非结构化 | escalated detail 规范化前缀：`repair_failed` / `delivery_failed` / `ci_red` |
| G3 | 合并事件缺失 | sync/对账 merged 时写 kind=merged（detail=PR url） |
| G4 | work 无心跳 | `heartbeat` 单行表，work 每轮循环 upsert；总览显示"最近心跳 N 秒前" |
| G5 | 配置变更无审计 | settings 保存对每个变更 key 写 config_changed 事件（只记 key 不记值） |

## 5 · 设置页存储模型（A）

`/opt/nuwa/nuwa.env`（systemd EnvironmentFile）为唯一事实源。设置页 = 该文件的分区掩码编辑器：GET 渲染现值（密钥掩码，留空保持）；POST 校验（数值/白名单）→ 写回（chmod 600）→ 对变更 key 写 config_changed 事件 → 提示"重启服务后生效"→ ④ 服务控制一键重启（`systemd-run --on-active=2` 编排，serve 响应后触发）。

env schema：`NUWA_GITHUB_TOKEN`、`ZAI_API_KEY`、`NUWA_INGEST_TOKEN`、`NUWA_UI_TOKEN`、`NUWA_PROVIDER`（zai|anthropic|faux）、`NUWA_MODEL`、`NUWA_BASE_URL`、`NUWA_INTERVAL`、`NUWA_CI_TIMEOUT`、`NUWA_DAILY_PR_LIMIT`、`NUWA_MAX_TURNS`、`NUWA_MAX_TOKENS`。work/serve 启动时读取；CLI 显式 flag 优先于 env。

## 6 · 鉴权

`/ui*`、`/settings*`、`/issue/*`：Basic，密码 = `NUWA_UI_TOKEN`（未设则 fallback `NUWA_INGEST_TOKEN`）。`/ingest`：`X-NUWA-Token` = `NUWA_INGEST_TOKEN`。UI 令牌与上报令牌分离（控制台可改密钥，采集令牌只上报）。

## 7 · 视觉

深色 GitHub-dark 调；状态机漏斗为骨架；**五色石**=严重度徽章（critical 红 / high 橙 / medium 金 / low 绿）；状态胶囊色：new 绿 / fixing 黄 / pr_open 蓝 / merged 紫 / rejected·escalated 红；相对时间（刚刚/N 分钟前/N 小时前/N 天前）；零 JS 框架。

## 8 · 交付

阶段 1（本次）：总览增强 + 详情页 + 设置页 + G1-G5 + D2。阶段 2（P1）：GitHub OAuth、趋势图、会话转录在线查看、repo 在线注册。
