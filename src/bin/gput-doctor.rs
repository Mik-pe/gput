use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use gput::{
    packet::{
        FlowKey, GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket, TCP_ACK, TCP_PSH,
        TCP_SYN, TcpPacketSpec, build_ipv4_tcp_packet, parse_ipv4_tcp,
        validate_ipv4_tcp_checksums,
    },
    processor::{GpuProcessor, Processor, ProcessorLimits},
};

const DOCTOR_PORT: u16 = 18_089;
const CLIENT_ISN: u32 = 0x1234_5678;

#[derive(Debug, Parser)]
#[command(
    name = "gput-doctor",
    version,
    about = "Check whether this machine can run gput's HTTP and raw-packet compute paths",
    after_help = "A software adapter proves correctness. It does not grant benchmark bragging rights."
)]
struct Cli {
    #[arg(long, help = "Fail when wgpu selected a CPU/software adapter")]
    require_hardware_gpu: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("gput doctor");
    println!("  treatment: storage buffers, bit shifts, atomics, and stubbornness\n");

    let adapter = discover_adapter();
    let mut failures = Vec::new();
    let mut software_adapter = false;

    match &adapter {
        Ok(info) => {
            software_adapter = matches!(info.device_type, wgpu::DeviceType::Cpu);
            println!(
                "  ✓ adapter: {} ({:?}, {:?})",
                info.name, info.backend, info.device_type
            );
            if software_adapter {
                println!(
                    "    note: this is a CPU/software adapter, excellent for correctness and terrible for victory laps"
                );
            }
        }
        Err(error) => {
            println!("  ✗ adapter discovery: {error:#}");
            failures.push("adapter discovery".to_owned());
        }
    }

    match check_http_shader() {
        Ok(elapsed) => println!(
            "  ✓ HTTP compute: parsed, routed, and rendered /health in {:.2} ms",
            elapsed.as_secs_f64() * 1_000.0
        ),
        Err(error) => {
            println!("  ✗ HTTP compute: {error:#}");
            failures.push("HTTP compute".to_owned());
        }
    }

    match check_packet_shader() {
        Ok((elapsed, adapter_name)) => println!(
            "  ✓ packet compute: SYN, ACK, GET /health, checksums, and response on {adapter_name} in {:.2} ms",
            elapsed.as_secs_f64() * 1_000.0
        ),
        Err(error) => {
            println!("  ✗ packet compute: {error:#}");
            failures.push("packet compute".to_owned());
        }
    }

    if cli.require_hardware_gpu && software_adapter {
        failures.push("hardware GPU requirement".to_owned());
        println!("  ✗ hardware requirement: selected adapter reports itself as a CPU");
    }

    if !failures.is_empty() {
        bail!(
            "doctor found {} broken organ(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }

    println!("\nprognosis: the GPU path is real; whether it is fast is now the benchmark's problem");
    println!("  next: cargo serve --backend gpu");
    println!("        cargo prove-gpu");
    println!("        cargo packet-bench --backend both");
    println!("        sudo cargo packetd --backend gpu");
    Ok(())
}

fn discover_adapter() -> Result<wgpu::AdapterInfo> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .context("wgpu found no compute adapter")?;
    Ok(adapter.get_info())
}

fn check_http_shader() -> Result<Duration> {
    let started = Instant::now();
    let mut processor = GpuProcessor::new(ProcessorLimits {
        max_batch_size: 4,
        max_request_bytes: 1_024,
        response_slot_bytes: 512,
    })?;
    let request: &[u8] = b"GET /health HTTP/1.1\r\nHost: doctor\r\nConnection: close\r\n\r\n";
    let mut responses = processor.process_batch(&[request])?;
    ensure!(responses.len() == 1, "GPU returned the wrong response count");
    let response = responses.pop().expect("response count was checked");
    ensure!(
        response.starts_with(b"HTTP/1.1 200 OK\r\n"),
        "GPU HTTP response did not return 200"
    );
    ensure!(
        contains_bytes(&response, b"X-Gput-Backend: gpu\r\n"),
        "GPU HTTP response did not identify the real backend"
    );
    ensure!(
        response.ends_with(b"\r\n\r\nok\n"),
        "GPU HTTP response body was not health output"
    );
    Ok(started.elapsed())
}

fn check_packet_shader() -> Result<(Duration, String)> {
    let started = Instant::now();
    let engine = GpuPacketEngine::new(PacketEngineConfig {
        max_batch_size: 8,
        flow_capacity: 64,
        flow_probe_limit: 32,
        listen_port: DOCTOR_PORT,
    })?;
    let adapter_name = engine.adapter_name().to_owned();
    let key = FlowKey {
        src_ip: 0x0a4d_0002,
        dst_ip: 0x0a4d_0001,
        src_port: 50_001,
        dst_port: DOCTOR_PORT,
    };

    let syn = build_ipv4_tcp_packet(TcpPacketSpec {
        key,
        seq: CLIENT_ISN,
        ack: 0,
        flags: TCP_SYN,
        payload: &[],
    })?;
    let syn_ack = one_packet(&engine, syn, "SYN")?;
    validate_ipv4_tcp_checksums(&syn_ack)?;
    let syn_tcp = parse_ipv4_tcp(&syn_ack).context("SYN-ACK did not parse")?;
    ensure!(
        syn_tcp.flags == TCP_SYN | TCP_ACK,
        "packet shader did not produce SYN-ACK"
    );
    ensure!(
        syn_tcp.ack == CLIENT_ISN.wrapping_add(1),
        "packet shader acknowledged the wrong SYN sequence"
    );

    let client_next = CLIENT_ISN.wrapping_add(1);
    let server_next = syn_tcp.seq.wrapping_add(1);
    let ack = build_ipv4_tcp_packet(TcpPacketSpec {
        key,
        seq: client_next,
        ack: server_next,
        flags: TCP_ACK,
        payload: &[],
    })?;
    ensure!(
        engine.process_batch(&[ack])? == [None],
        "pure handshake ACK unexpectedly emitted a packet"
    );

    let request = b"GET /health HTTP/1.1\r\nHost: doctor\r\n\r\n";
    let get = build_ipv4_tcp_packet(TcpPacketSpec {
        key,
        seq: client_next,
        ack: server_next,
        flags: TCP_ACK | TCP_PSH,
        payload: request,
    })?;
    let response = one_packet(&engine, get, "GET /health")?;
    validate_ipv4_tcp_checksums(&response)?;
    let tcp = parse_ipv4_tcp(&response).context("packet response did not parse")?;
    ensure!(
        tcp.ack == client_next.wrapping_add(request.len() as u32),
        "packet response acknowledged the wrong request sequence"
    );
    ensure!(
        tcp.payload.starts_with(b"HTTP/1.1 200 OK\r\n"),
        "packet response did not return 200"
    );
    ensure!(
        tcp.payload.ends_with(b"\r\n\r\nok\n"),
        "packet response body was not health output"
    );
    ensure!(
        contains_bytes(tcp.payload, b"X-Gput-Backend: gpu-packet\r\n"),
        "packet response did not identify the GPU packet backend"
    );

    Ok((started.elapsed(), adapter_name))
}

fn one_packet(
    engine: &impl PacketEngine,
    input: RawPacket,
    label: &str,
) -> Result<RawPacket> {
    let mut output = engine.process_batch(&[input])?;
    output
        .pop()
        .flatten()
        .with_context(|| format!("packet shader emitted no response for {label}"))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
