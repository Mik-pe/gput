struct PacketMeta {
    len: u32,
    word_offset: u32,
}

struct EngineParams {
    packet_count: u32,
    output_word_base: u32,
    output_stride_words: u32,
    flow_capacity: u32,
    listen_port: u32,
    flow_probe_limit: u32,
    pad0: u32,
    pad1: u32,
}

struct FlowSlot {
    state: atomic<u32>,
    key_hash: u32,
    src_ip: u32,
    dst_ip: u32,
    ports: u32,
    recv_next: u32,
    send_next: u32,
    send_unacked: u32,
    last_client_seq: u32,
    last_client_len: u32,
    last_response_seq: u32,
    last_response_ack: u32,
    last_response_id: u32,
    last_response_len: u32,
    generation: u32,
    padding: u32,
}

@group(0) @binding(0) var<storage, read> input_meta: array<PacketMeta>;
@group(0) @binding(1) var<storage, read> input_words: array<u32>;
@group(0) @binding(2) var<storage, read_write> output_data: array<u32>;
@group(0) @binding(3) var<storage, read_write> flows: array<FlowSlot>;
@group(0) @binding(4) var<uniform> params: EngineParams;

const TCP_FIN: u32 = 0x01u;
const TCP_SYN: u32 = 0x02u;
const TCP_RST: u32 = 0x04u;
const TCP_PSH: u32 = 0x08u;
const TCP_ACK: u32 = 0x10u;

const STATE_FREE: u32 = 0u;
const STATE_SYN_RECEIVED: u32 = 1u;
const STATE_ESTABLISHED: u32 = 2u;
const STATE_TOMBSTONE: u32 = 3u;
const STATE_CLAIMED: u32 = 0xffffffffu;
const INVALID_INDEX: u32 = 0xffffffffu;

fn read_byte(input_base: u32, byte_index: u32) -> u32 {
    let word = input_words[input_base + byte_index / 4u];
    return (word >> ((byte_index & 3u) * 8u)) & 0xffu;
}

fn byte_swap_u32(value: u32) -> u32 {
    return ((value & 0x000000ffu) << 24u) |
        ((value & 0x0000ff00u) << 8u) |
        ((value & 0x00ff0000u) >> 8u) |
        ((value & 0xff000000u) >> 24u);
}

fn read_u16_be(input_base: u32, offset: u32) -> u32 {
    let word = input_words[input_base + offset / 4u];
    let pair = (word >> ((offset & 2u) * 8u)) & 0xffffu;
    return ((pair & 0xffu) << 8u) | (pair >> 8u);
}

fn read_u32_be(input_base: u32, offset: u32) -> u32 {
    return byte_swap_u32(input_words[input_base + offset / 4u]);
}

fn write_u16_be(packet_index: u32, offset: u32, value: u32) {
    let base = params.output_word_base + packet_index * params.output_stride_words;
    let word_index = base + offset / 4u;
    let shift = (offset & 2u) * 8u;
    let mask = 0xffffu << shift;
    let swapped = ((value & 0xffu) << 8u) | ((value >> 8u) & 0xffu);
    output_data[word_index] =
        (output_data[word_index] & ~mask) | (swapped << shift);
}

fn write_u32_be(packet_index: u32, offset: u32, value: u32) {
    let base = params.output_word_base + packet_index * params.output_stride_words;
    output_data[base + offset / 4u] = byte_swap_u32(value);
}

fn flow_hash(src_ip: u32, dst_ip: u32, src_port: u32, dst_port: u32) -> u32 {
    var hash = 2166136261u;
    hash = (hash ^ src_ip) * 16777619u;
    hash = (hash ^ dst_ip) * 16777619u;
    hash = (hash ^ src_port) * 16777619u;
    hash = (hash ^ dst_port) * 16777619u;
    return hash;
}

fn packed_ports(src_port: u32, dst_port: u32) -> u32 {
    return (src_port << 16u) | dst_port;
}

fn flow_matches(
    base: u32,
    key_hash: u32,
    src_ip: u32,
    dst_ip: u32,
    ports: u32,
) -> bool {
    return flows[base].key_hash == key_hash &&
        flows[base].src_ip == src_ip &&
        flows[base].dst_ip == dst_ip &&
        flows[base].ports == ports;
}

fn flow_find(
    key_hash: u32,
    src_ip: u32,
    dst_ip: u32,
    ports: u32,
) -> u32 {
    let limit = min(params.flow_probe_limit, params.flow_capacity);
    var probe = 0u;
    loop {
        if probe >= limit {
            break;
        }
        let slot = (key_hash + probe) & (params.flow_capacity - 1u);
        let base = slot;
        let state = atomicLoad(&flows[base].state);
        if state == STATE_FREE {
            return INVALID_INDEX;
        }
        if state != STATE_CLAIMED && state != STATE_TOMBSTONE &&
            flow_matches(base, key_hash, src_ip, dst_ip, ports) {
            return base;
        }
        probe += 1u;
    }
    return INVALID_INDEX;
}

fn flow_claim(
    key_hash: u32,
    src_ip: u32,
    dst_ip: u32,
    ports: u32,
) -> u32 {
    let limit = min(params.flow_probe_limit, params.flow_capacity);
    var probe = 0u;
    loop {
        if probe >= limit {
            break;
        }
        let slot = (key_hash + probe) & (params.flow_capacity - 1u);
        let base = slot;
        let state = atomicLoad(&flows[base].state);
        if state == STATE_FREE || state == STATE_TOMBSTONE {
            let claim = atomicCompareExchangeWeak(
                &flows[base].state,
                state,
                STATE_CLAIMED,
            );
            if claim.exchanged {
                flows[base].key_hash = key_hash;
                flows[base].src_ip = src_ip;
                flows[base].dst_ip = dst_ip;
                flows[base].ports = ports;
                flows[base].recv_next = 0u;
                flows[base].send_next = 0u;
                flows[base].send_unacked = 0u;
                flows[base].last_client_seq = 0u;
                flows[base].last_client_len = 0u;
                flows[base].last_response_seq = 0u;
                flows[base].last_response_ack = 0u;
                flows[base].last_response_id = 0u;
                flows[base].last_response_len = 0u;
                flows[base].generation = 0u;
                flows[base].padding = 0u;
                return base;
            }
        }
        probe += 1u;
    }
    return INVALID_INDEX;
}

fn flow_release(base: u32) {
    flows[base].generation += 1u;
    atomicStore(&flows[base].state, STATE_TOMBSTONE);
}

fn checksum_fold(input_sum: u32) -> u32 {
    var sum = input_sum;
    sum = (sum & 0xffffu) + (sum >> 16u);
    sum = (sum & 0xffffu) + (sum >> 16u);
    return (~sum) & 0xffffu;
}

fn checksum_u32(value: u32) -> u32 {
    return (value >> 16u) + (value & 0xffffu);
}

fn ipv4_checksum(packet_len: u32, src_ip: u32, dst_ip: u32) -> u32 {
    var sum = 0x4500u + packet_len + 0x4000u + 0x4006u;
    sum += checksum_u32(src_ip);
    sum += checksum_u32(dst_ip);
    return checksum_fold(sum);
}

fn tcp_checksum(
    src_ip: u32,
    dst_ip: u32,
    src_port: u32,
    dst_port: u32,
    seq: u32,
    ack: u32,
    flags: u32,
    tcp_len: u32,
    response_id: u32,
) -> u32 {
    var sum = checksum_u32(src_ip) + checksum_u32(dst_ip) + 6u + tcp_len;
    sum += src_port + dst_port;
    sum += checksum_u32(seq) + checksum_u32(ack);
    sum += 0x5000u | flags;
    sum += 0xffffu;
    if response_id < RESPONSE_COUNT {
        sum += RESPONSE_CHECKSUM_SUMS[response_id];
    }
    return checksum_fold(sum);
}

fn write_ipv4(packet_index: u32, len: u32, src_ip: u32, dst_ip: u32) {
    let base = params.output_word_base + packet_index * params.output_stride_words;
    output_data[base] = byte_swap_u32(0x45000000u | len);
    output_data[base + 1u] = byte_swap_u32(0x00004000u);
    output_data[base + 2u] = byte_swap_u32(0x40060000u);
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
    let base = params.output_word_base + packet_index * params.output_stride_words;
    output_data[base + 5u] = byte_swap_u32((src_port << 16u) | dst_port);
    write_u32_be(packet_index, 24u, seq);
    write_u32_be(packet_index, 28u, ack);
    output_data[base + 8u] = byte_swap_u32(0x50000000u | (flags << 16u) | 0xffffu);
    output_data[base + 9u] = 0u;
}

fn write_checksums(
    packet_index: u32,
    packet_len: u32,
    src_ip: u32,
    dst_ip: u32,
    src_port: u32,
    dst_port: u32,
    seq: u32,
    ack: u32,
    flags: u32,
    response_id: u32,
) {
    let tcp_len = packet_len - 20u;
    write_u16_be(packet_index, 10u, ipv4_checksum(packet_len, src_ip, dst_ip));
    write_u16_be(
        packet_index,
        36u,
        tcp_checksum(
            src_ip,
            dst_ip,
            src_port,
            dst_port,
            seq,
            ack,
            flags,
            tcp_len,
            response_id,
        ),
    );
}

fn write_http_response(packet_index: u32, response_id: u32) {
    let source_base = RESPONSE_WORD_OFFSETS[response_id];
    let output_base = params.output_word_base + packet_index * params.output_stride_words + 10u;
    let word_count = RESPONSE_WORD_COUNTS[response_id];
    var word_index = 0u;
    loop {
        if word_index >= word_count {
            break;
        }
        output_data[output_base + word_index] =
            RESPONSE_WORDS[source_base + word_index];
        word_index += 1u;
    }
}

fn emit_packet(
    packet_index: u32,
    src_ip: u32,
    dst_ip: u32,
    src_port: u32,
    dst_port: u32,
    seq: u32,
    ack: u32,
    flags: u32,
    response_id: u32,
) {
    var payload_len = 0u;
    if response_id < RESPONSE_COUNT {
        payload_len = RESPONSE_LENGTHS[response_id];
    }
    let packet_len = 40u + payload_len;
    write_ipv4(packet_index, packet_len, src_ip, dst_ip);
    write_tcp(packet_index, src_port, dst_port, seq, ack, flags);
    if response_id < RESPONSE_COUNT {
        write_http_response(packet_index, response_id);
    }
    write_checksums(
        packet_index,
        packet_len,
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        seq,
        ack,
        flags,
        response_id,
    );
    output_data[packet_index] = packet_len;
}

fn find_crlf(input_base: u32, offset: u32, len: u32) -> u32 {
    if len < 2u {
        return INVALID_INDEX;
    }
    var index = 0u;
    loop {
        if index + 1u >= len {
            break;
        }
        if read_byte(input_base, offset + index) == 13u &&
            read_byte(input_base, offset + index + 1u) == 10u {
            return index;
        }
        index += 1u;
    }
    return INVALID_INDEX;
}

fn find_space(
    input_base: u32,
    offset: u32,
    start: u32,
    end: u32,
) -> u32 {
    var index = start;
    loop {
        if index >= end {
            break;
        }
        if read_byte(input_base, offset + index) == 32u {
            return index;
        }
        index += 1u;
    }
    return INVALID_INDEX;
}

fn method_is_get(input_base: u32, offset: u32, len: u32) -> bool {
    return len == 3u &&
        read_byte(input_base, offset) == 71u &&
        read_byte(input_base, offset + 1u) == 69u &&
        read_byte(input_base, offset + 2u) == 84u;
}

fn version_is_supported(input_base: u32, offset: u32, len: u32) -> bool {
    if len != 8u {
        return false;
    }
    let expected = array<u32, 7>(72u, 84u, 84u, 80u, 47u, 49u, 46u);
    var index = 0u;
    loop {
        if index >= 7u {
            break;
        }
        if read_byte(input_base, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    let minor = read_byte(input_base, offset + 7u);
    return minor == 48u || minor == 49u;
}

fn path_is_plaintext(input_base: u32, offset: u32, len: u32) -> bool {
    if len != 10u {
        return false;
    }
    let expected = array<u32, 10>(
        47u, 112u, 108u, 97u, 105u, 110u, 116u, 101u, 120u, 116u,
    );
    var index = 0u;
    loop {
        if index >= len {
            break;
        }
        if read_byte(input_base, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    return true;
}

fn path_is_health(input_base: u32, offset: u32, len: u32) -> bool {
    if len != 7u {
        return false;
    }
    let expected = array<u32, 7>(47u, 104u, 101u, 97u, 108u, 116u, 104u);
    var index = 0u;
    loop {
        if index >= len {
            break;
        }
        if read_byte(input_base, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    return true;
}

fn classify_http_request(input_base: u32, payload_offset: u32, payload_len: u32) -> u32 {
    let line_end = find_crlf(input_base, payload_offset, payload_len);
    if line_end == INVALID_INDEX {
        return RESPONSE_BAD_REQUEST;
    }
    let first_space = find_space(input_base, payload_offset, 0u, line_end);
    if first_space == INVALID_INDEX {
        return RESPONSE_BAD_REQUEST;
    }
    let second_space = find_space(
        input_base,
        payload_offset,
        first_space + 1u,
        line_end,
    );
    if second_space == INVALID_INDEX || second_space == first_space + 1u {
        return RESPONSE_BAD_REQUEST;
    }
    if !version_is_supported(
        input_base,
        payload_offset + second_space + 1u,
        line_end - second_space - 1u,
    ) {
        return RESPONSE_BAD_REQUEST;
    }
    if !method_is_get(input_base, payload_offset, first_space) {
        return RESPONSE_METHOD_NOT_ALLOWED;
    }

    let path_offset = payload_offset + first_space + 1u;
    var path_len = second_space - first_space - 1u;
    var index = 0u;
    loop {
        if index >= path_len {
            break;
        }
        if read_byte(input_base, path_offset + index) == 63u {
            path_len = index;
            break;
        }
        index += 1u;
    }
    if path_is_plaintext(input_base, path_offset, path_len) {
        return RESPONSE_PLAINTEXT;
    }
    if path_is_health(input_base, path_offset, path_len) {
        return RESPONSE_HEALTH;
    }
    return RESPONSE_NOT_FOUND;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let packet_index = gid.x;
    if packet_index >= params.packet_count {
        return;
    }
    output_data[packet_index] = 0u;

    let input_len = input_meta[packet_index].len;
    let input_base = input_meta[packet_index].word_offset;
    if input_len < 40u {
        return;
    }
    let version_ihl = read_byte(input_base, 0u);
    if version_ihl >> 4u != 4u || read_byte(input_base, 9u) != 6u {
        return;
    }
    let ip_header_len = (version_ihl & 0x0fu) * 4u;
    if ip_header_len < 20u || ip_header_len + 20u > input_len {
        return;
    }
    if (read_u16_be(input_base, 6u) & 0x3fffu) != 0u {
        return;
    }
    let total_len = read_u16_be(input_base, 2u);
    if total_len > input_len || total_len < ip_header_len + 20u {
        return;
    }

    let tcp_offset = ip_header_len;
    let data_offset = (read_byte(input_base, tcp_offset + 12u) >> 4u) * 4u;
    if data_offset < 20u || tcp_offset + data_offset > total_len {
        return;
    }
    let payload_offset = tcp_offset + data_offset;
    let payload_len = total_len - payload_offset;
    let src_ip = read_u32_be(input_base, 12u);
    let dst_ip = read_u32_be(input_base, 16u);
    let src_port = read_u16_be(input_base, tcp_offset);
    let dst_port = read_u16_be(input_base, tcp_offset + 2u);
    if dst_port != params.listen_port {
        return;
    }
    let seq = read_u32_be(input_base, tcp_offset + 4u);
    let ack = read_u32_be(input_base, tcp_offset + 8u);
    let flags = read_byte(input_base, tcp_offset + 13u);
    let key_hash = flow_hash(src_ip, dst_ip, src_port, dst_port);
    let ports = packed_ports(src_port, dst_port);

    if (flags & TCP_SYN) != 0u && (flags & TCP_ACK) == 0u {
        var base = flow_find(key_hash, src_ip, dst_ip, ports);
        if base == INVALID_INDEX {
            base = flow_claim(key_hash, src_ip, dst_ip, ports);
            if base == INVALID_INDEX {
                return;
            }
            let isn = key_hash ^ 0xa5a55a5au;
            flows[base].recv_next = seq + 1u;
            flows[base].send_next = isn + 1u;
            flows[base].send_unacked = isn;
            flows[base].last_response_len = 0u;
            atomicStore(&flows[base].state, STATE_SYN_RECEIVED);
        }

        let state = atomicLoad(&flows[base].state);
        if state == STATE_SYN_RECEIVED {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flows[base].send_next - 1u,
                flows[base].recv_next,
                TCP_SYN | TCP_ACK,
                INVALID_INDEX,
            );
        } else if state == STATE_ESTABLISHED {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flows[base].send_next,
                flows[base].recv_next,
                TCP_ACK,
                INVALID_INDEX,
            );
        }
        return;
    }

    let base = flow_find(key_hash, src_ip, dst_ip, ports);
    if base == INVALID_INDEX {
        return;
    }
    if (flags & TCP_RST) != 0u {
        flow_release(base);
        return;
    }

    var state = atomicLoad(&flows[base].state);
    if state == STATE_SYN_RECEIVED {
        if (flags & TCP_ACK) == 0u ||
            ack != flows[base].send_next ||
            seq != flows[base].recv_next {
            return;
        }
        atomicStore(&flows[base].state, STATE_ESTABLISHED);
        flows[base].send_unacked = ack;
        state = STATE_ESTABLISHED;
    }
    if state != STATE_ESTABLISHED {
        return;
    }

    if (flags & TCP_ACK) != 0u && ack == flows[base].send_next {
        flows[base].send_unacked = ack;
    }

    let recv_next = flows[base].recv_next;
    if seq != recv_next {
        if seq == flows[base].last_client_seq &&
            payload_len == flows[base].last_client_len &&
            flows[base].last_response_len > 0u {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flows[base].last_response_seq,
                flows[base].last_response_ack,
                TCP_ACK | TCP_PSH,
                flows[base].last_response_id,
            );
        } else {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flows[base].send_next,
                recv_next,
                TCP_ACK,
                INVALID_INDEX,
            );
        }
        return;
    }

    let has_fin = (flags & TCP_FIN) != 0u;
    if payload_len == 0u {
        if !has_fin {
            return;
        }
        let next_ack = seq + 1u;
        emit_packet(
            packet_index,
            dst_ip,
            src_ip,
            dst_port,
            src_port,
            flows[base].send_next,
            next_ack,
            TCP_ACK,
            INVALID_INDEX,
        );
        flow_release(base);
        return;
    }

    let response_id = classify_http_request(input_base, payload_offset, payload_len);
    let response_len = RESPONSE_LENGTHS[response_id];
    var next_ack = seq + payload_len;
    if has_fin {
        next_ack += 1u;
    }
    let response_seq = flows[base].send_next;
    flows[base].recv_next = next_ack;
    flows[base].send_next = response_seq + response_len;
    flows[base].last_client_seq = seq;
    flows[base].last_client_len = payload_len;
    flows[base].last_response_seq = response_seq;
    flows[base].last_response_ack = next_ack;
    flows[base].last_response_id = response_id;
    flows[base].last_response_len = response_len;

    emit_packet(
        packet_index,
        dst_ip,
        src_ip,
        dst_port,
        src_port,
        response_seq,
        next_ack,
        TCP_ACK | TCP_PSH,
        response_id,
    );
    if has_fin {
        flow_release(base);
    }
}
