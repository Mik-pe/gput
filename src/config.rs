use std::{net::SocketAddr, time::Duration};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};

use crate::protocol::MAX_GPU_RESPONSE_BYTES;

const MAX_TOTAL_BUFFER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendChoice {
    Auto,
    Cpu,
    Gpu,
}

#[derive(Debug, Parser)]
#[command(
    name = "gput",
    version,
    about = "An HTTP server that makes the GPU parse requests for no defensible reason"
)]
pub struct Cli {
    #[arg(long, env = "GPUT_BIND", default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    #[arg(long, env = "GPUT_BACKEND", value_enum, default_value_t = BackendChoice::Auto)]
    pub backend: BackendChoice,

    #[arg(long, env = "GPUT_BATCH_SIZE", default_value_t = 256)]
    pub batch_size: usize,

    #[arg(long, env = "GPUT_BATCH_WAIT_MICROS", default_value_t = 50)]
    pub batch_wait_micros: u64,

    #[arg(long, env = "GPUT_QUEUE_DEPTH", default_value_t = 8_192)]
    pub queue_depth: usize,

    #[arg(long, env = "GPUT_MAX_REQUEST_BYTES", default_value_t = 4_096)]
    pub max_request_bytes: usize,

    #[arg(long, env = "GPUT_RESPONSE_SLOT_BYTES", default_value_t = 256)]
    pub response_slot_bytes: usize,

    #[arg(long, env = "GPUT_MAX_CONNECTIONS", default_value_t = 4_096)]
    pub max_connections: usize,

    #[arg(long, env = "GPUT_READ_TIMEOUT_SECS", default_value_t = 5)]
    pub read_timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub backend: BackendChoice,
    pub batch_size: usize,
    pub batch_wait: Duration,
    pub queue_depth: usize,
    pub max_request_bytes: usize,
    pub response_slot_bytes: usize,
    pub max_connections: usize,
    pub read_timeout: Duration,
}

impl TryFrom<Cli> for ServerConfig {
    type Error = anyhow::Error;

    fn try_from(cli: Cli) -> Result<Self> {
        let config = Self {
            bind: cli.bind,
            backend: cli.backend,
            batch_size: cli.batch_size,
            batch_wait: Duration::from_micros(cli.batch_wait_micros),
            queue_depth: cli.queue_depth,
            max_request_bytes: cli.max_request_bytes,
            response_slot_bytes: cli.response_slot_bytes,
            max_connections: cli.max_connections,
            read_timeout: Duration::from_secs(cli.read_timeout_secs),
        };

        config.validate()?;
        Ok(config)
    }
}

impl ServerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.batch_size == 0 {
            bail!("--batch-size must be greater than zero");
        }

        if self.batch_size > u32::MAX as usize {
            bail!("--batch-size must fit in a u32");
        }

        if self.batch_wait.is_zero() {
            bail!("--batch-wait-micros must be greater than zero");
        }

        if self.queue_depth < self.batch_size {
            bail!("--queue-depth must be at least --batch-size");
        }

        if self.max_request_bytes < 16 {
            bail!("--max-request-bytes must be at least 16");
        }

        if self.response_slot_bytes < MAX_GPU_RESPONSE_BYTES {
            bail!(
                "--response-slot-bytes must be at least {MAX_GPU_RESPONSE_BYTES} for the built-in routes"
            );
        }

        if self.max_connections == 0 {
            bail!("--max-connections must be greater than zero");
        }

        if self.read_timeout.is_zero() {
            bail!("--read-timeout-secs must be greater than zero");
        }

        let request_stride = round_up_to_word(self.max_request_bytes)?;
        let response_stride = round_up_to_word(self.response_slot_bytes)?;
        let request_buffer_bytes = self
            .batch_size
            .checked_mul(request_stride)
            .ok_or_else(|| anyhow::anyhow!("request buffer size overflow"))?;
        let response_buffer_bytes = self
            .batch_size
            .checked_mul(response_stride)
            .ok_or_else(|| anyhow::anyhow!("response buffer size overflow"))?;

        if request_buffer_bytes > MAX_TOTAL_BUFFER_BYTES {
            bail!(
                "configured request slots require {request_buffer_bytes} bytes; limit is {MAX_TOTAL_BUFFER_BYTES}"
            );
        }

        if response_buffer_bytes > MAX_TOTAL_BUFFER_BYTES {
            bail!(
                "configured response slots require {response_buffer_bytes} bytes; limit is {MAX_TOTAL_BUFFER_BYTES}"
            );
        }

        Ok(())
    }
}

fn round_up_to_word(value: usize) -> Result<usize> {
    value
        .checked_add(3)
        .map(|rounded| rounded & !3)
        .ok_or_else(|| anyhow::anyhow!("buffer stride overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ServerConfig {
        ServerConfig {
            bind: "127.0.0.1:8080".parse().expect("valid address"),
            backend: BackendChoice::Auto,
            batch_size: 256,
            batch_wait: Duration::from_micros(50),
            queue_depth: 8_192,
            max_request_bytes: 4_096,
            response_slot_bytes: 256,
            max_connections: 4_096,
            read_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn accepts_default_shape() {
        valid_config().validate().expect("default config is valid");
    }

    #[test]
    fn rejects_queue_smaller_than_batch() {
        let mut config = valid_config();
        config.queue_depth = config.batch_size - 1;

        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_response_slots_that_cannot_hold_builtins() {
        let mut config = valid_config();
        config.response_slot_bytes = MAX_GPU_RESPONSE_BYTES - 1;

        assert!(config.validate().is_err());
    }
}
