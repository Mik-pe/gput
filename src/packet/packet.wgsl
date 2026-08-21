struct PacketMeta {
    len: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

struct EngineParams {
    packet_count: u32,
    packet_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
}

@group(0) @binding(0) var<storage, read> input_meta: array<PacketMeta>;
@group(0) @binding(1) var<storage, read> input_words: array<u32>;
@group(0) @binding(2) var<storage, read_write> output_meta: array<PacketMeta>;
@group(0) @binding(3) var<storage, read_write> output_words: array<u32>;
@group(0) @binding(4) var<storage, read_write> flows: array<u32>;
@group(0) @binding(5) var<uniform> params: EngineParams;

const FLOW_WORDS: u32 = 8u;
const TCP_FIN: u32 = 0x01u;
const TCP_SYN: u32 = 0x02u;
const TCP_PSH: u32 = 0x08u;
const TCP_ACK: u32 = 0x10u;
const STATE_SYN_RECEIVED: u32 = 1u;
const STATE_ESTABLISHED: u32 = 2u;

fn read_byte(packet_index: u32, byte_index: u32) -> u32 {
    let base = packet_index * params.packet_stride_words;
    let word = input_words[base + byte_index / 4u];
    return (word >> ((byte_index & 3u) * 8u)) & 0xffu;
}

fn read_output_byte(packet_index: u32, byte_index: u32) -> u32 {
    let base = packet_index * params.packet_stride_words;
    let word = output_words[base + byte_index / 4u];
    return (word >> ((byte_index & 3u) * 8u)) & 0xffu;
}

fn write_byte(packet_index: u32, byte_index: u32, value: u32) {
    let base = packet_index * params.packet_stride_words;
    let word_index = base + byte_index / 4u;
    let shift = (byte_index & 3u) * 8u;
    let mask = 0xffu << shift;
    output_words[word_index] =
        (output_words[word_index] & ~mask) | ((value & 0xffu) << shift);
}

fn read_u16_be(packet_index: u32, offset: u32) -> u32 {
    return (read_byte(packet_index, offset) << 8u) |
        read_byte(packet_index, offset + 1u);
}

fn read_u32_be(packet_index: u32, offset: u32) -> u32 {
    return (read_byte(packet_index, offset) << 24u) |
        (read_byte(packet_index, offset + 1u) << 16u) |
        (read_byte(packet_index, offset + 2u) << 8u) |
        read_byte(packet_index, offset + 3u);
}

fn read_output_u16_be(packet_index: u32, offset: u32) -> u32 {
    return (read_output_byte(packet_index, offset) << 8u) |
        read_output_byte(packet_index, offset + 1u);
}

fn write_u16_be(packet_index: u32, offset: u32, value: u32) {
    write_byte(packet_index, offset, value >> 8u);
    write_byte(packet_index, offset + 1u, value);
}

fn write_u32_be(packet_index: u32, offset: u32, value: u32) {
    write_byte(packet_index, offset, value >> 24u);
    write_byte(packet_index, offset + 1u, value >> 16u);
    write_byte(packet_index, offset + 2u, value >> 8u);
    write_byte(packet_index, offset + 3u, value);
}

fn flow_hash(src_ip: u32, dst_ip: u32, src_port: u32, dst_port: u32) -> u32 {
    var hash = 2166136261u;
    hash = (hash ^ src_ip) * 16777619u;
    hash = (hash ^ dst_ip) * 16777619u;
    hash = (hash ^ src_port) * 16777619u;
    hash = (hash ^ dst_port) * 16777619u;
    return hash;
}

fn checksum_fold(input_sum: u32) -> u32 {
    var sum = input_sum;
    sum = (sum & 0xffffu) + (sum >> 16u);
    sum = (sum & 0xffffu) + (sum >> 16u);
    return (~sum) & 0xffffu;
}

fn ipv4_checksum(packet_index: u32) -> u32 {
    var sum = 0u;
    var offset = 0u;
    loop {
        if offset >= 20u {
            break;
        }
        if offset != 10u {
            sum += read_output_u16_be(packet_index, offset);
        }
        offset += 2u;
    }
    return checksum_fold(sum);
}

fn tcp_checksum(packet_index: u32, tcp_len: u32) -> u32 {
    var sum = 0u;
    sum += read_output_u16_be(packet_index, 12u);
    sum += read_output_u16_be(packet_index, 14u);
    sum += read_output_u16_be(packet_index, 16u);
    sum += read_output_u16_be(packet_index, 18u);
    sum += 6u;
    sum += tcp_len;

    var offset = 20u;
    loop {
        if offset >= 20u + tcp_len {
            break;
        }
        if offset != 36u {
            var word = read_output_byte(packet_index, offset) << 8u;
            if offset + 1u < 20u + tcp_len {
                word |= read_output_byte(packet_index, offset + 1u);
            }
            sum += word;
        }
        offset += 2u;
    }
    return checksum_fold(sum);
}

fn clear_output(packet_index: u32, len: u32) {
    var index = 0u;
    loop {
        if index >= len {
            break;
        }
        write_byte(packet_index, index, 0u);
        index += 1u;
    }
}

fn write_ipv4(packet_index: u32, len: u32, src_ip: u32, dst_ip: u32) {
    clear_output(packet_index, len);
    write_byte(packet_index, 0u, 0x45u);
    write_u16_be(packet_index, 2u, len);
    write_u16_be(packet_index, 6u, 0x4000u);
    write_byte(packet_index, 8u, 64u);
    write_byte(packet_index, 9u, 6u);
    write_u32_be(packet_index, 12u, src_ip);
    write_u32_be(packet_index, 16u, dst_ip);
}

fn write_tcp(
    packet_index: u32,
    src_port: u32,
    dst_port: u32,
    seq: u32,
    ack: u32,
    flags: u32,
) {
    write_u16_be(packet_index, 20u, src_port);
    write_u16_be(packet_index, 22u, dst_port);
    write_u32_be(packet_index, 24u, seq);
    write_u32_be(packet_index, 28u, ack);
    write_byte(packet_index, 32u, 0x50u);
    write_byte(packet_index, 33u, flags);
    write_u16_be(packet_index, 34u, 65535u);
}

fn write_checksums(packet_index: u32, tcp_len: u32) {
    write_u16_be(packet_index, 10u, ipv4_checksum(packet_index));
    write_u16_be(packet_index, 36u, tcp_checksum(packet_index, tcp_len));
}

fn is_plaintext_get(packet_index: u32, payload_offset: u32, payload_len: u32) -> bool {
    if payload_len < 15u {
        return false;
    }
    let expected = array<u32, 15>(
        71u, 69u, 84u, 32u, 47u, 112u, 108u, 97u,
        105u, 110u, 116u, 101u, 120u, 116u, 32u,
    );
    var index = 0u;
    loop {
        if index >= 15u {
            break;
        }
        if read_byte(packet_index, payload_offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    return true;
}

fn write_http_response(packet_index: u32) {
    var index = 0u;
    loop {
        if index >= HTTP_RESPONSE_LEN {
            break;
        }
        let word = HTTP_RESPONSE_WORDS[index / 4u];
        write_byte(
            packet_index,
            40u + index,
            (word >> ((index & 3u) * 8u)) & 0xffu,
        );
        index += 1u;
    }
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let packet_index = gid.x;
    if packet_index >= params.packet_count {
        return;
    }
    output_meta[packet_index].len = 0u;

    let input_len = input_meta[packet_index].len;
    if input_len < 40u {
        return;
    }
    if read_byte(packet_index, 0u) != 0x45u || read_byte(packet_index, 9u) != 6u {
        return;
    }
    let total_len = read_u16_be(packet_index, 2u);
    if total_len > input_len || total_len < 40u {
        return;
    }

    let src_ip = read_u32_be(packet_index, 12u);
    let dst_ip = read_u32_be(packet_index, 16u);
    let src_port = read_u16_be(packet_index, 20u);
    let dst_port = read_u16_be(packet_index, 22u);
    if dst_port != params.listen_port {
        return;
    }

    let data_offset = (read_byte(packet_index, 32u) >> 4u) * 4u;
    if data_offset < 20u || 20u + data_offset > total_len {
        return;
    }
    let payload_offset = 20u + data_offset;
    let payload_len = total_len - payload_offset;
    let seq = read_u32_be(packet_index, 24u);
    let ack = read_u32_be(packet_index, 28u);
    let flags = read_byte(packet_index, 33u);
    let key_hash = flow_hash(src_ip, dst_ip, src_port, dst_port);
    let slot = key_hash & (params.flow_capacity - 1u);
    let base = slot * FLOW_WORDS;
    let state = flows[base];

    if (flags & TCP_SYN) != 0u && (flags & TCP_ACK) == 0u {
        let isn = key_hash ^ 0xa5a55a5au;
        flows[base] = STATE_SYN_RECEIVED;
        flows[base + 1u] = key_hash;
        flows[base + 2u] = seq + 1u;
        flows[base + 3u] = isn + 1u;

        write_ipv4(packet_index, 40u, dst_ip, src_ip);
        write_tcp(
            packet_index,
            dst_port,
            src_port,
            isn,
            seq + 1u,
            TCP_SYN | TCP_ACK,
        );
        write_checksums(packet_index, 20u);
        output_meta[packet_index].len = 40u;
        return;
    }

    if flows[base + 1u] != key_hash {
        return;
    }
    if state == STATE_SYN_RECEIVED && (flags & TCP_ACK) != 0u && ack == flows[base + 3u] {
        flows[base] = STATE_ESTABLISHED;
        return;
    }
    if state != STATE_ESTABLISHED {
        return;
    }
    if seq != flows[base + 2u] {
        return;
    }

    if (flags & TCP_FIN) != 0u {
        let next_ack = seq + payload_len + 1u;
        flows[base + 2u] = next_ack;
        write_ipv4(packet_index, 40u, dst_ip, src_ip);
        write_tcp(
            packet_index,
            dst_port,
            src_port,
            flows[base + 3u],
            next_ack,
            TCP_ACK,
        );
        write_checksums(packet_index, 20u);
        output_meta[packet_index].len = 40u;
        flows[base] = 0u;
        return;
    }

    if payload_len == 0u || !is_plaintext_get(packet_index, payload_offset, payload_len) {
        return;
    }

    let next_ack = seq + payload_len;
    let send_seq = flows[base + 3u];
    flows[base + 2u] = next_ack;
    flows[base + 3u] = send_seq + HTTP_RESPONSE_LEN;
    let response_len = 40u + HTTP_RESPONSE_LEN;

    write_ipv4(packet_index, response_len, dst_ip, src_ip);
    write_tcp(
        packet_index,
        dst_port,
        src_port,
        send_seq,
        next_ack,
        TCP_ACK | TCP_PSH,
    );
    write_http_response(packet_index);
    write_checksums(packet_index, 20u + HTTP_RESPONSE_LEN);
    output_meta[packet_index].len = response_len;
}
