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
    while lines and not lines[-1].strip():
        lines.pop()
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
ci_marker = '\nci_path = ".github/workflows/ci.yml"\n'
if ci_marker not in source:
    raise SystemExit("speed-polish CI tail marker did not match")
source = source.split(ci_marker, 1)[0] + "\n"
exec(compile(source, str(path), "exec"))

batcher = Path("src/batcher.rs")
batcher_source = batcher.read_text()
old_order = """        jobs.clear();
        requests.clear();
        jobs.push(first);
"""
new_order = """        requests.clear();
        jobs.clear();
        jobs.push(first);
"""
if old_order not in batcher_source:
    raise SystemExit("batcher reset order did not match")
batcher_source = batcher_source.replace(old_order, new_order, 1)
old_call = """        requests.extend(jobs.iter().map(|job| job.request.as_slice()));
        let result = processor.process_batch(&requests);
"""
new_call = """        requests.extend(jobs.iter().map(|job| job.request.as_slice()));
        let result = processor.process_batch(&requests);
        requests.clear();
"""
if old_call not in batcher_source:
    raise SystemExit("batcher processor call did not match")
batcher.write_text(batcher_source.replace(old_call, new_call, 1))
