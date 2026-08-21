#!/usr/bin/env python3
from pathlib import Path

path = Path("scripts/tight-readback.py")
source = path.read_text()
old = '''        let meta_copy_bytes = (packet_count * std::mem::size_of::<PacketMeta>()) as u64;
        let word_copy_bytes =
            (packet_count * INPUT_PACKET_WORDS * std::mem::size_of::<u32>()) as u64;
'''
new = '''        let meta_copy_bytes = (packet_count * std::mem::size_of::<PacketMeta>()) as u64;
        let word_copy_bytes = (packet_count * INPUT_PACKET_WORDS * std::mem::size_of::<u32>()) as u64;
'''
if old not in source:
    raise SystemExit("tight-readback copy-size template did not match")
source = source.replace(old, new, 1)
exec(compile(source, str(path), "exec"))
