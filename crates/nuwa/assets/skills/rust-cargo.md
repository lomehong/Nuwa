---
name: rust-cargo
version: 1
description: Rust 编译/测试失败：从第一条错误定位最小修复
languages: rust
matches: cargo, rustc
---

# Rust 修复路径

1. 先复现：运行失败命令，找到第一条 `error[...]` 或 `panicked at`，不要从 warning 或测试摘要开始猜。
2. 顺着错误给出的文件、函数、行号，读完整函数与相邻类型定义，确认 API 契约后再动手。
3. 常见根因顺序：类型/借用不匹配 → 缺失或错误的 feature/依赖 → 公开 API 变更 → 测试假设过期。
4. 保持最小正确改动：
   - 不为了迁就调用点而改公共签名，除非根因就在签名；
   - 不用 `#[allow(...)]`、`unwrap`、`dbg!` 掩盖问题；
   - 不重排 import 或格式化无关模块。
5. 验证：重跑同一条失败命令；仓库若声明了 clippy/格式化门，再跑与改动相关的最小集合。
6. 结束时给出 ROOT CAUSE / CHANGE / VERIFICATION 三行，VERIFICATION 写实际命令与结果。
