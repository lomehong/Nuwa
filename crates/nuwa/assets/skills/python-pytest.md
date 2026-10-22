---
name: python-pytest
version: 1
description: Python/pytest 失败：区分导入、断言、fixture 与类型/lint 问题
languages: python
matches: pytest, python -m, tox, mypy, ruff
---

# Python 修复路径

1. 读 traceback 最底部：异常类型与抛出点；再向上找本仓库的第一帧。
2. ImportError/ModuleNotFoundError：确认包结构与导入路径，不要往 sys.path 上打补丁。
3. 断言失败：对比实际/期望值，判断根因在实现还是测试假设。
4. fixture/环境问题：检查 fixture 作用域与外部依赖；环境缺失要明确记录，不要跳过测试。
5. 类型/lint 失败：修实现以满足契约，不用 `# type: ignore` / `# noqa` 压掉。
6. 验证：重跑同一条失败命令；结束时给出 ROOT CAUSE / CHANGE / VERIFICATION。
