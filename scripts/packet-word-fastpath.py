#!/usr/bin/env python3
from pathlib import Path
import re


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if old not in source:
        raise SystemExit(f"expected block not found: {label}")
    return source.replace(old, new, 1)


def replace_regex(source: str, pattern: str, replacement: str, label: str) -> str:
    updated, count = re.subn(
        pattern,
        lambda _match: replacement,
        source,
        count=1,
        flags=re.DOTALL,
    )
    if count != 1:
        raise SystemExit(f"expected one regex match for {label}, got {count}")
    return updated


packet_path = Path("src/packet/mod.rs")
source = packet_path.read_text()
source = replace_regex(
    source,
    r"fn packet_shader_source\(\) -> Result<String> \{.*?\n\}\n\nfn write_u32_array",
    r'''fn packet_shader_source() -> Result<String> {
    let mut response_bytes = Vec::new();
    let mut response_word_offsets = Vec::with_capacity(PACKET_RESPONSES.len());
    let mut response_word_counts = Vec::with_capacity(PACKET_RESPONSES.len());
    let mut response_lengths = Vec::with_capacity(PACKET_RESPONSES.len());
    let mut response_checksum_sums = Vec::with_capacity(PACKET_RESPONSES.len());

    for response in PACKET_RESPONSES {
        while !response_bytes.len().is_multiple_of(4) {
            response_bytes.push(0);
        }
        response_word_offsets.push((response_bytes.len() / 4) as u32);
        response_word_counts.push(response.len().div_ceil(4) as u32);
        response_lengths.push(response.len() as u32);
        response_checksum_sums.push(internet_checksum_sum(response));
        response_bytes.extend_from_slice(response);
    }

    let words = pack_bytes(&response_bytes);
    let mut source = String::new();
    writeln!(source, "const FLOW_WORDS: u32 = {FLOW_WORDS}u;")?;
    writeln!(
        source,
        "const RESPONSE_COUNT: u32 = {}u;",
        PACKET_RESPONSES.len()
    )?;
    writeln!(
        source,
        "const RESPONSE_PLAINTEXT: u32 = {RESPONSE_PLAINTEXT}u;"
    )?;
    writeln!(source, "const RESPONSE_HEALTH: u32 = {RESPONSE_HEALTH}u;")?;
    writeln!(
        source,
        "const RESPONSE_BAD_REQUEST: u32 = {RESPONSE_BAD_REQUEST}u;"
    )?;
    writeln!(
        source,
        "const RESPONSE_NOT_FOUND: u32 = {RESPONSE_NOT_FOUND}u;"
    )?;
    writeln!(
        source,
        "const RESPONSE_METHOD_NOT_ALLOWED: u32 = {RESPONSE_METHOD_NOT_ALLOWED}u;"
    )?;
    write_u32_array(
        &mut source,
        "RESPONSE_WORD_OFFSETS",
        &response_word_offsets,
    )?;
    write_u32_array(&mut source, "RESPONSE_WORD_COUNTS", &response_word_counts)?;
    write_u32_array(&mut source, "RESPONSE_LENGTHS", &response_lengths)?;
    write_u32_array(
        &mut source,
        "RESPONSE_CHECKSUM_SUMS",
        &response_checksum_sums,
    )?;
    writeln!(
        source,
        "const RESPONSE_WORDS: array<u32, {}> = array<u32, {}>(",
        words.len(),
        words.len()
    )?;
    for chunk in words.chunks(8) {
        for word in chunk {
            write!(source, "0x{word:08x}u,")?;
        }
        source.push('\n');
    }
    source.push_str(");\n");
    source.push_str(include_str!("packet.wgsl"));
    Ok(source)
}

fn internet_checksum_sum(bytes: &[u8]) -> u32 {
    bytes
        .chunks(2)
        .map(|chunk| {
            let high = u16::from(chunk[0]) << 8;
            let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
            u32::from(high | low)
        })
        .sum()
}

fn write_u32_array''',
    "word-aligned response generator",
)
source = replace_once(
    source,
    '''    #[test]
    fn response_slots_are_tight_instead_of_mtu_sized_furniture_vans() {
''',
    '''    #[test]
    fn precomputed_response_checksum_sums_match_the_wire_words() {
        for response in PACKET_RESPONSES {
            let expected = response.chunks(2).fold(0_u32, |sum, chunk| {
                let high = u16::from(chunk[0]) << 8;
                let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
                sum + u32::from(high | low)
            });
            assert_eq!(internet_checksum_sum(response), expected);
        }
    }

    #[test]
    fn response_slots_are_tight_instead_of_mtu_sized_furniture_vans() {
''',
    "checksum sum unit test",
)
packet_path.write_text(source)

shader_path = Path("src/packet/packet.wgsl")
source = shader_path.read_text()
source = replace_regex(
    source,
    r"fn tcp_checksum\(packet_index: u32, tcp_len: u32\) -> u32 \{.*?\n\}\n\nfn clear_output\(packet_index: u32, len: u32\) \{.*?\n\}",
    '''fn tcp_checksum(packet_index: u32, tcp_len: u32, response_id: u32) -> u32 {
    var sum = 0u;
    sum += read_output_u16_be(packet_index, 12u);
    sum += read_output_u16_be(packet_index, 14u);
    sum += read_output_u16_be(packet_index, 16u);
    sum += read_output_u16_be(packet_index, 18u);
    sum += 6u;
    sum += tcp_len;

    var offset = 20u;
    loop {
        if offset >= 40u {
            break;
        }
        if offset != 36u {
            sum += read_output_u16_be(packet_index, offset);
        }
        offset += 2u;
    }
    if response_id < RESPONSE_COUNT {
        sum += RESPONSE_CHECKSUM_SUMS[response_id];
    }
    return checksum_fold(sum);
}

fn clear_output_header(packet_index: u32) {
    let base = packet_index * params.output_stride_words;
    var word_index = 0u;
    loop {
        if word_index >= 10u {
            break;
        }
        output_words[base + word_index] = 0u;
        word_index += 1u;
    }
}''',
    "constant-time TCP payload checksum",
)
source = replace_once(
    source,
    '''fn write_ipv4(packet_index: u32, len: u32, src_ip: u32, dst_ip: u32) {
    clear_output(packet_index, len);
''',
    '''fn write_ipv4(packet_index: u32, len: u32, src_ip: u32, dst_ip: u32) {
    clear_output_header(packet_index);
''',
    "header-only clear",
)
source = replace_regex(
    source,
    r"fn write_checksums\(packet_index: u32, tcp_len: u32\) \{.*?\n\}\n\nfn write_http_response\(packet_index: u32, response_id: u32\) \{.*?\n\}",
    '''fn write_checksums(packet_index: u32, tcp_len: u32, response_id: u32) {
    write_u16_be(packet_index, 10u, ipv4_checksum(packet_index));
    write_u16_be(
        packet_index,
        36u,
        tcp_checksum(packet_index, tcp_len, response_id),
    );
}

fn write_http_response(packet_index: u32, response_id: u32) {
    let source_base = RESPONSE_WORD_OFFSETS[response_id];
    let output_base = packet_index * params.output_stride_words + 10u;
    let word_count = RESPONSE_WORD_COUNTS[response_id];
    var word_index = 0u;
    loop {
        if word_index >= word_count {
            break;
        }
        output_words[output_base + word_index] =
            RESPONSE_WORDS[source_base + word_index];
        word_index += 1u;
    }
}''',
    "word-copy response writer",
)
source = replace_once(
    source,
    "    write_checksums(packet_index, 20u + payload_len);",
    "    write_checksums(packet_index, 20u + payload_len, response_id);",
    "response-aware checksum call",
)
shader_path.write_text(source)

readme_path = Path("README.md")
source = readme_path.read_text()
source = replace_once(
    source,
    "- GPU-generated IPv4 and TCP checksums\n",
    "- GPU-generated IPv4 and TCP checksums, with precomputed static payload sums\n- word-packed HTTP response templates copied four bytes at a time instead of knitted byte by byte\n",
    "README packet fast path bullets",
)
readme_path.write_text(source)
