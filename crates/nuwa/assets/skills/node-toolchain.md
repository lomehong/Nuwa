---
name: node-toolchain
version: 1
description: Node/TypeScript 构建与测试失败：先区分类型、断言、解析与依赖问题
languages: javascript, typescript, node
matches: npm, pnpm, yarn, node, vitest, jest, tsc
---

# Node 修复路径

1. 读第一条真正失败的日志，先分类：类型/语法错误（tsc）、测试断言、模块解析（Cannot find module）、安装/原生依赖完整性。
2. 类型错误：定位声明与使用处，优先修实现满足既有类型契约；不用 `any`、`as` 或 `@ts-expect-error` 压掉错误。
3. 模块解析：确认 import 路径大小写、扩展名与 package.json 的 exports；不要靠新增 tsconfig 路径别名绕过。
4. 测试失败：读断言两侧的实际值，判断是产品行为错误还是测试假设过期；修根因，不改测试去迁就错误实现。
5. 依赖问题：不要手写 lockfile；根因确实在依赖版本时，只改 package.json 中必要的一条并让包管理器重新生成。lockfile 若在禁改清单里，就用现有依赖完成修复。
6. 验证：用仓库声明的包管理器重跑失败命令；结束时给出 ROOT CAUSE / CHANGE / VERIFICATION。
