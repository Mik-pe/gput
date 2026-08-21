use anyhow::{Context, Result, ensure};
use gput::packet::{GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket};

const CLIENT_IP: u32 = 0x0a4d0002;
const SERVER_IP: u32 = 0x0a4d0001;
const CLIENT_PORT: u16 = 50_000;
const SERVER_PORT: u16 = 8080;
const CLIENT_ISN: u32 = 1_000_000;
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;

fn main() -> Result<()> {
    let engine = GpuPacketEngine::new(PacketEngineConfig {
        listen_port: SERVER_PORT,
        ..PacketEngineConfig::default()
    })?;

    let syn = tcp_packet(CLIENT_ISN, 0, TCP_SYN, &[])?;
    let syn_ack = one_output(&engine, syn, "SYN")?;
    validate_ipv4_tcp_checksum(syn_ack.as_bytes())?;
    ensure!(
        tcp_flags(syn_ack.as_bytes()) == TCP_SYN | TCP_ACK,
        "GPU did not emit SYN-ACK"
    );
    ensure!(
        tcp_ack(syn_ack.as_bytes()) == CLIENT_ISN + 1,
        "SYN-ACK acknowledged the wrong client sequence"
    );
    let server_isn = tcp_seq(syn_ack.as_bytes());

    let ack = tcp_packet(CLIENT_ISN + 1, server_isn + 1, TCP_ACK, &[])?;
    let handshake_output = engine.process_batch(&[ack])?;
    ensure!(
        handshake_output == [None],
        "pure handshake ACK should not emit a packet"
    );

    let request = b"GET /plaintext HTTP/1.1\r\nHost: gpu\r\nConnection: keep-alive\r\n\r\n";
    let get = tcp_packet(CLIENT_ISN + 1, server_isn + 1, TCP_ACK | TCP_PSH, request)?;
    let response = one_output(&engine, get, "GET /plaintext")?;
    validate_ipv4_tcp_checksum(response.as_bytes())?;
    let payload = tcp_payload(response.as_bytes())?;
    ensure!(
        payload.ends_with(b"\r\n\r\nHello, World!\n"),
        "GPU packet response did not contain the plaintext HTTP body"
    );

    let fin = tcp_packet(
        CLIENT_ISN + 1 + request.len() as u32,
        tcp_seq(response.as_bytes()) + payload.len() as u32,
        TCP_ACK | TCP_FIN,
        &[],
    )?;
    let fin_ack = one_output(&engine, fin, "FIN")?;
    ensure!(
        tcp_flags(fin_ack.as_bytes()) == TCP_ACK,
        "GPU did not ACK FIN"
    );
    validate_ipv4_tcp_checksum(fin_ack.as_bytes())?;

    println!("GPU packet TCP handshake: ok");
    println!(
        "GPU packet HTTP payload:\n{}",
        String::from_utf8_lossy(payload)
    );
    Ok(())
}

fn one_output(engine: &impl PacketEngine, input: RawPacket, label: &str) -> Result<RawPacket> {
    let mut output = engine.process_batch(&[input])?;
    output
        .pop()
        .flatten()
        .with_context(|| format!("GPU emitted no packet for {label}"))
}

fn tcp_packet(seq: u32, ack: u32, flags: u8, payload: &[u8]) -> Result<RawPacket> {
    let total_len = 40 + payload.len();
    let mut packet = vec![0_u8; total_len];
    packet[0] = 0x45;
    write_u16(&mut packet, 2, total_len as u16);
    write_u16(&mut packet, 6, 0x4000);
    packet[8] = 64;
    packet[9] = 6;
    write_u32(&mut packet, 12, CLIENT_IP);
    write_u32(&mut packet, 16, SERVER_IP);
    write_u16(&mut packet, 20, CLIENT_PORT);
    write_u16(&mut packet, 22, SERVER_PORT);
    write_u32(&mut packet, 24, seq);
    write_u32(&mut packet, 28, ack);
    packet[32] = 0x50;
    packet[33] = flags;
    write_u16(&mut packet, 34, 65_535);
    packet[40..].copy_from_slice(payload);
    let ip_checksum = checksum(&packet[..20]);
    write_u16(&mut packet, 10, ip_checksum);
    let tcp_checksum = tcp_checksum(&packet);
    write_u16(&mut packet, 36, tcp_checksum);
    RawPacket::new(packet)
}

fn validate_ipv4_tcp_checksum(packet: &[u8]) -> Result<()> {
    ensure!(packet.len() >= 40, "response packet is too small");
    ensure!(checksum(&packet[..20]) == 0, "invalid IPv4 header checksum");
    ensure!(
        tcp_checksum_with_existing_field(packet) == 0,
        "invalid TCP checksum"
    );
    Ok(())
}

fn tcp_payload(packet: &[u8]) -> Result<&[u8]> {
    ensure!(packet.len() >= 40, "TCP packet is too small");
    let header_len = usize::from(packet[32] >> 4) * 4;
    let payload_offset = 20 + header_len;
    ensure!(payload_offset <= packet.len(), "invalid TCP data offset");
    Ok(&packet[payload_offset..])
}

fn tcp_seq(packet: &[u8]) -> u32 {
    read_u32(packet, 24)
}

fn tcp_ack(packet: &[u8]) -> u32 {
    read_u32(packet, 28)
}

fn tcp_flags(packet: &[u8]) -> u8 {
    packet[33]
}

fn checksum(bytes: &[u8]) -> u16 {
    fold_checksum(sum_words(bytes))
}

fn tcp_checksum(packet: &[u8]) -> u16 {
    let mut copy = packet.to_vec();
    copy[36] = 0;
    copy[37] = 0;
    fold_checksum(tcp_sum(&copy))
}

fn tcp_checksum_with_existing_field(packet: &[u8]) -> u16 {
    fold_checksum(tcp_sum(packet))
}

fn tcp_sum(packet: &[u8]) -> u32 {
    let mut sum = 0_u32;
    sum += u32::from(read_u16(packet, 12));
    sum += u32::from(read_u16(packet, 14));
    sum += u32::from(read_u16(packet, 16));
    sum += u32::from(read_u16(packet, 18));
    sum += 6;
    sum += (packet.len() - 20) as u32;
    sum + sum_words(&packet[20..])
}

fn sum_words(bytes: &[u8]) -> u32 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks(2) {
        let high = u16::from(chunk[0]) << 8;
        let low = chunk.get(1).copied().map(u16::from).unwrap_or(0);
        sum += u32::from(high | low);
    }
    sum
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
