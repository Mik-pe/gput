use std::{
    io::ErrorKind,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use gput::packet::{GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket};

#[derive(Debug, Parser)]
#[command(
    name = "gput-packetd",
    version,
    about = "Serve the GPU TCP fast path through a batched portable L3 TUN interface"
)]
struct Cli {
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

    #[arg(long, default_value_t = 256)]
    gpu_batch_capacity: usize,

    #[arg(long, default_value_t = 50)]
    batch_wait_micros: u64,

    #[arg(long, default_value_t = 4096)]
    flow_capacity: usize,

    #[arg(long, default_value_t = 32)]
    flow_probe_limit: usize,

    #[arg(long, default_value_t = 5)]
    stats_interval_secs: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let engine = GpuPacketEngine::new(PacketEngineConfig {
        max_batch_size: cli.gpu_batch_capacity,
        flow_capacity: cli.flow_capacity,
        flow_probe_limit: cli.flow_probe_limit,
        listen_port: cli.listen_port,
    })?;

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
    eprintln!(
        "gput GPU packet path ready on {}: curl --noproxy '*' http://{}:{}/plaintext",
        engine.adapter_name(),
        cli.peer,
        cli.listen_port
    );

    let mut buffer = vec![0_u8; usize::from(cli.mtu).max(2048)];
    let mut batch = Vec::with_capacity(cli.gpu_batch_capacity);
    let batch_wait = Duration::from_micros(cli.batch_wait_micros);
    let stats_interval = Duration::from_secs(cli.stats_interval_secs);
    let started = Instant::now();
    let mut last_stats = started;
    let mut packets_in = 0_u64;
    let mut packets_out = 0_u64;
    let mut batches = 0_u64;
    let mut peak_batch = 0_usize;

    loop {
        batch.clear();
        let first_len = device.recv(&mut buffer).context("failed to read TUN packet")?;
        if first_len == 0 {
            continue;
        }
        batch.push(RawPacket::new(buffer[..first_len].to_vec())?);

        let deadline = Instant::now() + batch_wait;
        while batch.len() < cli.gpu_batch_capacity {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match device.recv_timeout(&mut buffer, remaining) {
                Ok(0) => break,
                Ok(packet_len) => {
                    batch.push(RawPacket::new(buffer[..packet_len].to_vec())?);
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
        for response in engine.process_batch(&batch)?.into_iter().flatten() {
            let sent = device
                .send(response.as_bytes())
                .context("failed to inject GPU response into TUN")?;
            ensure!(
                sent == response.as_bytes().len(),
                "TUN accepted {sent} of {} response bytes",
                response.as_bytes().len()
            );
            packets_out = packets_out.saturating_add(1);
        }

        if !stats_interval.is_zero() && last_stats.elapsed() >= stats_interval {
            let elapsed = started.elapsed().as_secs_f64();
            let metrics = engine.metrics();
            let average_batch = packets_in as f64 / batches.max(1) as f64;
            let packets_per_dispatch = metrics.packets as f64 / metrics.dispatches.max(1) as f64;
            eprintln!(
                "gput packet stats: in={packets_in} out={packets_out} pps={:.0} batches={batches} avg_batch={average_batch:.2} peak_batch={peak_batch} gpu_dispatches={} packets/dispatch={packets_per_dispatch:.2}",
                packets_in as f64 / elapsed.max(f64::EPSILON),
                metrics.dispatches,
            );
            last_stats = Instant::now();
        }
    }
}
