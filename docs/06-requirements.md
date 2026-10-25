# 06 · 约束与要求

状态：已确认（2026-10-15）

## 硬约束

1. **目标仓库全部为所有者私有仓库**，源码对服务端可达；服务端统一持有最小权限 deploy key
2. **修复在服务端完成**；现场（生产环境）可能资源有限、不装开发运行时，现场组件只做采集/上报/执行预定义验证脚本
3. **直接开 PR**，无 draft 中间态；CI 通过 + 所有者人工合并是唯一合入路径
4. **触发前必须聚合**为问题单：同指纹去重、冷却窗口，不允许逐异常触发

## 语言无关性的边界（已核实）

- **agent 本体（rpi 裁剪版）零语言假设**：工具面为纯文件/文本/shell 操作（read/write/edit/bash/powershell/grep/find/ls），已 grep 核实 rpi-agent / rpi-harness / rpi-tools 中无任何 cargo/rustc/maven/gradle 语言假设；edit 工具为精确文本替换 + 模糊回退
- 可修 Go / Java / Rust 等任何语言的仓库

语言差异由外围承担（"三处语言相关位"）：

| 位置 | 职责 | 归属 |
|---|---|---|
| 采集层 | 按语言写事件采集 SDK 与堆栈指纹 parser（Go goroutine trace / Java 异常链 / Rust panic 格式互不相同） | 青鸟，按需逐语言增 |
| 运行器 | 按技术栈备工具链（JDK+Maven/Gradle、Go、cargo…），保证"复现验证"可执行 | 应龙 runner 镜像 |
| 仓库指令文件 | 每个目标仓库自教构建/测试命令、代码规范、禁改路径（AGENTS.md 类 bootstrap 机制） | 每个接入仓库 |

## 对每个接入系统的接入清单

1. 安装对应语言采集 SDK（青鸟），接好错误/panic/日志采集点
2. 目标仓库放入指令文件（构建命令、测试命令、禁改路径、规范）
3. 确认存在能跑该项目测试的 runner（含工具链与依赖，如可离线拉取依赖则更佳）
4. 在问题单库登记仓库与 deploy key 映射

## 质量与安全要求

- 保留 crates 必须补 `[lints]` 工作区配置，clippy 进 CI（上游零 lint 配置，生产 unwrap ≈730 处——嵌入前削减）
- vendor/锁定 pi-rust 衍生代码版本；上游（≈0.85 版/天）只做选择性 cherry-pick
- 上下文包与上报日志脱敏：密钥、敏感数据不进 LLM 上下文（2026-10-20 落地：work 启动清秘 + stack/title/prior context 已知 secret/常见 token pattern redaction；owner-authored `notes` 仍是自由文本）
- 服务端 LLM key 只存在于运行器池一处；现场组件不含任何密钥

## 需求标识索引

- `REQ-001`：目标仓库均为所有者私有仓库，源码对服务端可达。
- `REQ-002`：修复在服务端完成，现场只采集、上报和执行预定义验证脚本。
- `REQ-003`：直接开 PR，CI 通过和所有者人工合并是唯一合入路径。
- `REQ-004`：触发前按指纹聚合并去重，遵守冷却窗口。
- `NFR-001`：保留 crate 必须有工作区 lints，Clippy 进入 CI。
- `NFR-002`：vendored 版本锁定，本地补丁必须登记并接受上游对账。
- `NFR-003`：上下文、日志、证据和转录必须脱敏。
- `NFR-004`：LLM key 集中在 runner，现场组件不得持有密钥。
