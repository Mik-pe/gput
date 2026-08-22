use std::{
    io::ErrorKind,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use gput::packet::{CpuPacketEngine, GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum PacketBackendChoice {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Debug, Parser)]
#[command(
    name = "gput-packetd",
    version,
    about = "Serve gput's tiny TCP fast path through a batched portable L3 TUN interface",
    after_help = "The selected engine always identifies itself in X-Gput-Backend. No fake GPU moustaches."
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = PacketBackendChoice::Auto)]
    backend: PacketBackendChoice,

    #[arg(long, default_value = "10.77.0.1")]
    local: Ipv4Addr,

    #[arg(long, default_value = "10.77.0.2")]
    peer: Ipv4Addr,

    #[arg(long, default_value = "255.255.255.0")]
    netmask: Ipv4Addr,

    #[arg(long, default_value_t = 1500)]
    mtu: u16,

    #[arg(long, default_value_t = 8080)]
    listen_port: u16,

    #[arg(
        long,
        visible_alias = "gpu-batch-capacity",
        default_value_t = 256,
        help = "Maximum independent packets offered to one engine call"
    )]
    batch_capacity: usize,

    #[arg(long, default_value_t = 50)]
    batch_wait_micros: u64,

    #[arg(long, default_value_t = 4096)]
    flow_capacity: usize,

    #[arg(long, default_value_t = 32)]
    flow_probe_limit: usize,

    #[arg(long, default_value_t = 5)]
    stats_interval_secs: u64,
}

struct SelectedEngine {
    engine: Box<dyn PacketEngine>,
    adapter: Option<String>,
    fallback_reason: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gput=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    let config = PacketEngineConfig {
        max_batch_size: cli.batch_capacity,
        flow_capacity: cli.flow_capacity,
        flow_probe_limit: cli.flow_probe_limit,
        listen_port: cli.listen_port,
    };
    let selected = select_engine(cli.backend, config)?;
    let engine_name = selected.engine.name();

    if let Some(reason) = &selected.fallback_reason {
        eprintln!("GPU declined the invitation: {reason:#}");
        eprintln!("Falling back to the CPU reference without pretending otherwise.");
    }

    let mut tun_config = tun::Configuration::default();
    tun_config
        .address(cli.local)
        .destination(cli.peer)
        .netmask(cli.netmask)
        .mtu(cli.mtu)
        .up();

    #[cfg(target_os = "linux")]
    tun_config.platform_config(|config| {
        config.ensure_root_privileges(true);
    });

    let device = tun::create(&tun_config).context("failed to create TUN interface")?;
    eprintln!("gput-packetd");
    eprintln!("  engine:  {engine_name}");
    eprintln!(
        "  adapter: {}",
        selected.adapter.as_deref().unwrap_or("host CPU")
    );
    eprintln!("  routes:  GET /plaintext, GET /health");
    eprintln!(
        "  try:     curl --noproxy '*' http://{}:{}/plaintext",
        cli.peer, cli.listen_port
    );
    eprintln!("  recipe:  raw packets, bounded buffers, and unreasonable stubbornness");

    let mut buffer = vec![0_u8; usize::from(cli.mtu).max(2048)];
    let mut batch = Vec::with_capacity(cli.batch_capacity);
    let mut spare_packets = Vec::with_capacity(cli.batch_capacity);
    let batch_wait = Duration::from_micros(cli.batch_wait_micros);
    let stats_interval = Duration::from_secs(cli.stats_interval_secs);
    let started = Instant::now();
    let mut last_stats = started;
    let mut packets_in = 0_u64;
    let mut packets_out = 0_u64;
    let mut batches = 0_u64;
    let mut peak_batch = 0_usize;
    let mut receive_allocations = 0_u64;
    let mut receive_reuses = 0_u64;

    loop {
        spare_packets.append(&mut batch);
        let first_len = device
            .recv(&mut buffer)
            .context("failed to read TUN packet")?;
        if first_len == 0 {
            continue;
        }
        if push_received_packet(&mut batch, &mut spare_packets, &buffer[..first_len])? {
            receive_reuses = receive_reuses.saturating_add(1);
        } else {
            receive_allocations = receive_allocations.saturating_add(1);
        }

        let deadline = Instant::now() + batch_wait;
        while batch.len() < cli.batch_capacity {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match device.recv_timeout(&mut buffer, remaining) {
                Ok(0) => break,
                Ok(packet_len) => {
                    if push_received_packet(&mut batch, &mut spare_packets, &buffer[..packet_len])?
                    {
                        receive_reuses = receive_reuses.saturating_add(1);
                    } else {
                        receive_allocations = receive_allocations.saturating_add(1);
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) =>
                {
                    break;
                }
                Err(error) => return Err(error).context("failed to batch TUN packets"),
            }
        }

        packets_in = packets_in.saturating_add(batch.len() as u64);
        batches = batches.saturating_add(1);
        peak_batch = peak_batch.max(batch.len());
        let mut send_response = |_source_index: usize, response: Option<&[u8]>| -> Result<()> {
            let Some(response) = response else {
                return Ok(());
            };
            let sent = device
                .send(response)
                .context("failed to inject borrowed engine response into TUN")?;
            ensure!(
                sent == response.len(),
                "TUN accepted {sent} of {} response bytes",
                response.len()
            );
            packets_out = packets_out.saturating_add(1);
            Ok(())
        };
        selected
            .engine
            .process_batch_to(&batch, &mut send_response)?;

        if !stats_interval.is_zero() && last_stats.elapsed() >= stats_interval {
            let elapsed = started.elapsed().as_secs_f64();
            let metrics = selected.engine.metrics();
            let average_batch = packets_in as f64 / batches.max(1) as f64;
            let packets_per_dispatch = metrics.packets as f64 / metrics.dispatches.max(1) as f64;
            eprintln!(
                "gput packet stats: engine={engine_name} in={packets_in} out={packets_out} pps={:.0} batches={batches} avg_batch={average_batch:.2} peak_batch={peak_batch} rx_allocations={receive_allocations} rx_reuses={receive_reuses} engine_dispatches={} packets/dispatch={packets_per_dispatch:.2}",
                packets_in as f64 / elapsed.max(f64::EPSILON),
                metrics.dispatches,
            );
            last_stats = Instant::now();
        }
    }
}

fn push_received_packet(
    batch: &mut Vec<RawPacket>,
    spare_packets: &mut Vec<RawPacket>,
    bytes: &[u8],
) -> Result<bool> {
    if let Some(mut packet) = spare_packets.pop() {
        packet.replace_from_slice(bytes)?;
        batch.push(packet);
        Ok(true)
    } else {
        batch.push(RawPacket::copy_from_slice(bytes)?);
        Ok(false)
    }
}

fn select_engine(
    choice: PacketBackendChoice,
    config: PacketEngineConfig,
) -> Result<SelectedEngine> {
    match choice {
        PacketBackendChoice::Cpu => {
            let engine = CpuPacketEngine::new(config)?;
            Ok(SelectedEngine {
                engine: Box::new(engine),
                adapter: None,
                fallback_reason: None,
            })
        }
        PacketBackendChoice::Gpu => select_gpu(config, None),
        PacketBackendChoice::Auto => match GpuPacketEngine::new(config) {
            Ok(engine) => Ok(SelectedEngine {
                adapter: Some(engine.adapter_name().to_owned()),
                engine: Box::new(engine),
                fallback_reason: None,
            }),
            Err(error) => {
                let engine = CpuPacketEngine::new(config)?;
                Ok(SelectedEngine {
                    engine: Box::new(engine),
                    adapter: None,
                    fallback_reason: Some(format!("{error:#}")),
                })
            }
        },
    }
}

fn select_gpu(
    config: PacketEngineConfig,
    fallback_reason: Option<String>,
) -> Result<SelectedEngine> {
    let engine = GpuPacketEngine::new(config)?;
    Ok(SelectedEngine {
        adapter: Some(engine.adapter_name().to_owned()),
        engine: Box::new(engine),
        fallback_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_cpu_mode_never_grows_a_fake_gpu_moustache() {
        let selected = select_engine(
            PacketBackendChoice::Cpu,
            PacketEngineConfig {
                max_batch_size: 4,
                flow_capacity: 64,
                flow_probe_limit: 16,
                listen_port: 8080,
            },
        )
        .expect("CPU packet engine initializes");

        assert_eq!(selected.engine.name(), "cpu-packet");
        assert!(selected.adapter.is_none());
        assert!(selected.fallback_reason.is_none());
    }
}
