use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinSet,
    time,
};

const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024;
const HEADER_TERMINATOR: &[u8; 4] = b"\r\n\r\n";

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExpectedBackend {
    Cpu,
    Gpu,
}

impl ExpectedBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "gput-bench",
    version,
    about = "A deliberately small load generator for the deliberately unreasonable gput server"
)]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8080")]
    address: SocketAddr,

    #[arg(long, default_value = "/hello")]
    path: String,

    #[arg(long, default_value_t = 10_000)]
    requests: u64,

    #[arg(long, default_value_t = 128)]
    concurrency: usize,

    #[arg(long, default_value_t = 256)]
    warmup: u64,

    #[arg(long, default_value_t = 5_000)]
    timeout_millis: u64,

    #[arg(long, default_value_t = DEFAULT_MAX_RESPONSE_BYTES)]
    max_response_bytes: usize,

    #[arg(long, value_enum)]
    expected_backend: Option<ExpectedBackend>,

    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone)]
struct BenchConfig {
    target: Arc<Target>,
    requests: u64,
    concurrency: usize,
    warmup: u64,
    json: bool,
}

#[derive(Debug)]
struct Target {
    address: SocketAddr,
    request: Arc<[u8]>,
    expected_backend: Option<&'static str>,
    timeout: Duration,
    max_response_bytes: usize,
}

impl TryFrom<Cli> for BenchConfig {
    type Error = anyhow::Error;

    fn try_from(cli: Cli) -> Result<Self> {
        ensure!(cli.requests > 0, "--requests must be greater than zero");
        ensure!(
            cli.concurrency > 0,
            "--concurrency must be greater than zero"
        );
        ensure!(
            cli.timeout_millis > 0,
            "--timeout-millis must be greater than zero"
        );
        ensure!(
            cli.max_response_bytes >= 128,
            "--max-response-bytes must be at least 128"
        );
        validate_path(&cli.path)?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: gput-bench/{}\r\n\r\n",
            cli.path,
            cli.address,
            env!("CARGO_PKG_VERSION")
        );

        Ok(Self {
            target: Arc::new(Target {
                address: cli.address,
                request: request.into_bytes().into(),
                expected_backend: cli.expected_backend.map(ExpectedBackend::as_str),
                timeout: Duration::from_millis(cli.timeout_millis),
                max_response_bytes: cli.max_response_bytes,
            }),
            requests: cli.requests,
            concurrency: cli.concurrency,
            warmup: cli.warmup,
            json: cli.json,
        })
    }
}

#[derive(Debug, Default)]
struct WorkerResult {
    latencies_nanos: Vec<u64>,
    response_bytes: u64,
}

#[derive(Debug)]
struct PhaseResult {
    requests: u64,
    concurrency: usize,
    elapsed: Duration,
    latencies_nanos: Vec<u64>,
    response_bytes: u64,
}

impl PhaseResult {
    fn requests_per_second(&self) -> f64 {
        self.requests as f64 / self.elapsed.as_secs_f64()
    }

    fn percentile_nanos(&self, percentile: u32) -> u64 {
        percentile_nearest_rank(&self.latencies_nanos, percentile)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = BenchConfig::try_from(Cli::parse())?;

    if config.warmup > 0 {
        run_phase(
            Arc::clone(&config.target),
            config.warmup,
            config.concurrency,
        )
        .await
        .context("warmup failed")?;
    }

    let result = run_phase(
        Arc::clone(&config.target),
        config.requests,
        config.concurrency,
    )
    .await
    .context("benchmark failed")?;

    if config.json {
        print_json_report(&result);
    } else {
        print_human_report(&config, &result);
    }

    Ok(())
}

async fn run_phase(
    target: Arc<Target>,
    requests: u64,
    requested_concurrency: usize,
) -> Result<PhaseResult> {
    let request_limit = usize::try_from(requests).unwrap_or(usize::MAX);
    let concurrency = requested_concurrency.min(request_limit).max(1);
    let next_request = Arc::new(AtomicU64::new(0));
    let mut workers = JoinSet::new();
    let phase_started = Instant::now();

    for _ in 0..concurrency {
        let target = Arc::clone(&target);
        let next_request = Arc::clone(&next_request);
        workers.spawn(async move {
            let mut result = WorkerResult::default();

            loop {
                let request_index = next_request.fetch_add(1, Ordering::Relaxed);
                if request_index >= requests {
                    break;
                }

                let request_started = Instant::now();
                let response_bytes = time::timeout(target.timeout, send_request(&target))
                    .await
                    .with_context(|| format!("request {request_index} timed out"))??;
                let latency_nanos = request_started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX)) as u64;

                result.latencies_nanos.push(latency_nanos);
                result.response_bytes = result
                    .response_bytes
                    .checked_add(response_bytes as u64)
                    .context("response byte counter overflowed")?;
            }

            Ok::<_, anyhow::Error>(result)
        });
    }

    let mut latencies_nanos = Vec::with_capacity(request_limit.min(1_000_000));
    let mut response_bytes = 0_u64;

    while let Some(joined) = workers.join_next().await {
        let worker = joined.context("benchmark worker panicked")??;
        response_bytes = response_bytes
            .checked_add(worker.response_bytes)
            .context("response byte counter overflowed")?;
        latencies_nanos.extend(worker.latencies_nanos);
    }

    let elapsed = phase_started.elapsed();
    ensure!(
        latencies_nanos.len() as u64 == requests,
        "completed {} of {requests} requests",
        latencies_nanos.len()
    );
    latencies_nanos.sort_unstable();

    Ok(PhaseResult {
        requests,
        concurrency,
        elapsed,
        latencies_nanos,
        response_bytes,
    })
}

async fn send_request(target: &Target) -> Result<usize> {
    let mut stream = TcpStream::connect(target.address)
        .await
        .with_context(|| format!("failed to connect to {}", target.address))?;
    stream.set_nodelay(true)?;
    stream.write_all(&target.request).await?;
    stream.shutdown().await?;

    let read_limit = u64::try_from(target.max_response_bytes)
        .context("maximum response size does not fit u64")?
        .saturating_add(1);
    let mut response = Vec::with_capacity(target.max_response_bytes.min(4_096));
    stream
        .take(read_limit)
        .read_to_end(&mut response)
        .await
        .context("failed to read response")?;

    ensure!(
        response.len() <= target.max_response_bytes,
        "response exceeded {} bytes",
        target.max_response_bytes
    );
    validate_response(&response, target.expected_backend)?;
    Ok(response.len())
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(path.starts_with('/'), "--path must start with '/'");
    ensure!(
        !path.is_empty()
            && path
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != 0x7f),
        "--path must contain only visible ASCII without spaces"
    );
    Ok(())
}

fn validate_response(response: &[u8], expected_backend: Option<&str>) -> Result<()> {
    let header_end = response
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|position| position + HEADER_TERMINATOR.len())
        .context("response has no complete HTTP headers")?;
    let header_bytes = &response[..header_end - HEADER_TERMINATOR.len()];
    let headers = std::str::from_utf8(header_bytes).context("response headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let status_line = lines.next().context("response has no status line")?;
    ensure!(
        status_line == "HTTP/1.1 200 OK",
        "unexpected response status: {status_line}"
    );

    let mut content_length = None;
    let mut backend = None;
    for line in lines {
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .context("invalid Content-Length response header")?,
            );
        } else if let Some(value) = line.strip_prefix("X-Gput-Backend: ") {
            backend = Some(value);
        }
    }

    let body_len = response.len() - header_end;
    ensure!(
        content_length == Some(body_len),
        "Content-Length {:?} does not match {body_len} response body bytes",
        content_length
    );

    if let Some(expected_backend) = expected_backend {
        ensure!(
            backend == Some(expected_backend),
            "expected backend {expected_backend}, got {}",
            backend.unwrap_or("<missing>")
        );
    }

    Ok(())
}

fn percentile_nearest_rank(sorted_nanos: &[u64], percentile: u32) -> u64 {
    assert!(!sorted_nanos.is_empty());
    assert!(percentile <= 100);

    let last_index = sorted_nanos.len() - 1;
    let index = (last_index * percentile as usize + 50) / 100;
    sorted_nanos[index]
}

fn print_human_report(config: &BenchConfig, result: &PhaseResult) {
    let backend = config
        .target
        .expected_backend
        .map_or("unchecked", |backend| backend);

    println!("gput-bench");
    println!("  target:      {}", config.target.address);
    println!("  backend:     {backend}");
    println!("  requests:    {}", result.requests);
    println!("  concurrency: {}", result.concurrency);
    println!("  elapsed:     {:.3} s", result.elapsed.as_secs_f64());
    println!("  throughput:  {:.0} req/s", result.requests_per_second());
    println!(
        "  latency:     p50 {:.3} ms | p95 {:.3} ms | p99 {:.3} ms | max {:.3} ms",
        nanos_to_millis(result.percentile_nanos(50)),
        nanos_to_millis(result.percentile_nanos(95)),
        nanos_to_millis(result.percentile_nanos(99)),
        nanos_to_millis(result.percentile_nanos(100)),
    );
    println!("  response:    {} bytes total", result.response_bytes);
}

fn print_json_report(result: &PhaseResult) {
    println!(
        concat!(
            "{{",
            "\"requests\":{},",
            "\"concurrency\":{},",
            "\"elapsed_seconds\":{:.9},",
            "\"requests_per_second\":{:.3},",
            "\"latency_nanos\":{{",
            "\"p50\":{},",
            "\"p95\":{},",
            "\"p99\":{},",
            "\"max\":{}",
            "}},",
            "\"response_bytes\":{}",
            "}}"
        ),
        result.requests,
        result.concurrency,
        result.elapsed.as_secs_f64(),
        result.requests_per_second(),
        result.percentile_nanos(50),
        result.percentile_nanos(95),
        result.percentile_nanos(99),
        result.percentile_nanos(100),
        result.response_bytes,
    );
}

fn nanos_to_millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_a_well_formed_backend_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nX-Gput-Backend: gpu\r\n\r\nok\n";

        validate_response(response, Some("gpu")).expect("valid response");
    }

    #[test]
    fn rejects_a_content_length_lie() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 9000\r\nX-Gput-Backend: cpu\r\n\r\nok\n";

        assert!(validate_response(response, Some("cpu")).is_err());
    }

    #[test]
    fn calculates_nearest_rank_percentiles() {
        let samples = [10, 20, 30, 40, 50];

        assert_eq!(percentile_nearest_rank(&samples, 0), 10);
        assert_eq!(percentile_nearest_rank(&samples, 50), 30);
        assert_eq!(percentile_nearest_rank(&samples, 95), 50);
        assert_eq!(percentile_nearest_rank(&samples, 100), 50);
    }

    #[test]
    fn rejects_paths_that_can_inject_request_lines() {
        assert!(validate_path("/hello").is_ok());
        assert!(validate_path("hello").is_err());
        assert!(validate_path("/hello world").is_err());
        assert!(validate_path("/hello\r\nInjected: yes").is_err());
    }
}
