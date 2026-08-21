use anyhow::Result;
use tracing::info;

use crate::{
    Router,
    batcher::{BatcherConfig, spawn_batcher},
    config::ServerConfig,
    network,
    processor::{ProcessorLimits, create_processor_with_router},
};

pub async fn serve(config: ServerConfig, router: Router) -> Result<()> {
    config.validate()?;

    let processor = create_processor_with_router(
        config.backend,
        ProcessorLimits {
            max_batch_size: config.batch_size,
            max_request_bytes: config.max_request_bytes,
            response_slot_bytes: config.response_slot_bytes,
        },
        router,
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
        response_slot_bytes = config.response_slot_bytes,
        "starting the unnecessarily accelerated web server"
    );

    network::serve(config, batcher).await
}
