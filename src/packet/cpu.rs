use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result};

use super::{
    PacketEngine, PacketEngineConfig, PacketEngineMetrics, RawPacket, classify_http_request,
    packet_response, validate_config,
    wire::{
        FlowKey, TCP_ACK, TCP_FIN, TCP_PSH, TCP_RST, TCP_SYN, TcpPacketSpec,
        build_ipv4_tcp_packet, flow_hash, parse_ipv4_tcp,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowState {
    SynReceived,
    Established,
}

#[derive(Debug, Clone, Copy)]
struct LastResponse {
    client_seq: u32,
    client_len: usize,
    response_seq: u32,
    response_ack: u32,
    response_id: u32,
}

#[derive(Debug)]
struct Flow {
    state: FlowState,
    recv_next: u32,
    send_next: u32,
    send_unacked: u32,
    last_response: Option<LastResponse>,
}

pub struct CpuPacketEngine {
    config: PacketEngineConfig,
    flows: Mutex<HashMap<FlowKey, Flow>>,
    dispatches: AtomicU64,
    packets: AtomicU64,
}

impl CpuPacketEngine {
    pub fn new(config: PacketEngineConfig) -> Result<Self> {
        validate_config(config)?;
        Ok(Self {
            config,
            flows: Mutex::new(HashMap::with_capacity(config.flow_capacity)),
            dispatches: AtomicU64::new(0),
            packets: AtomicU64::new(0),
        })
    }
}

impl PacketEngine for CpuPacketEngine {
    fn name(&self) -> &'static str {
        "cpu-packet"
    }

    fn process_batch(&self, packets: &[RawPacket]) -> Result<Vec<Option<RawPacket>>> {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        self.packets
            .fetch_add(packets.len() as u64, Ordering::Relaxed);
        let mut flows = self.flows.lock().context("CPU packet flow table poisoned")?;
        packets
            .iter()
            .map(|packet| process_packet(&mut flows, self.config, packet))
            .collect()
    }

    fn metrics(&self) -> PacketEngineMetrics {
        PacketEngineMetrics {
            dispatches: self.dispatches.load(Ordering::Relaxed),
            packets: self.packets.load(Ordering::Relaxed),
        }
    }
}

fn process_packet(
    flows: &mut HashMap<FlowKey, Flow>,
    config: PacketEngineConfig,
    packet: &RawPacket,
) -> Result<Option<RawPacket>> {
    let Some(tcp) = parse_ipv4_tcp(packet) else {
        return Ok(None);
    };
    if tcp.key.dst_port != config.listen_port {
        return Ok(None);
    }

    if tcp.flags & TCP_RST != 0 {
        flows.remove(&tcp.key);
        return Ok(None);
    }

    if tcp.flags & TCP_SYN != 0 && tcp.flags & TCP_ACK == 0 {
        if !flows.contains_key(&tcp.key) && flows.len() >= config.flow_capacity {
            return Ok(None);
        }
        let client_next = tcp.seq.wrapping_add(1);
        let server_isn = flow_hash(tcp.key) ^ 0xa5a55a5a;
        let flow = flows.entry(tcp.key).or_insert(Flow {
            state: FlowState::SynReceived,
            recv_next: client_next,
            send_next: server_isn.wrapping_add(1),
            send_unacked: server_isn,
            last_response: None,
        });
        if flow.state == FlowState::SynReceived && flow.recv_next != client_next {
            flow.recv_next = client_next;
            flow.send_next = server_isn.wrapping_add(1);
            flow.send_unacked = server_isn;
            flow.last_response = None;
        }
        let sequence = if flow.state == FlowState::SynReceived {
            flow.send_next.wrapping_sub(1)
        } else {
            flow.send_next
        };
        let flags = if flow.state == FlowState::SynReceived {
            TCP_SYN | TCP_ACK
        } else {
            TCP_ACK
        };
        return emit(tcp.key, sequence, flow.recv_next, flags, &[]).map(Some);
    }

    let Some(flow) = flows.get_mut(&tcp.key) else {
        return Ok(None);
    };
    if flow.state == FlowState::SynReceived {
        if tcp.flags & TCP_ACK == 0
            || tcp.ack != flow.send_next
            || tcp.seq != flow.recv_next
        {
            return Ok(None);
        }
        flow.state = FlowState::Established;
        flow.send_unacked = tcp.ack;
    }

    if tcp.flags & TCP_ACK != 0 && tcp.ack == flow.send_next {
        flow.send_unacked = tcp.ack;
    }

    if tcp.seq != flow.recv_next {
        if let Some(last) = flow.last_response
            && tcp.seq == last.client_seq
            && tcp.payload.len() == last.client_len
        {
            let response = cpu_response(last.response_id);
            return emit(
                tcp.key,
                last.response_seq,
                last.response_ack,
                TCP_ACK | TCP_PSH,
                &response,
            )
            .map(Some);
        }
        return emit(tcp.key, flow.send_next, flow.recv_next, TCP_ACK, &[]).map(Some);
    }

    let has_fin = tcp.flags & TCP_FIN != 0;
    if tcp.payload.is_empty() {
        if !has_fin {
            return Ok(None);
        }
        let next_ack = tcp.seq.wrapping_add(1);
        let output = emit(tcp.key, flow.send_next, next_ack, TCP_ACK, &[])?;
        flows.remove(&tcp.key);
        return Ok(Some(output));
    }

    let response_id = classify_http_request(tcp.payload);
    let response = cpu_response(response_id);
    let fin_advance = if has_fin { 1 } else { 0 };
    let next_ack = tcp
        .seq
        .wrapping_add(tcp.payload.len() as u32)
        .wrapping_add(fin_advance);
    let response_seq = flow.send_next;
    flow.recv_next = next_ack;
    flow.send_next = flow.send_next.wrapping_add(response.len() as u32);
    flow.last_response = Some(LastResponse {
        client_seq: tcp.seq,
        client_len: tcp.payload.len(),
        response_seq,
        response_ack: next_ack,
        response_id,
    });
    let output = emit(
        tcp.key,
        response_seq,
        next_ack,
        TCP_ACK | TCP_PSH,
        &response,
    )?;
    if has_fin {
        flows.remove(&tcp.key);
    }
    Ok(Some(output))
}

fn cpu_response(response_id: u32) -> Vec<u8> {
    let mut response = packet_response(response_id).to_vec();
    if let Some(offset) = response
        .windows(b"gpu-packet".len())
        .position(|window| window == b"gpu-packet")
    {
        response[offset..offset + b"cpu-packet".len()].copy_from_slice(b"cpu-packet");
    }
    response
}

fn emit(
    key: FlowKey,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) -> Result<RawPacket> {
    build_ipv4_tcp_packet(TcpPacketSpec {
        key: FlowKey {
            src_ip: key.dst_ip,
            dst_ip: key.src_ip,
            src_port: key.dst_port,
            dst_port: key.src_port,
        },
        seq,
        ack,
        flags,
        payload,
    })
}
