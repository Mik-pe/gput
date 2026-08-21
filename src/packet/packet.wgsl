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
    flow_probe_limit: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> input_meta: array<PacketMeta>;
@group(0) @binding(1) var<storage, read> input_words: array<u32>;
@group(0) @binding(2) var<storage, read_write> output_meta: array<PacketMeta>;
@group(0) @binding(3) var<storage, read_write> output_words: array<u32>;
@group(0) @binding(4) var<storage, read_write> flows: array<atomic<u32>>;
@group(0) @binding(5) var<uniform> params: EngineParams;

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

const FLOW_STATE: u32 = 0u;
const FLOW_KEY_HASH: u32 = 1u;
const FLOW_SRC_IP: u32 = 2u;
const FLOW_DST_IP: u32 = 3u;
const FLOW_PORTS: u32 = 4u;
const FLOW_RECV_NEXT: u32 = 5u;
const FLOW_SEND_NEXT: u32 = 6u;
const FLOW_SEND_UNACKED: u32 = 7u;
const FLOW_LAST_CLIENT_SEQ: u32 = 8u;
const FLOW_LAST_CLIENT_LEN: u32 = 9u;
const FLOW_LAST_RESPONSE_SEQ: u32 = 10u;
const FLOW_LAST_RESPONSE_ACK: u32 = 11u;
const FLOW_LAST_RESPONSE_ID: u32 = 12u;
const FLOW_LAST_RESPONSE_LEN: u32 = 13u;
const FLOW_GENERATION: u32 = 14u;

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

fn flow_load(base: u32, field: u32) -> u32 {
    return atomicLoad(&flows[base + field]);
}

fn flow_store(base: u32, field: u32, value: u32) {
    atomicStore(&flows[base + field], value);
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
    return flow_load(base, FLOW_KEY_HASH) == key_hash &&
        flow_load(base, FLOW_SRC_IP) == src_ip &&
        flow_load(base, FLOW_DST_IP) == dst_ip &&
        flow_load(base, FLOW_PORTS) == ports;
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
        let base = slot * FLOW_WORDS;
        let state = flow_load(base, FLOW_STATE);
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
        let base = slot * FLOW_WORDS;
        let state = flow_load(base, FLOW_STATE);
        if state == STATE_FREE || state == STATE_TOMBSTONE {
            let claim = atomicCompareExchangeWeak(
                &flows[base + FLOW_STATE],
                state,
                STATE_CLAIMED,
            );
            if claim.exchanged {
                flow_store(base, FLOW_KEY_HASH, key_hash);
                flow_store(base, FLOW_SRC_IP, src_ip);
                flow_store(base, FLOW_DST_IP, dst_ip);
                flow_store(base, FLOW_PORTS, ports);
                var field = FLOW_RECV_NEXT;
                loop {
                    if field >= FLOW_WORDS {
                        break;
                    }
                    flow_store(base, field, 0u);
                    field += 1u;
                }
                return base;
            }
        }
        probe += 1u;
    }
    return INVALID_INDEX;
}

fn flow_release(base: u32) {
    flow_store(
        base,
        FLOW_GENERATION,
        flow_load(base, FLOW_GENERATION) + 1u,
    );
    flow_store(base, FLOW_STATE, STATE_TOMBSTONE);
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

fn write_http_response(packet_index: u32, response_id: u32) {
    let offset = RESPONSE_OFFSETS[response_id];
    let len = RESPONSE_LENGTHS[response_id];
    var index = 0u;
    loop {
        if index >= len {
            break;
        }
        let absolute = offset + index;
        let word = RESPONSE_WORDS[absolute / 4u];
        write_byte(
            packet_index,
            40u + index,
            (word >> ((absolute & 3u) * 8u)) & 0xffu,
        );
        index += 1u;
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
    write_checksums(packet_index, 20u + payload_len);
    output_meta[packet_index].len = packet_len;
}

fn find_crlf(packet_index: u32, offset: u32, len: u32) -> u32 {
    if len < 2u {
        return INVALID_INDEX;
    }
    var index = 0u;
    loop {
        if index + 1u >= len {
            break;
        }
        if read_byte(packet_index, offset + index) == 13u &&
            read_byte(packet_index, offset + index + 1u) == 10u {
            return index;
        }
        index += 1u;
    }
    return INVALID_INDEX;
}

fn find_space(
    packet_index: u32,
    offset: u32,
    start: u32,
    end: u32,
) -> u32 {
    var index = start;
    loop {
        if index >= end {
            break;
        }
        if read_byte(packet_index, offset + index) == 32u {
            return index;
        }
        index += 1u;
    }
    return INVALID_INDEX;
}

fn method_is_get(packet_index: u32, offset: u32, len: u32) -> bool {
    return len == 3u &&
        read_byte(packet_index, offset) == 71u &&
        read_byte(packet_index, offset + 1u) == 69u &&
        read_byte(packet_index, offset + 2u) == 84u;
}

fn version_is_supported(packet_index: u32, offset: u32, len: u32) -> bool {
    if len != 8u {
        return false;
    }
    let expected = array<u32, 7>(72u, 84u, 84u, 80u, 47u, 49u, 46u);
    var index = 0u;
    loop {
        if index >= 7u {
            break;
        }
        if read_byte(packet_index, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    let minor = read_byte(packet_index, offset + 7u);
    return minor == 48u || minor == 49u;
}

fn path_is_plaintext(packet_index: u32, offset: u32, len: u32) -> bool {
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
        if read_byte(packet_index, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    return true;
}

fn path_is_health(packet_index: u32, offset: u32, len: u32) -> bool {
    if len != 7u {
        return false;
    }
    let expected = array<u32, 7>(47u, 104u, 101u, 97u, 108u, 116u, 104u);
    var index = 0u;
    loop {
        if index >= len {
            break;
        }
        if read_byte(packet_index, offset + index) != expected[index] {
            return false;
        }
        index += 1u;
    }
    return true;
}

fn classify_http_request(packet_index: u32, payload_offset: u32, payload_len: u32) -> u32 {
    let line_end = find_crlf(packet_index, payload_offset, payload_len);
    if line_end == INVALID_INDEX {
        return RESPONSE_BAD_REQUEST;
    }
    let first_space = find_space(packet_index, payload_offset, 0u, line_end);
    if first_space == INVALID_INDEX {
        return RESPONSE_BAD_REQUEST;
    }
    let second_space = find_space(
        packet_index,
        payload_offset,
        first_space + 1u,
        line_end,
    );
    if second_space == INVALID_INDEX || second_space == first_space + 1u {
        return RESPONSE_BAD_REQUEST;
    }
    if !version_is_supported(
        packet_index,
        payload_offset + second_space + 1u,
        line_end - second_space - 1u,
    ) {
        return RESPONSE_BAD_REQUEST;
    }
    if !method_is_get(packet_index, payload_offset, first_space) {
        return RESPONSE_METHOD_NOT_ALLOWED;
    }

    let path_offset = payload_offset + first_space + 1u;
    var path_len = second_space - first_space - 1u;
    var index = 0u;
    loop {
        if index >= path_len {
            break;
        }
        if read_byte(packet_index, path_offset + index) == 63u {
            path_len = index;
            break;
        }
        index += 1u;
    }
    if path_is_plaintext(packet_index, path_offset, path_len) {
        return RESPONSE_PLAINTEXT;
    }
    if path_is_health(packet_index, path_offset, path_len) {
        return RESPONSE_HEALTH;
    }
    return RESPONSE_NOT_FOUND;
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
    let version_ihl = read_byte(packet_index, 0u);
    if version_ihl >> 4u != 4u || read_byte(packet_index, 9u) != 6u {
        return;
    }
    let ip_header_len = (version_ihl & 0x0fu) * 4u;
    if ip_header_len < 20u || ip_header_len + 20u > input_len {
        return;
    }
    if (read_u16_be(packet_index, 6u) & 0x3fffu) != 0u {
        return;
    }
    let total_len = read_u16_be(packet_index, 2u);
    if total_len > input_len || total_len < ip_header_len + 20u {
        return;
    }

    let tcp_offset = ip_header_len;
    let data_offset = (read_byte(packet_index, tcp_offset + 12u) >> 4u) * 4u;
    if data_offset < 20u || tcp_offset + data_offset > total_len {
        return;
    }
    let payload_offset = tcp_offset + data_offset;
    let payload_len = total_len - payload_offset;
    let src_ip = read_u32_be(packet_index, 12u);
    let dst_ip = read_u32_be(packet_index, 16u);
    let src_port = read_u16_be(packet_index, tcp_offset);
    let dst_port = read_u16_be(packet_index, tcp_offset + 2u);
    if dst_port != params.listen_port {
        return;
    }
    let seq = read_u32_be(packet_index, tcp_offset + 4u);
    let ack = read_u32_be(packet_index, tcp_offset + 8u);
    let flags = read_byte(packet_index, tcp_offset + 13u);
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
            flow_store(base, FLOW_RECV_NEXT, seq + 1u);
            flow_store(base, FLOW_SEND_NEXT, isn + 1u);
            flow_store(base, FLOW_SEND_UNACKED, isn);
            flow_store(base, FLOW_LAST_RESPONSE_LEN, 0u);
            flow_store(base, FLOW_STATE, STATE_SYN_RECEIVED);
        }

        let state = flow_load(base, FLOW_STATE);
        if state == STATE_SYN_RECEIVED {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flow_load(base, FLOW_SEND_NEXT) - 1u,
                flow_load(base, FLOW_RECV_NEXT),
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
                flow_load(base, FLOW_SEND_NEXT),
                flow_load(base, FLOW_RECV_NEXT),
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

    var state = flow_load(base, FLOW_STATE);
    if state == STATE_SYN_RECEIVED {
        if (flags & TCP_ACK) == 0u ||
            ack != flow_load(base, FLOW_SEND_NEXT) ||
            seq != flow_load(base, FLOW_RECV_NEXT) {
            return;
        }
        flow_store(base, FLOW_STATE, STATE_ESTABLISHED);
        flow_store(base, FLOW_SEND_UNACKED, ack);
        state = STATE_ESTABLISHED;
    }
    if state != STATE_ESTABLISHED {
        return;
    }

    if (flags & TCP_ACK) != 0u && ack == flow_load(base, FLOW_SEND_NEXT) {
        flow_store(base, FLOW_SEND_UNACKED, ack);
    }

    let recv_next = flow_load(base, FLOW_RECV_NEXT);
    if seq != recv_next {
        if seq == flow_load(base, FLOW_LAST_CLIENT_SEQ) &&
            payload_len == flow_load(base, FLOW_LAST_CLIENT_LEN) &&
            flow_load(base, FLOW_LAST_RESPONSE_LEN) > 0u {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flow_load(base, FLOW_LAST_RESPONSE_SEQ),
                flow_load(base, FLOW_LAST_RESPONSE_ACK),
                TCP_ACK | TCP_PSH,
                flow_load(base, FLOW_LAST_RESPONSE_ID),
            );
        } else {
            emit_packet(
                packet_index,
                dst_ip,
                src_ip,
                dst_port,
                src_port,
                flow_load(base, FLOW_SEND_NEXT),
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
            flow_load(base, FLOW_SEND_NEXT),
            next_ack,
            TCP_ACK,
            INVALID_INDEX,
        );
        flow_release(base);
        return;
    }

    let response_id = classify_http_request(packet_index, payload_offset, payload_len);
    let response_len = RESPONSE_LENGTHS[response_id];
    var next_ack = seq + payload_len;
    if has_fin {
        next_ack += 1u;
    }
    let response_seq = flow_load(base, FLOW_SEND_NEXT);
    flow_store(base, FLOW_RECV_NEXT, next_ack);
    flow_store(base, FLOW_SEND_NEXT, response_seq + response_len);
    flow_store(base, FLOW_LAST_CLIENT_SEQ, seq);
    flow_store(base, FLOW_LAST_CLIENT_LEN, payload_len);
    flow_store(base, FLOW_LAST_RESPONSE_SEQ, response_seq);
    flow_store(base, FLOW_LAST_RESPONSE_ACK, next_ack);
    flow_store(base, FLOW_LAST_RESPONSE_ID, response_id);
    flow_store(base, FLOW_LAST_RESPONSE_LEN, response_len);

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