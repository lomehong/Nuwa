"""Owner dependency closure: this entrypoint and owner_expected.json only.

Exit 1 is an expected assertion/known empty-input bug; faults exit 2.
Candidate code is a subprocess, never imported into the owner process.
"""
import json
import os
from pathlib import Path
import subprocess
import sys


def check():
    if len(sys.argv) != 1:
        return 2
    root = Path(os.environ["NUWA_VERIFY_ROOT"])
    cases = json.loads(Path(__file__).with_name("owner_expected.json").read_text())
    failed = False
    for values, expected in cases:
        result = subprocess.run(
            [sys.executable, "-I", "-B", str(root / "calc.py")],
            input=json.dumps(values), text=True, capture_output=True,
            cwd=root, timeout=5,
        )
        if result.returncode:
            known_bug = (
                not values and result.returncode == 1
                and result.stderr.rstrip().endswith("ZeroDivisionError: division by zero")
            )
            if not known_bug:
                return 2
            failed = True
            continue
        try:
            actual = json.loads(result.stdout)
        except (ValueError, TypeError):
            return 2
        if isinstance(actual, bool) or not isinstance(actual, (float, int)):
            failed = True
        elif actual != expected:
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        code = check()
    except Exception:
        code = 2
    raise SystemExit(code)