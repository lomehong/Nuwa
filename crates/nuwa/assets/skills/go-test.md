---
name: go-test
version: 1
description: Go 测试/构建失败：区分编译错误、断言失败与超时
languages: go
matches: go test, go build, go vet
---

# Go 修复路径

1. 读第一条 `--- FAIL` 或编译错误：文件:行号、实际值与期望值。
2. 编译错误优先：未定义符号、签名不匹配、import 循环。不要用空实现或 `_ = x` 掩盖。
3. 测试失败：确认实现行为与测试断言谁错了。修实现；只有测试明显落后于当前契约（有代码证据）时才改测试。
4. 并发/超时失败：检查 goroutine 泄漏、未关闭的 response body、未传递的 context；不要靠调大超时掩盖。
5. 验证：先跑失败包，再跑相关包；结束时给出 ROOT CAUSE / CHANGE / VERIFICATION。
