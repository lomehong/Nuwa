# Deliberately base-green: only the frozen owner oracle proves the runtime bug.
import ast
from pathlib import Path

ast.parse(Path("calc.py").read_text(encoding="utf-8"))