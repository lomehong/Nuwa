#!/usr/bin/env python3
"""架构契约检查：crate 依赖方向 + nuwa 模块分层。

规则写死在这里，CI 强制执行；不允许“先合并再补”。已知的向上依赖登记在
DEBT 里，必须带原因——只减不增。
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 低层 crate 不允许依赖高层 crate（依赖只能向下）。
CRATE_RULES: dict[str, set[str]] = {
    "shanhaijing": {"nuwa", "rpi-telemetry", "rpi-ai", "rpi-agent", "rpi-tools", "rpi-harness"},
    "rpi-telemetry": {"nuwa", "shanhaijing", "rpi-ai", "rpi-agent", "rpi-tools", "rpi-harness"},
    "rpi-ai": {"nuwa", "shanhaijing"},
    "rpi-agent": {"nuwa", "shanhaijing"},
    "rpi-tools": {"nuwa", "shanhaijing"},
    "rpi-harness": {"nuwa", "shanhaijing"},
}

# nuwa 内部模块的禁止边：修复/交付内核不得反向依赖 HTTP 宿主或表面层。
MODULE_RULES: dict[str, set[str]] = {
    "api": {"deliver", "repair", "guard", "verify", "instructions", "check", "git_ops"},
    "web": {"deliver", "repair", "guard", "verify", "instructions", "check", "git_ops"},
    "repair": {"api", "serve", "deliver", "verify", "web", "check", "session", "settings"},
    "verify": {"api", "serve", "deliver", "repair", "web", "check", "session", "settings"},
    "deliver": {"api", "serve", "web", "check", "session", "settings"},
    "git_ops": {
        "api", "serve", "deliver", "repair", "verify", "guard", "secrets", "issue",
        "instructions", "check", "web", "session", "settings",
    },
    "secrets": {
        "api", "serve", "deliver", "repair", "verify", "guard", "issue",
        "instructions", "check", "web", "session", "settings",
    },
    "issue": {
        "api", "serve", "deliver", "repair", "verify", "guard", "git_ops", "secrets",
        "instructions", "check", "web", "session", "settings",
    },
    "instructions": {
        "api", "serve", "deliver", "repair", "verify", "guard", "check", "web",
        "session", "settings",
    },
    "webhook": {
        "api", "serve", "deliver", "repair", "verify", "guard", "instructions",
        "check", "web", "session", "settings",
    },
    "check": {"api", "serve", "web"},
    "pipeline": {"api", "serve", "web", "check"},
    "skills": {
        "api", "serve", "deliver", "repair", "verify", "guard", "instructions",
        "check", "web", "session", "settings",
    },
    "trace": {"api", "serve", "web"},
    "session": {"api", "serve"},
    "settings": {
        "api", "serve", "deliver", "repair", "verify", "guard", "instructions",
        "check", "web", "session",
    },
}

# 已存在的向上依赖：登记为债，新增即失败。只允许删除，不允许新增。
DEBT: dict[tuple[str, str], str] = {
    ("session", "api"): "session.rs 混入 HTTP 中间件（ApiError 响应）；中间件迁出后删除",
    ("session", "serve"): "session.rs 混入 HTTP 中间件（AppState）；中间件迁出后删除",
}

MODULES = set(MODULE_RULES) | {"main", "serve"}


def cargo_workspace_deps() -> dict[str, set[str]]:
    output = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    metadata = json.loads(output)
    workspace = {package["name"] for package in metadata["packages"]}
    deps: dict[str, set[str]] = {}
    for package in metadata["packages"]:
        if package["name"] not in workspace:
            continue
        deps[package["name"]] = {
            dep["name"] for dep in package["dependencies"] if dep["name"] in workspace
        }
    return deps


def module_files(module: str) -> list[Path]:
    base = ROOT / "crates" / "nuwa" / "src"
    if module == "api":
        return sorted((base / "api").rglob("*.rs"))
    if module == "main":
        return [base / "main.rs"]
    direct = base / f"{module}.rs"
    nested = base / module
    files = [direct] if direct.is_file() else []
    if nested.is_dir():
        files.extend(sorted(nested.rglob("*.rs")))
    return files


def code_lines(path: Path):
    for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        stripped = line.lstrip()
        if stripped.startswith(("//", "///", "//!", "#")):
            continue
        # 去掉行尾注释；字符串里出现 `//` 的概率极低，且只会漏报不会误报。
        code = line.split("//", 1)[0]
        yield number, code


def main() -> int:
    violations: list[str] = []

    deps = cargo_workspace_deps()
    for crate, forbidden in CRATE_RULES.items():
        for dep in sorted(deps.get(crate, set()) & forbidden):
            violations.append(f"crate {crate} 不得依赖 {dep}")

    for module, forbidden in MODULE_RULES.items():
        for path in module_files(module):
            for number, code in code_lines(path):
                for target in re.findall(r"\bcrate::([a-z_]+)\b", code):
                    if target not in forbidden:
                        continue
                    if (module, target) in DEBT:
                        continue
                    violations.append(
                        f"{path.relative_to(ROOT)}:{number}: {module} 不得引用 crate::{target}"
                    )

    for path in sorted((ROOT / "crates").glob("rpi-*/src/**/*.rs")):
        for number, code in code_lines(path):
            for upward in ("nuwa", "shanhaijing"):
                if re.search(rf"\b{upward}::", code):
                    violations.append(
                        f"{path.relative_to(ROOT)}:{number}: vendored crate 不得引用 {upward}"
                    )

    if violations:
        print("架构契约检查失败：")
        for violation in violations:
            print(f"  - {violation}")
        return 1
    print(f"架构契约检查通过（{len(MODULE_RULES)} 个模块，{len(CRATE_RULES)} 个 crate，{len(DEBT)} 条已登记债）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
