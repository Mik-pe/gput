#!/usr/bin/env python3
from pathlib import Path

path = Path("scripts/speed-polish.py")
source = path.read_text()
old = """    old = dedent(old)\n    new = dedent(new)\n"""
new = """    old = dedent(old).lstrip(\"\\n\")\n    new = dedent(new).lstrip(\"\\n\")\n"""
if old not in source:
    raise SystemExit("speed-polish replace helper did not match")
source = source.replace(old, new, 1)
exec(compile(source, str(path), "exec"))
