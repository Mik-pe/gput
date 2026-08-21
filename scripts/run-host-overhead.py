#!/usr/bin/env python3
from pathlib import Path

implementation = Path("scripts/host-overhead.py")
source = implementation.read_text()
exec(compile(source, str(implementation), "exec"))

packet = Path("src/packet/mod.rs")
source = packet.read_text()
old = """            std::mem::swap(&mut scratch.pending, &mut scratch.deferred);
"""
new = """            {
                let PacketScratch {
                    pending, deferred, ..
                } = &mut *scratch;
                std::mem::swap(pending, deferred);
            }
"""
if old not in source:
    raise SystemExit("scheduler swap did not match generated source")
packet.write_text(source.replace(old, new, 1))
