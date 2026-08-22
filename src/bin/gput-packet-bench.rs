use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use gput::packet::{
    CpuPacketEngine, FlowKey, GpuPacketEngine, PacketEngine, PacketEngineConfig,
    PacketEngineMetrics, RawPacket, TCP_ACK, TCP_FIN, TCP_PSH, TCP_SYN, TcpPacketSpec,
    build_ipv4_tcp_packet, parse_ipv4_tcp, validate_ipv4_tcp_checksums,
};

const SERVER_IP: u32 = 0x0a4d0001;
const DEFAULT_REQUEST: &[u8] =
    b"GET /plaintext HTTP/1.1\r\nHost: packet-bench\r\nConnection: keep-alive\r\n\r\n";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    Cpu,
    Gpu,
    #[default]
    Both,
}

#[derive(Debug, Parser)]
#[command(
    name = "gput-packet-bench",
    version,
    about = "Compare the same raw IPv4/TCP state machine on a CPU reference and the GPU packet engine"
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = BackendChoice::Both)]
    backend: BackendChoice,

    #[arg(long, default_value_t = 65_536)]
    flows: usize,

    #[arg(long, default_value_t = 1000)]
    requests_per_flow: usize,

    #[arg(long, default_value_t = 20)]
    warmup_requests_per_flow: usize,

    #[arg(long, default_value_t = 65_536)]
    batch_size: usize,

    #[arg(long, default_value_t = 131_072)]
    flow_capacity: usize,

    #[arg(long, default_value_t = 64)]
    flow_probe_limit: usize,

    #[arg(long, default_value_t = 8080)]
    listen_port: u16,

    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy)]
struct BenchSettings {
    flows: usize,
    requests_per_flow: usize,
    warmup_requests_per_flow: usize,
    listen_port: u16,
}

#[derive(Debug, Clone, Copy)]
struct FlowCursor {
    key: FlowKey,
    client_next: u32,
    server_next: u32,
}

#[derive(Debug)]
struct BenchResult {
    backend: &'static str,
    adapter: Option<String>,
    flows: usize,
    requests: u64,
    handshake: Duration,
    engine_elapsed: Duration,
    wall_elapsed: Duration,
    round_latencies: Vec<u64>,
    response_bytes: u64,
    metrics: PacketEngineMetrics,
}

impl BenchResult {
    fn engine_requests_per_second(&self) -> f64 {
        self.requests as f64 / self.engine_elapsed.as_secs_f64()
    }

    fn end_to_end_requests_per_second(&self) -> f64 {
        self.requests as f64 / self.wall_elapsed.as_secs_f64()
    }

    fn wire_packets_per_second(&self) -> f64 {
        self.requests.saturating_mul(2) as f64 / self.engine_elapsed.as_secs_f64()
    }

    fn response_mib_per_second(&self) -> f64 {
        self.response_bytes as f64 / (1024.0 * 1024.0) / self.engine_elapsed.as_secs_f64()
    }

    fn handshake_flows_per_second(&self) -> f64 {
        self.flows as f64 / self.handshake.as_secs_f64()
    }

    fn percentile_nanos(&self, percentile: u32) -> u64 {
        percentile_nearest_rank(&self.round_latencies, percentile)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure!(cli.flows > 0, "--flows must be positive");
    ensure!(
        cli.requests_per_flow > 0,
        "--requests-per-flow must be positive"
    );
    ensure!(cli.batch_size > 0, "--batch-size must be positive");
    ensure!(
        cli.flows <= cli.flow_capacity,
        "--flow-capacity must be at least --flows"
    );
    ensure!(
        cli.flows <= 1_000_000,
        "--flows is capped at one million to keep the synthetic address space sane"
    );

    let packet_config = PacketEngineConfig {
        max_batch_size: cli.batch_size,
        flow_capacity: cli.flow_capacity,
        flow_probe_limit: cli.flow_probe_limit,
        listen_port: cli.listen_port,
    };
    let settings = BenchSettings {
        flows: cli.flows,
        requests_per_flow: cli.requests_per_flow,
        warmup_requests_per_flow: cli.warmup_requests_per_flow,
        listen_port: cli.listen_port,
    };
    let mut results = Vec::new();

    if matches!(cli.backend, BackendChoice::Cpu | BackendChoice::Both) {
        let engine = CpuPacketEngine::new(packet_config)?;
        results.push(run_benchmark(&engine, settings, None)?);
    }
    if matches!(cli.backend, BackendChoice::Gpu | BackendChoice::Both) {
        let engine = GpuPacketEngine::new(packet_config)?;
        let adapter = Some(engine.adapter_name().to_owned());
        results.push(run_benchmark(&engine, settings, adapter)?);
    }

    if cli.json {
        print_json(&results);
    } else {
        print_human(&results);
    }
    Ok(())
}

fn run_benchmark(
    engine: &impl PacketEngine,
    settings: BenchSettings,
    adapter: Option<String>,
) -> Result<BenchResult> {
    let mut flows = synthetic_flows(settings.flows, settings.listen_port)?;
    let handshake = establish_flows(engine, &mut flows)?;
    let mut responses = Vec::with_capacity(settings.flows);

    for _ in 0..settings.warmup_requests_per_flow {
        run_http_round(engine, &mut flows, &mut responses)?;
    }

    let metrics_before = engine.metrics();
    let wall_started = Instant::now();
    let mut engine_elapsed = Duration::ZERO;
    let mut round_latencies = Vec::with_capacity(settings.requests_per_flow);
    let mut response_bytes = 0_u64;
    for _ in 0..settings.requests_per_flow {
        let round = run_http_round(engine, &mut flows, &mut responses)?;
        engine_elapsed += round.elapsed;
        round_latencies.push(duration_nanos(round.elapsed));
        response_bytes = response_bytes.saturating_add(round.response_bytes);
    }
    let wall_elapsed = wall_started.elapsed();
    let metrics = engine.metrics().saturating_sub(metrics_before);
    close_flows(engine, &flows)?;
    round_latencies.sort_unstable();
    let requests = u64::try_from(settings.flows)
        .ok()
        .and_then(|flows| {
            u64::try_from(settings.requests_per_flow)
                .ok()
                .and_then(|rounds| flows.checked_mul(rounds))
        })
        .context("benchmark request count overflow")?;

    Ok(BenchResult {
        backend: engine.name(),
        adapter,
        flows: settings.flows,
        requests,
        handshake,
        engine_elapsed,
        wall_elapsed,
        round_latencies,
        response_bytes,
        metrics,
    })
}

fn synthetic_flows(count: usize, listen_port: u16) -> Result<Vec<FlowCursor>> {
    let mut flows = Vec::with_capacity(count);
    for index in 0..count {
        let address_offset = u32::try_from(index / 60_000).context("flow address overflow")?;
        let port_offset = u16::try_from(index % 60_000).context("flow port overflow")?;
        flows.push(FlowCursor {
            key: FlowKey {
                src_ip: 0x0a4d0002_u32.wrapping_add(address_offset),
                dst_ip: SERVER_IP,
                src_port: 1024_u16.saturating_add(port_offset),
                dst_port: listen_port,
            },
            client_next: 1_000_000_u32.wrapping_add((index as u32).wrapping_mul(4096)),
            server_next: 0,
        });
    }
    Ok(flows)
}

fn establish_flows(engine: &impl PacketEngine, flows: &mut [FlowCursor]) -> Result<Duration> {
    let syns = flows
        .iter()
        .map(|flow| {
            build_ipv4_tcp_packet(TcpPacketSpec {
                key: flow.key,
                seq: flow.client_next,
                ack: 0,
                flags: TCP_SYN,
                payload: &[],
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let started = Instant::now();
    let syn_acks = engine.process_batch(&syns)?;
    let elapsed = started.elapsed();
    ensure!(syn_acks.len() == flows.len(), "SYN output count mismatch");

    for (flow, response) in flows.iter_mut().zip(syn_acks) {
        let response = response.context("SYN did not produce SYN-ACK")?;
        validate_ipv4_tcp_checksums(&response)?;
        let tcp = parse_ipv4_tcp(&response).context("SYN-ACK did not parse")?;
        ensure!(
            tcp.flags == TCP_SYN | TCP_ACK,
            "handshake response was not SYN-ACK"
        );
        ensure!(
            tcp.ack == flow.client_next.wrapping_add(1),
            "SYN-ACK acknowledged the wrong sequence"
        );
        flow.client_next = flow.client_next.wrapping_add(1);
        flow.server_next = tcp.seq.wrapping_add(1);
    }

    let acknowledgements = flows
        .iter()
        .map(|flow| client_packet(*flow, TCP_ACK, &[]))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        engine
            .process_batch(&acknowledgements)?
            .iter()
            .all(Option::is_none),
        "handshake ACK emitted unexpected packets"
    );
    Ok(elapsed)
}

struct RoundResult {
    elapsed: Duration,
    response_bytes: u64,
}

fn run_http_round(
    engine: &impl PacketEngine,
    flows: &mut [FlowCursor],
    responses: &mut Vec<Option<RawPacket>>,
) -> Result<RoundResult> {
    let requests = flows
        .iter()
        .map(|flow| client_packet(*flow, TCP_ACK | TCP_PSH, DEFAULT_REQUEST))
        .collect::<Result<Vec<_>>>()?;
    let started = Instant::now();
    engine.process_batch_into(&requests, responses)?;
    let elapsed = started.elapsed();
    ensure!(responses.len() == flows.len(), "HTTP output count mismatch");

    let mut response_bytes = 0_u64;
    for (flow, response) in flows.iter_mut().zip(responses.iter()) {
        let response = response
            .as_ref()
            .context("HTTP request produced no packet")?;
        validate_ipv4_tcp_checksums(response)?;
        let tcp = parse_ipv4_tcp(response).context("HTTP response did not parse")?;
        ensure!(
            tcp.seq == flow.server_next,
            "HTTP response sequence drifted"
        );
        ensure!(
            tcp.ack == flow.client_next.wrapping_add(DEFAULT_REQUEST.len() as u32),
            "HTTP response acknowledgement drifted"
        );
        ensure!(
            tcp.payload.starts_with(b"HTTP/1.1 200 OK\r\n"),
            "HTTP response status was not 200"
        );
        ensure!(
            tcp.payload.ends_with(b"\r\n\r\nHello, World!\n"),
            "HTTP response body was not the plaintext benchmark body"
        );
        ensure!(
            tcp.payload
                .windows(engine.name().len())
                .any(|window| window == engine.name().as_bytes()),
            "HTTP response lied about packet backend"
        );
        flow.client_next = flow.client_next.wrapping_add(DEFAULT_REQUEST.len() as u32);
        flow.server_next = flow.server_next.wrapping_add(tcp.payload.len() as u32);
        response_bytes = response_bytes.saturating_add(tcp.payload.len() as u64);
    }

    Ok(RoundResult {
        elapsed,
        response_bytes,
    })
}

fn close_flows(engine: &impl PacketEngine, flows: &[FlowCursor]) -> Result<()> {
    let fins = flows
        .iter()
        .map(|flow| client_packet(*flow, TCP_ACK | TCP_FIN, &[]))
        .collect::<Result<Vec<_>>>()?;
    let responses = engine.process_batch(&fins)?;
    ensure!(responses.len() == flows.len(), "FIN output count mismatch");
    for (flow, response) in flows.iter().zip(responses) {
        let response = response.context("FIN produced no ACK")?;
        let tcp = parse_ipv4_tcp(&response).context("FIN ACK did not parse")?;
        ensure!(tcp.flags == TCP_ACK, "FIN response was not an ACK");
        ensure!(
            tcp.ack == flow.client_next.wrapping_add(1),
            "FIN ACK used the wrong acknowledgement"
        );
    }
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

fn percentile_nearest_rank(sorted: &[u64], percentile: u32) -> u64 {
    assert!(!sorted.is_empty());
    assert!(percentile <= 100);
    let last = sorted.len() - 1;
    let index = (last * percentile as usize + 50) / 100;
    sorted[index]
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn nanos_to_millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

fn print_human(results: &[BenchResult]) {
    println!("gput raw-packet championship");
    println!(
        "backend     flows     requests   engine req/s   end-to-end req/s   wire pkt/s   MiB/s   p50 round   p99 round   dispatch fill"
    );
    for result in results {
        let fill = result.metrics.packets as f64 / result.metrics.dispatches.max(1) as f64;
        println!(
            "{:<11} {:>7} {:>12} {:>14.0} {:>18.0} {:>12.0} {:>7.1} {:>10.3}ms {:>10.3}ms {:>13.2}",
            result.backend,
            result.flows,
            result.requests,
            result.engine_requests_per_second(),
            result.end_to_end_requests_per_second(),
            result.wire_packets_per_second(),
            result.response_mib_per_second(),
            nanos_to_millis(result.percentile_nanos(50)),
            nanos_to_millis(result.percentile_nanos(99)),
            fill,
        );
        println!(
            "  handshake: {:.0} flows/s | engine: {:.6}s | wall: {:.6}s | dispatches: {}{}",
            result.handshake_flows_per_second(),
            result.engine_elapsed.as_secs_f64(),
            result.wall_elapsed.as_secs_f64(),
            result.metrics.dispatches,
            result
                .adapter
                .as_deref()
                .map(|adapter| format!(" | adapter: {adapter}"))
                .unwrap_or_default(),
        );
        if result.metrics.readback_nanos > 0 {
            let packets = result.metrics.packets.max(1) as f64;
            println!(
                "  profile ns/packet: schedule {:.1} | pack {:.1} | upload {:.1} | submit {:.1} | GPU+readback {:.1} | decode {:.1}",
                result.metrics.schedule_nanos as f64 / packets,
                result.metrics.pack_nanos as f64 / packets,
                result.metrics.upload_nanos as f64 / packets,
                result.metrics.submit_nanos as f64 / packets,
                result.metrics.readback_nanos as f64 / packets,
                result.metrics.decode_nanos as f64 / packets,
            );
        }
    }
    if let (Some(cpu), Some(gpu)) = (
        results.iter().find(|result| result.backend == "cpu-packet"),
        results.iter().find(|result| result.backend == "gpu-packet"),
    ) {
        println!(
            "GPU / single-threaded CPU reference: {:.3}x engine throughput, {:.3}x end-to-end throughput",
            gpu.engine_requests_per_second() / cpu.engine_requests_per_second(),
            gpu.end_to_end_requests_per_second() / cpu.end_to_end_requests_per_second(),
        );
    }
}

fn print_json(results: &[BenchResult]) {
    print!("{{\"results\":[");
    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            print!(",");
        }
        print!(
            concat!(
                "{{",
                "\"backend\":\"{}\",",
                "\"adapter\":{},",
                "\"flows\":{},",
                "\"requests\":{},",
                "\"handshake_flows_per_second\":{:.3},",
                "\"engine_seconds\":{:.9},",
                "\"wall_seconds\":{:.9},",
                "\"engine_requests_per_second\":{:.3},",
                "\"end_to_end_requests_per_second\":{:.3},",
                "\"wire_packets_per_second\":{:.3},",
                "\"response_mib_per_second\":{:.3},",
                "\"p50_round_nanos\":{},",
                "\"p99_round_nanos\":{},",
                "\"dispatches\":{},",
                "\"packets\":{},",
                "\"schedule_nanos\":{},",
                "\"pack_nanos\":{},",
                "\"upload_nanos\":{},",
                "\"submit_nanos\":{},",
                "\"readback_nanos\":{},",
                "\"decode_nanos\":{}",
                "}}"
            ),
            result.backend,
            result
                .adapter
                .as_ref()
                .map(|adapter| format!("\"{}\"", adapter.replace('"', "\\\"")))
                .unwrap_or_else(|| "null".to_owned()),
            result.flows,
            result.requests,
            result.handshake_flows_per_second(),
            result.engine_elapsed.as_secs_f64(),
            result.wall_elapsed.as_secs_f64(),
            result.engine_requests_per_second(),
            result.end_to_end_requests_per_second(),
            result.wire_packets_per_second(),
            result.response_mib_per_second(),
            result.percentile_nanos(50),
            result.percentile_nanos(99),
            result.metrics.dispatches,
            result.metrics.packets,
            result.metrics.schedule_nanos,
            result.metrics.pack_nanos,
            result.metrics.upload_nanos,
            result.metrics.submit_nanos,
            result.metrics.readback_nanos,
            result.metrics.decode_nanos,
        );
    }
    println!("]}}");
}
