"""Print manifest digest fields; no commands, repairs, tests or writes.

Run with Python from any cwd, then copy these fields into batch4-manifest.json.
batch4/.gitattributes pins fixture bytes independently of checkout autocrlf.
"""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parent
files = sorted((root / "batch4").rglob("*"))
hashes = {
    path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
    for path in files if path.is_file()
}
oracle = {
    path: digest for path, digest in hashes.items()
    if path in ("batch4/repo/owner_oracle.py", "batch4/repo/owner_expected.json")
}
def aggregate(entries):
    return hashlib.sha256("".join(
        f"{path}\0{digest}\n" for path, digest in sorted(entries.items())
    ).encode()).hexdigest()

print(json.dumps({
    "fixture_sha256": {"files": hashes, "digest": aggregate(hashes)},
    "oracle_sha256": {"files": oracle, "digest": aggregate(oracle)},
}, indent=2))