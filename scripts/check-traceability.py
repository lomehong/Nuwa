#!/usr/bin/env python3
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
MATRIX = DOCS / "10-traceability.md"
ID = re.compile(r"^(?:REQ|NFR|DES)-\d{3}$")
ADR = re.compile(r"^## ADR-(\d{3})")
ROW = re.compile(r"^\|\s*((?:REQ|NFR|DES)-\d{3})\s*\|")


def fail(message: str) -> None:
    print(f"traceability: {message}", file=sys.stderr)
    raise SystemExit(1)


def identifiers(path: Path, prefixes: tuple[str, ...]) -> set[str]:
    if not path.exists():
        return set()
    found = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        for prefix in prefixes:
            found.update(re.findall(rf"{prefix}-\d{{3}}", line))
    return found


def main() -> None:
    if not MATRIX.exists():
        fail("docs/10-traceability.md is missing")
    adrs = set()
    for line in (DOCS / "04-decisions.md").read_text(encoding="utf-8").splitlines():
        match = ADR.match(line)
        if match:
            adrs.add(f"ADR-{match.group(1)}")

    requirements: list[str] = []
    designs: list[str] = []
    for line in MATRIX.read_text(encoding="utf-8").splitlines():
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) != 4 or not re.fullmatch(
            r"(?:REQ|NFR)-\d{3}(?:, (?:REQ|NFR)-\d{3})*", cells[0]
        ):
            continue
        design = re.fullmatch(r"(DES-\d{3}) / (ADR-\d{3})", cells[1])
        if not design:
            fail(f"row has no valid design/ADR pair: {line}")
        if design.group(1)[4:] != design.group(2)[4:]:
            fail(f"design/ADR numbering differs: {line}")
        if not cells[2] or not cells[3]:
            fail(f"row lacks implementation or verification evidence: {cells[0]} {cells[1]}")
        requirements.extend(cells[0].split(", "))
        designs.append(design.group(1))

    if not requirements:
        fail("matrix has no requirement rows")
    for value in requirements + designs:
        if not ID.fullmatch(value):
            fail(f"invalid ID: {value}")
    duplicate_designs = [item for item, count in Counter(designs).items() if count != 1]
    if duplicate_designs:
        fail(f"design IDs must occur exactly once: {duplicate_designs}")

    documented = identifiers(DOCS / "01-vision.md", ("REQ", "NFR"))
    documented |= identifiers(DOCS / "06-requirements.md", ("REQ", "NFR"))
    matrix_requirements = set(requirements)
    if documented != matrix_requirements:
        fail(
            f"requirement IDs differ: only docs={sorted(documented - matrix_requirements)}, "
            f"only matrix={sorted(matrix_requirements - documented)}"
        )
    expected_designs = {f"DES-{adr[4:]}" for adr in adrs}
    if set(designs) != expected_designs:
        fail(
            f"ADR/design mismatch: missing={sorted(expected_designs - set(designs))}, "
            f"orphan={sorted(set(designs) - expected_designs)}"
        )
    print(
        f"traceability: {len(matrix_requirements)} requirements, "
        f"{len(designs)} designs, 0 uncovered, 0 orphan, 0 numbering errors"
    )


if __name__ == "__main__":
    main()
