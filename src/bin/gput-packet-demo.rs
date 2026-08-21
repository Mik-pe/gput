use anyhow::{Context, Result, ensure};
use gput::packet::{
    CpuPacketEngine, FlowKey, GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket, TCP_ACK,
    TCP_FIN, TCP_PSH, TCP_SYN, TcpPacketSpec, build_ipv4_tcp_packet, flow_hash,
    parse_ipv4_tcp, validate_ipv4_tcp_checksums,
};

const CLIENT_IP: u32 = 0x0a4d0002;
const SERVER_IP: u32 = 0x0a4d0001;
const CLIENT_PORT: u16 = 50_000;
const SERVER_PORT: u16 = 8080;
const CLIENT_ISN: u32 = 1_000_000;

#[derive(Debug, Clone, Copy)]
struct FlowCursor {
    key: FlowKey,
    client_next: u32,
    server_next: u32,
}

fn main() -> Result<()> {
    let config = PacketEngineConfig {
        max_batch_size: 64,
        flow_capacity: 64,
        flow_probe_limit: 64,
        listen_port: SERVER_PORT,
    };

    let cpu = CpuPacketEngine::new(config)?;
    run_protocol_proof(&cpu)?;

    let gpu = GpuPacketEngine::new(config)?;
    run_protocol_proof(&gpu)?;

    println!("CPU and GPU packet semantics agree: ok");
    println!("GPU adapter: {}", gpu.adapter_name());
    Ok(())
}

fn run_protocol_proof(engine: &impl PacketEngine) -> Result<()> {
    let key = FlowKey {
        src_ip: CLIENT_IP,
        dst_ip: SERVER_IP,
        src_port: CLIENT_PORT,
        dst_port: SERVER_PORT,
    };
    let mut flow = open_flow(engine, key, CLIENT_ISN)?;

    let request = b"GET /plaintext HTTP/1.1\r\nHost: gpu\r\nConnection: keep-alive\r\n\r\n";
    let request_packet = client_packet(flow, TCP_ACK | TCP_PSH, request)?;
    let response = one_output(engine, request_packet.clone(), "GET /plaintext")?;
    let duplicate = one_output(engine, request_packet, "duplicate GET /plaintext")?;
    validate_response(
        &response,
        flow,
        request,
        b"HTTP/1.1 200 OK\r\n",
        b"Hello, World!\n",
        engine.name(),
    )?;
    ensure!(
        parse_ipv4_tcp(&response).map(|tcp| (tcp.seq, tcp.ack, tcp.payload))
            == parse_ipv4_tcp(&duplicate).map(|tcp| (tcp.seq, tcp.ack, tcp.payload)),
        "{} did not reproduce the same unacknowledged response",
        engine.name()
    );
    advance_flow(&mut flow, request, &response)?;

    exchange(
        engine,
        &mut flow,
        b"GET /health HTTP/1.1\r\nHost: gpu\r\n\r\n",
        b"HTTP/1.1 200 OK\r\n",
        b"ok\n",
    )?;
    exchange(
        engine,
        &mut flow,
        b"GET /missing HTTP/1.1\r\nHost: gpu\r\n\r\n",
        b"HTTP/1.1 404 Not Found\r\n",
        b"not found\n",
    )?;
    exchange(
        engine,
        &mut flow,
        b"POST /plaintext HTTP/1.1\r\nHost: gpu\r\n\r\n",
        b"HTTP/1.1 405 Method Not Allowed\r\n",
        b"method not allowed\n",
    )?;
    close_flow(engine, flow)?;
    prove_collision_safe_lookup(engine)?;

    println!(
        "{}: handshake, retransmit, routing, checksums, FIN and collisions ok",
        engine.name()
    );
    Ok(())
}

fn open_flow(
    engine: &impl PacketEngine,
    key: FlowKey,
    client_isn: u32,
) -> Result<FlowCursor> {
    let syn = build_ipv4_tcp_packet(TcpPacketSpec {
        key,
        seq: client_isn,
        ack: 0,
        flags: TCP_SYN,
        payload: &[],
    })?;
    let syn_ack = one_output(engine, syn.clone(), "SYN")?;
    let repeated_syn_ack = one_output(engine, syn, "retransmitted SYN")?;
    validate_ipv4_tcp_checksums(&syn_ack)?;
    validate_ipv4_tcp_checksums(&repeated_syn_ack)?;
    let tcp = parse_ipv4_tcp(&syn_ack).context("SYN-ACK did not parse")?;
    let repeated = parse_ipv4_tcp(&repeated_syn_ack).context("repeated SYN-ACK did not parse")?;
    ensure!(tcp.flags == TCP_SYN | TCP_ACK, "engine did not emit SYN-ACK");
    ensure!(
        tcp.ack == client_isn + 1,
        "SYN-ACK acknowledged the wrong client sequence"
    );
    ensure!(
        tcp.seq == repeated.seq,
        "retransmitted SYN changed the server ISN"
    );

    let flow = FlowCursor {
        key,
        client_next: client_isn + 1,
        server_next: tcp.seq + 1,
    };
    let ack = client_packet(flow, TCP_ACK, &[])?;
    ensure!(
        engine.process_batch(&[ack])? == [None],
        "pure handshake ACK should not emit a packet"
    );
    Ok(flow)
}

fn exchange(
    engine: &impl PacketEngine,
    flow: &mut FlowCursor,
    request: &[u8],
    expected_status: &[u8],
    expected_body: &[u8],
) -> Result<()> {
    let packet = client_packet(*flow, TCP_ACK | TCP_PSH, request)?;
    let response = one_output(engine, packet, "HTTP request")?;
    validate_response(
        &response,
        *flow,
        request,
        expected_status,
        expected_body,
        engine.name(),
    )?;
    advance_flow(flow, request, &response)
}

fn validate_response(
    response: &RawPacket,
    flow: FlowCursor,
    request: &[u8],
    expected_status: &[u8],
    expected_body: &[u8],
    expected_backend: &str,
) -> Result<()> {
    validate_ipv4_tcp_checksums(response)?;
    let tcp = parse_ipv4_tcp(response).context("response packet did not parse")?;
    ensure!(
        tcp.seq == flow.server_next,
        "response used the wrong server sequence"
    );
    ensure!(
        tcp.ack == flow.client_next + request.len() as u32,
        "response acknowledged the wrong client sequence"
    );
    ensure!(
        tcp.payload.starts_with(expected_status),
        "response had the wrong HTTP status"
    );
    ensure!(
        tcp.payload.ends_with(expected_body),
        "response had the wrong HTTP body"
    );
    ensure!(
        tcp.payload
            .windows(expected_backend.len())
            .any(|window| window == expected_backend.as_bytes()),
        "response did not identify backend {expected_backend}"
    );
    Ok(())
}

fn advance_flow(flow: &mut FlowCursor, request: &[u8], response: &RawPacket) -> Result<()> {
    let tcp = parse_ipv4_tcp(response).context("response packet did not parse")?;
    flow.client_next = flow.client_next.wrapping_add(request.len() as u32);
    flow.server_next = flow.server_next.wrapping_add(tcp.payload.len() as u32);
    Ok(())
}

fn close_flow(engine: &impl PacketEngine, flow: FlowCursor) -> Result<()> {
    let fin = client_packet(flow, TCP_ACK | TCP_FIN, &[])?;
    let response = one_output(engine, fin, "FIN")?;
    validate_ipv4_tcp_checksums(&response)?;
    let tcp = parse_ipv4_tcp(&response).context("FIN ACK did not parse")?;
    ensure!(tcp.flags == TCP_ACK, "engine did not ACK FIN");
    ensure!(
        tcp.ack == flow.client_next + 1,
        "FIN ACK used the wrong acknowledgement"
    );
    Ok(())
}

fn prove_collision_safe_lookup(engine: &impl PacketEngine) -> Result<()> {
    let first = FlowKey {
        src_ip: CLIENT_IP,
        dst_ip: SERVER_IP,
        src_port: 40_000,
        dst_port: SERVER_PORT,
    };
    let slot = flow_hash(first) & 63;
    let second_port = (40_001..u16::MAX)
        .find(|port| {
            flow_hash(FlowKey {
                src_port: *port,
                ..first
            }) & 63
                == slot
        })
        .context("could not find a colliding flow for the demo")?;
    let second = FlowKey {
        src_port: second_port,
        ..first
    };
    ensure!(first != second, "collision test reused the same flow");

    let flows = [(first, 2_000_000_u32), (second, 3_000_000_u32)];
    let syns = flows
        .map(|(key, seq)| {
            build_ipv4_tcp_packet(TcpPacketSpec {
                key,
                seq,
                ack: 0,
                flags: TCP_SYN,
                payload: &[],
            })
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    let syn_acks = engine.process_batch(&syns)?;
    let mut cursors = Vec::new();
    for ((key, client_isn), response) in flows.into_iter().zip(syn_acks) {
        let response = response.context("colliding SYN produced no SYN-ACK")?;
        let tcp = parse_ipv4_tcp(&response).context("colliding SYN-ACK did not parse")?;
        cursors.push(FlowCursor {
            key,
            client_next: client_isn + 1,
            server_next: tcp.seq + 1,
        });
    }

    let acknowledgements = cursors
        .iter()
        .copied()
        .map(|flow| client_packet(flow, TCP_ACK, &[]))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        engine
            .process_batch(&acknowledgements)?
            .iter()
            .all(Option::is_none),
        "collision handshake ACK unexpectedly emitted traffic"
    );

    let request = b"GET /plaintext HTTP/1.1\r\nHost: collision\r\n\r\n";
    let gets = cursors
        .iter()
        .copied()
        .map(|flow| client_packet(flow, TCP_ACK | TCP_PSH, request))
        .collect::<Result<Vec<_>>>()?;
    let responses = engine.process_batch(&gets)?;
    ensure!(
        responses.iter().all(|response| {
            response
                .as_ref()
                .and_then(parse_ipv4_tcp)
                .is_some_and(|tcp| tcp.payload.ends_with(b"Hello, World!\n"))
        }),
        "one of the colliding flows lost its HTTP response"
    );
    Ok(())
}

fn client_packet(flow: FlowCursor, flags: u8, payload: &[u8]) -> Result<RawPacket> {
    build_ipv4_tcp_packet(TcpPacketSpec {
        key: flow.key,
        seq: flow.client_next,
        ack: flow.server_next,
        flags,
        payload,
    })
}

fn one_output(engine: &impl PacketEngine, input: RawPacket, label: &str) -> Result<RawPacket> {
    let mut output = engine.process_batch(&[input])?;
    output
        .pop()
        .flatten()
        .with_context(|| format!("{} emitted no packet for {label}", engine.name()))
}
