use std::{net::SocketAddr, time::Duration};

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};

use crate::protocol::MIN_GPU_RESPONSE_SLOT_BYTES;

const MAX_TOTAL_BUFFER_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_BATCH_SIZE: usize = 256;
const DEFAULT_BATCH_WAIT_MICROS: u64 = 50;
const DEFAULT_QUEUE_DEPTH: usize = 8_192;
const DEFAULT_MAX_REQUEST_BYTES: usize = 4_096;
const DEFAULT_RESPONSE_SLOT_BYTES: usize = 512;
const DEFAULT_MAX_CONNECTIONS: usize = 4_096;
const DEFAULT_READ_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum BackendChoice {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Debug, Parser)]
#[command(
    name = "gput",
    version,
    about = "An HTTP server that makes the GPU parse and route requests for no defensible reason"
)]
pub struct Cli {
    #[arg(long, env = "GPUT_BIND", default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    #[arg(long, env = "GPUT_BACKEND", value_enum, default_value_t = BackendChoice::Auto)]
    pub backend: BackendChoice,

    #[arg(long, env = "GPUT_BATCH_SIZE", default_value_t = DEFAULT_BATCH_SIZE)]
    pub batch_size: usize,

    #[arg(
        long,
        env = "GPUT_BATCH_WAIT_MICROS",
        default_value_t = DEFAULT_BATCH_WAIT_MICROS
    )]
    pub batch_wait_micros: u64,

    #[arg(long, env = "GPUT_QUEUE_DEPTH", default_value_t = DEFAULT_QUEUE_DEPTH)]
    pub queue_depth: usize,

    #[arg(
        long,
        env = "GPUT_MAX_REQUEST_BYTES",
        default_value_t = DEFAULT_MAX_REQUEST_BYTES
    )]
    pub max_request_bytes: usize,

    #[arg(
        long,
        env = "GPUT_RESPONSE_SLOT_BYTES",
        default_value_t = DEFAULT_RESPONSE_SLOT_BYTES
    )]
    pub response_slot_bytes: usize,

    #[arg(
        long,
        env = "GPUT_MAX_CONNECTIONS",
        default_value_t = DEFAULT_MAX_CONNECTIONS
    )]
    pub max_connections: usize,

    #[arg(
        long,
        env = "GPUT_READ_TIMEOUT_SECS",
        default_value_t = DEFAULT_READ_TIMEOUT_SECS
    )]
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            backend: BackendChoice::Auto,
            batch_size: DEFAULT_BATCH_SIZE,
            batch_wait: Duration::from_micros(DEFAULT_BATCH_WAIT_MICROS),
            queue_depth: DEFAULT_QUEUE_DEPTH,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            response_slot_bytes: DEFAULT_RESPONSE_SLOT_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            read_timeout: Duration::from_secs(DEFAULT_READ_TIMEOUT_SECS),
        }
    }
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

        if self.response_slot_bytes < MIN_GPU_RESPONSE_SLOT_BYTES {
            bail!("--response-slot-bytes must be at least {MIN_GPU_RESPONSE_SLOT_BYTES}");
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

    #[test]
    fn accepts_default_shape() {
        ServerConfig::default()
            .validate()
            .expect("default config is valid");
    }

    #[test]
    fn rejects_queue_smaller_than_batch() {
        let mut config = ServerConfig::default();
        config.queue_depth = config.batch_size - 1;

        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_response_slots_below_the_absolute_minimum() {
        let config = ServerConfig {
            response_slot_bytes: MIN_GPU_RESPONSE_SLOT_BYTES - 1,
            ..ServerConfig::default()
        };

        assert!(config.validate().is_err());
    }
}
