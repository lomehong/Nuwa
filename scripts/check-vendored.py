#!/usr/bin/env python3
"""vendored 补丁清单检查。

`crates/rpi-*` 相对 vendored 基线 tag 的每一处改动，都必须登记在
docs/vendored-patches.json 里（路径 + 原因）。清单反过来也必须仍然对应真实
改动——补丁被上游吸收或撤回后，要同步删掉条目。

CI 需要完整历史（actions/checkout 的 fetch-depth: 0）。
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "docs" / "vendored-patches.json"


def git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True
    )
    if result.returncode != 0:
        raise SystemExit(f"git {' '.join(args)} 失败：{result.stderr.strip()}")
    return result.stdout


def norm(path: str) -> str:
    return path.replace("\\", "/").strip()


def main() -> int:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    baseline = manifest["baseline_commit"]
    if subprocess.run(
        ["git", "cat-file", "-e", f"{baseline}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
    ).returncode != 0:
        raise SystemExit(
            f"找不到 vendored 基线提交 {baseline}；"
            "CI 的 actions/checkout 需要 fetch-depth: 0"
        )

    declared = {norm(entry["path"]): entry for entry in manifest["files"]}
    for path, entry in declared.items():
        if not entry.get("reason", "").strip():
            raise SystemExit(f"{path}: 补丁清单必须写明原因")

    changed = {
        norm(path)
        for path in git("diff", "--name-only", baseline, "--", "crates/rpi-*").splitlines()
        if path.strip()
    }
    changed |= {
        norm(path)
        for path in git(
            "ls-files", "--others", "--exclude-standard", "--", "crates/rpi-*"
        ).splitlines()
        if path.strip()
    }

    undeclared = sorted(changed - set(declared))
    stale = sorted(set(declared) - changed)

    if undeclared:
        print("vendored 改动未登记（补丁清单必须同步）：")
        for path in undeclared:
            print(f"  + {path}")
    if stale:
        print("补丁清单条目已不存在于 diff（补丁被吸收/撤回？）：")
        for path in stale:
            print(f"  - {path}")
    if undeclared or stale:
        return 1

    print(f"vendored 补丁清单检查通过：{len(changed)} 个文件，基线 {baseline[:12]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
