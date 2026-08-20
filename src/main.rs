use anyhow::Result;
use clap::Parser;
use gput::{
    batcher::{BatcherConfig, spawn_batcher},
    config::{Cli, ServerConfig},
    network,
    processor::{ProcessorLimits, create_processor},
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gput=info")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = ServerConfig::try_from(Cli::parse())?;
    let processor = create_processor(
        config.backend,
        ProcessorLimits {
            max_batch_size: config.batch_size,
            max_request_bytes: config.max_request_bytes,
            response_slot_bytes: config.response_slot_bytes,
        },
    )?;
    let backend = processor.name();
    let (batcher, _batch_worker) = spawn_batcher(
        processor,
        BatcherConfig {
            max_batch_size: config.batch_size,
            max_batch_wait: config.batch_wait,
            queue_depth: config.queue_depth,
        },
    )?;

    info!(
        backend,
        batch_size = config.batch_size,
        batch_wait_micros = config.batch_wait.as_micros(),
        max_request_bytes = config.max_request_bytes,
        "starting the unnecessarily accelerated web server"
    );

    network::serve(config, batcher).await
}
