#!/usr/bin/env python3
from pathlib import Path

path = Path("scripts/speed-polish.py")
source = path.read_text()
old = '''def replace(source: str, old: str, new: str, label: str) -> str:
    old = dedent(old)
    new = dedent(new)
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)
'''
new = '''def normalize_block(block: str) -> str:
    lines = block.lstrip("\\n").splitlines()
    normalized = "\\n".join(
        line[4:] if line.startswith("    ") else line for line in lines
    )
    return normalized + "\\n"


def replace(source: str, old: str, new: str, label: str) -> str:
    old = normalize_block(old)
    new = normalize_block(new)
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)
'''
if old not in source:
    raise SystemExit("speed-polish replace helper did not match")
source = source.replace(old, new, 1)
exec(compile(source, str(path), "exec"))
