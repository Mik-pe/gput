use anyhow::{Result, ensure};

use super::{MAX_RAW_PACKET_BYTES, RawPacket};

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowKey {
    pub src_ip: u32,
    pub dst_ip: u32,
    pub src_port: u16,
    pub dst_port: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct TcpPacketView<'a> {
    pub key: FlowKey,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
pub struct TcpPacketSpec<'a> {
    pub key: FlowKey,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub payload: &'a [u8],
}

pub fn parse_ipv4_tcp(packet: &RawPacket) -> Option<TcpPacketView<'_>> {
    parse_ipv4_tcp_bytes(packet.as_bytes())
}

pub fn build_ipv4_tcp_packet(spec: TcpPacketSpec<'_>) -> Result<RawPacket> {
    let total_len = 40_usize
        .checked_add(spec.payload.len())
        .expect("packet length overflow");
    ensure!(
        total_len <= MAX_RAW_PACKET_BYTES,
        "IPv4/TCP packet is {total_len} bytes; maximum is {MAX_RAW_PACKET_BYTES}"
    );
    ensure!(total_len <= usize::from(u16::MAX), "packet length exceeds IPv4 limit");

    let mut packet = vec![0_u8; total_len];
    packet[0] = 0x45;
    write_u16(&mut packet, 2, total_len as u16);
    write_u16(&mut packet, 6, 0x4000);
    packet[8] = 64;
    packet[9] = 6;
    write_u32(&mut packet, 12, spec.key.src_ip);
    write_u32(&mut packet, 16, spec.key.dst_ip);
    write_u16(&mut packet, 20, spec.key.src_port);
    write_u16(&mut packet, 22, spec.key.dst_port);
    write_u32(&mut packet, 24, spec.seq);
    write_u32(&mut packet, 28, spec.ack);
    packet[32] = 0x50;
    packet[33] = spec.flags;
    write_u16(&mut packet, 34, u16::MAX);
    packet[40..].copy_from_slice(spec.payload);

    let ip_checksum = checksum(&packet[..20]);
    write_u16(&mut packet, 10, ip_checksum);
    let tcp_checksum = tcp_checksum(&packet, 20, true);
    write_u16(&mut packet, 36, tcp_checksum);
    RawPacket::new(packet)
}

pub fn validate_ipv4_tcp_checksums(packet: &RawPacket) -> Result<()> {
    let bytes = packet.as_bytes();
    ensure!(bytes.len() >= 40, "IPv4/TCP packet is too small");
    let header_len = usize::from(bytes[0] & 0x0f) * 4;
    ensure!(header_len >= 20 && header_len <= bytes.len(), "invalid IPv4 header length");
    let total_len = usize::from(read_u16(bytes, 2));
    ensure!(total_len <= bytes.len(), "IPv4 total length exceeds packet bytes");
    ensure!(checksum(&bytes[..header_len]) == 0, "invalid IPv4 header checksum");
    ensure!(
        tcp_checksum(&bytes[..total_len], header_len, false) == 0,
        "invalid TCP checksum"
    );
    Ok(())
}

pub fn flow_hash(key: FlowKey) -> u32 {
    let mut hash = 2_166_136_261_u32;
    hash = (hash ^ key.src_ip).wrapping_mul(16_777_619);
    hash = (hash ^ key.dst_ip).wrapping_mul(16_777_619);
    hash = (hash ^ u32::from(key.src_port)).wrapping_mul(16_777_619);
    (hash ^ u32::from(key.dst_port)).wrapping_mul(16_777_619)
}

fn parse_ipv4_tcp_bytes(bytes: &[u8]) -> Option<TcpPacketView<'_>> {
    if bytes.len() < 40 || bytes[0] >> 4 != 4 || bytes[9] != 6 {
        return None;
    }

    let ip_header_len = usize::from(bytes[0] & 0x0f) * 4;
    if ip_header_len < 20 || ip_header_len + 20 > bytes.len() {
        return None;
    }
    let fragment = read_u16(bytes, 6);
    if fragment & 0x3fff != 0 {
        return None;
    }
    let total_len = usize::from(read_u16(bytes, 2));
    if total_len < ip_header_len + 20 || total_len > bytes.len() {
        return None;
    }

    let tcp_header_len = usize::from(bytes[ip_header_len + 12] >> 4) * 4;
    if tcp_header_len < 20 || ip_header_len + tcp_header_len > total_len {
        return None;
    }
    let payload_offset = ip_header_len + tcp_header_len;

    Some(TcpPacketView {
        key: FlowKey {
            src_ip: read_u32(bytes, 12),
            dst_ip: read_u32(bytes, 16),
            src_port: read_u16(bytes, ip_header_len),
            dst_port: read_u16(bytes, ip_header_len + 2),
        },
        seq: read_u32(bytes, ip_header_len + 4),
        ack: read_u32(bytes, ip_header_len + 8),
        flags: bytes[ip_header_len + 13],
        payload: &bytes[payload_offset..total_len],
    })
}

fn tcp_checksum(packet: &[u8], ip_header_len: usize, clear_checksum: bool) -> u16 {
    let tcp_len = packet.len() - ip_header_len;
    let mut sum = 0_u32;
    sum += u32::from(read_u16(packet, 12));
    sum += u32::from(read_u16(packet, 14));
    sum += u32::from(read_u16(packet, 16));
    sum += u32::from(read_u16(packet, 18));
    sum += 6;
    sum += tcp_len as u32;

    for (word_index, chunk) in packet[ip_header_len..].chunks(2).enumerate() {
        if clear_checksum && word_index == 8 {
            continue;
        }
        let high = u16::from(chunk[0]) << 8;
        let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
        sum += u32::from(high | low);
    }
    fold_checksum(sum)
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks(2) {
        let high = u16::from(chunk[0]) << 8;
        let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
        sum += u32::from(high | low);
    }
    fold_checksum(sum)
}

fn fold_checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_builder_round_trips_through_parser_and_checksums() {
        let key = FlowKey {
            src_ip: 0x0a4d0002,
            dst_ip: 0x0a4d0001,
            src_port: 50_000,
            dst_port: 8080,
        };
        let packet = build_ipv4_tcp_packet(TcpPacketSpec {
            key,
            seq: 123,
            ack: 456,
            flags: TCP_ACK | TCP_PSH,
            payload: b"GET /plaintext HTTP/1.1\r\n\r\n",
        })
        .expect("packet builds");
        let parsed = parse_ipv4_tcp(&packet).expect("packet parses");

        assert_eq!(parsed.key, key);
        assert_eq!(parsed.seq, 123);
        assert_eq!(parsed.ack, 456);
        assert_eq!(parsed.flags, TCP_ACK | TCP_PSH);
        assert_eq!(parsed.payload, b"GET /plaintext HTTP/1.1\r\n\r\n");
        validate_ipv4_tcp_checksums(&packet).expect("checksums are valid");
    }
}
