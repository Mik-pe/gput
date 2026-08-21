use std::{
    io::{Read, Write},
    net::Ipv4Addr,
};

use anyhow::{Context, Result};
use clap::Parser;
use gput::packet::{GpuPacketEngine, PacketEngine, PacketEngineConfig, RawPacket};

#[derive(Debug, Parser)]
#[command(
    name = "gput-packetd",
    version,
    about = "Serve the GPU TCP fast path through a portable L3 TUN interface"
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

    #[arg(long, default_value_t = 4096)]
    flow_capacity: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let engine = GpuPacketEngine::new(PacketEngineConfig {
        max_batch_size: cli.gpu_batch_capacity,
        flow_capacity: cli.flow_capacity,
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

    let mut device = tun::create(&tun_config).context("failed to create TUN interface")?;
    eprintln!(
        "gput packet path ready: curl --noproxy '*' http://{}:{}/plaintext",
        cli.peer, cli.listen_port
    );

    let mut buffer = vec![0_u8; usize::from(cli.mtu).max(2048)];
    loop {
        let packet_len = device
            .read(&mut buffer)
            .context("failed to read TUN packet")?;
        if packet_len == 0 {
            continue;
        }

        let packet = RawPacket::new(buffer[..packet_len].to_vec())?;
        for response in engine.process_batch(&[packet])?.into_iter().flatten() {
            device
                .write_all(response.as_bytes())
                .context("failed to inject GPU response into TUN")?;
        }
    }
}
