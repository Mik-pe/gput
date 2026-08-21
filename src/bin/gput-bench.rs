use std::{
    fmt::Write as _,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum Mode {
    #[default]
    Run,
    Suite,
}

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
    about = "Persistent HTTP load generation for comparing gput with less electrically adventurous servers"
)]
struct Cli {
    #[arg(value_enum, default_value = "run")]
    mode: Mode,

    #[arg(long, default_value = "127.0.0.1:8080")]
    address: SocketAddr,

    #[arg(long, default_value = "/plaintext")]
    path: String,

    #[arg(long, default_value_t = 100_000)]
    requests: u64,

    #[arg(long, default_value_t = 128)]
    concurrency: usize,

    #[arg(long, default_value_t = 2_000)]
    warmup: u64,

    #[arg(long, default_value_t = 1)]
    pipeline: usize,

    #[arg(long, value_delimiter = ',', default_value = "1,16,64,256,1024")]
    suite_concurrency: Vec<usize>,

    #[arg(long, default_value_t = 3)]
    repeats: usize,

    #[arg(long, default_value = "target")]
    label: String,

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
    mode: Mode,
    target: Arc<Target>,
    requests: u64,
    concurrency: usize,
    warmup: u64,
    pipeline: usize,
    suite_concurrency: Vec<usize>,
    repeats: usize,
    label: String,
    path: String,
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
        ensure!(cli.concurrency > 0, "--concurrency must be greater than zero");
        ensure!(cli.pipeline > 0, "--pipeline must be greater than zero");
        ensure!(cli.repeats > 0, "--repeats must be greater than zero");
        ensure!(
            !cli.suite_concurrency.is_empty()
                && cli.suite_concurrency.iter().all(|concurrency| *concurrency > 0),
            "--suite-concurrency must contain positive values"
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
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nUser-Agent: gput-bench/{}\r\n\r\n",
            cli.path,
            cli.address,
            env!("CARGO_PKG_VERSION")
        );

        Ok(Self {
            mode: cli.mode,
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
            pipeline: cli.pipeline,
            suite_concurrency: cli.suite_concurrency,
            repeats: cli.repeats,
            label: cli.label,
            path: cli.path,
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
    pipeline: usize,
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

#[derive(Debug)]
struct SuiteRow {
    concurrency: usize,
    median_rps: f64,
    best_rps: f64,
    median_p99_nanos: u64,
}

struct BenchConnection {
    stream: TcpStream,
    read_buffer: Vec<u8>,
}

impl BenchConnection {
    async fn connect(target: &Target) -> Result<Self> {
        let stream = time::timeout(target.timeout, TcpStream::connect(target.address))
            .await
            .with_context(|| format!("connection to {} timed out", target.address))??;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            read_buffer: Vec::with_capacity(8 * 1024),
        })
    }

    async fn round_trip(&mut self, target: &Target, count: usize) -> Result<WorkerResult> {
        let started = Instant::now();
        for _ in 0..count {
            self.stream.write_all(&target.request).await?;
        }
        self.stream.flush().await?;

        let mut result = WorkerResult {
            latencies_nanos: Vec::with_capacity(count),
            response_bytes: 0,
        };
        for _ in 0..count {
            let response_bytes = self.read_response(target).await?;
            result.response_bytes = result
                .response_bytes
                .checked_add(response_bytes as u64)
                .context("response byte counter overflowed")?;
            result.latencies_nanos.push(elapsed_nanos(started));
        }
        Ok(result)
    }

    async fn read_response(&mut self, target: &Target) -> Result<usize> {
        let mut chunk = [0_u8; 8 * 1024];

        loop {
            if let Some(response_len) = response_frame_len(
                &self.read_buffer,
                target.max_response_bytes,
            )? {
                validate_response(
                    &self.read_buffer[..response_len],
                    target.expected_backend,
                )?;
                let remaining = self.read_buffer.len() - response_len;
                self.read_buffer.copy_within(response_len.., 0);
                self.read_buffer.truncate(remaining);
                return Ok(response_len);
            }

            let bytes_read = self.stream.read(&mut chunk).await?;
            if bytes_read == 0 {
                bail!("server closed a persistent benchmark connection mid-response");
            }
            self.read_buffer.extend_from_slice(&chunk[..bytes_read]);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = BenchConfig::try_from(Cli::parse())?;

    match config.mode {
        Mode::Run => run_once(&config).await,
        Mode::Suite => run_suite(&config).await,
    }
}

async fn run_once(config: &BenchConfig) -> Result<()> {
    if config.warmup > 0 {
        run_phase(
            Arc::clone(&config.target),
            config.warmup,
            config.concurrency,
            config.pipeline,
        )
        .await
        .context("warmup failed")?;
    }

    let result = run_phase(
        Arc::clone(&config.target),
        config.requests,
        config.concurrency,
        config.pipeline,
    )
    .await
    .context("benchmark failed")?;

    if config.json {
        print_json_report(&result);
    } else {
        print_human_report(config, &result);
    }

    Ok(())
}

async fn run_suite(config: &BenchConfig) -> Result<()> {
    let mut rows = Vec::with_capacity(config.suite_concurrency.len());

    for &concurrency in &config.suite_concurrency {
        if config.warmup > 0 {
            run_phase(
                Arc::clone(&config.target),
                config.warmup,
                concurrency,
                config.pipeline,
            )
            .await
            .with_context(|| format!("warmup failed at concurrency {concurrency}"))?;
        }

        let mut runs = Vec::with_capacity(config.repeats);
        for _ in 0..config.repeats {
            runs.push(
                run_phase(
                    Arc::clone(&config.target),
                    config.requests,
                    concurrency,
                    config.pipeline,
                )
                .await
                .with_context(|| format!("suite failed at concurrency {concurrency}"))?,
            );
        }

        let mut rps = runs
            .iter()
            .map(PhaseResult::requests_per_second)
            .collect::<Vec<_>>();
        rps.sort_by(f64::total_cmp);
        let mut p99 = runs
            .iter()
            .map(|run| run.percentile_nanos(99))
            .collect::<Vec<_>>();
        p99.sort_unstable();

        rows.push(SuiteRow {
            concurrency,
            median_rps: median_f64(&rps),
            best_rps: *rps.last().expect("suite always has at least one run"),
            median_p99_nanos: median_u64(&p99),
        });
    }

    if config.json {
        print_suite_json(config, &rows);
    } else {
        print_suite_human(config, &rows);
    }

    Ok(())
}

async fn run_phase(
    target: Arc<Target>,
    requests: u64,
    requested_concurrency: usize,
    pipeline: usize,
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
            let mut connection = BenchConnection::connect(&target).await?;
            let mut result = WorkerResult::default();
            let claim = u64::try_from(pipeline).unwrap_or(u64::MAX);

            loop {
                let first_request = next_request.fetch_add(claim, Ordering::Relaxed);
                if first_request >= requests {
                    break;
                }
                let count = usize::try_from((requests - first_request).min(claim))
                    .context("pipeline batch size does not fit usize")?;
                let batch = time::timeout(
                    target.timeout,
                    connection.round_trip(&target, count),
                )
                .await
                .with_context(|| format!("request batch starting at {first_request} timed out"))??;

                result.response_bytes = result
                    .response_bytes
                    .checked_add(batch.response_bytes)
                    .context("response byte counter overflowed")?;
                result.latencies_nanos.extend(batch.latencies_nanos);
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
        pipeline,
        elapsed,
        latencies_nanos,
        response_bytes,
    })
}

fn response_frame_len(buffer: &[u8], max_response_bytes: usize) -> Result<Option<usize>> {
    let Some(header_end) = buffer
        .windows(HEADER_TERMINATOR.len())
        .position(|window| window == HEADER_TERMINATOR)
        .map(|position| position + HEADER_TERMINATOR.len())
    else {
        ensure!(
            buffer.len() <= max_response_bytes,
            "response headers exceeded {max_response_bytes} bytes"
        );
        return Ok(None);
    };

    let content_length = response_content_length(&buffer[..header_end - HEADER_TERMINATOR.len()])?;
    let response_len = header_end
        .checked_add(content_length)
        .context("response length overflowed")?;
    ensure!(
        response_len <= max_response_bytes,
        "response exceeded {max_response_bytes} bytes"
    );

    Ok((buffer.len() >= response_len).then_some(response_len))
}

fn response_content_length(headers: &[u8]) -> Result<usize> {
    for raw_line in headers.split(|byte| *byte == b'\n').skip(1) {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if line[..colon].eq_ignore_ascii_case(b"content-length") {
            let value = std::str::from_utf8(trim_ascii(&line[colon + 1..]))
                .context("Content-Length is not ASCII")?;
            return value
                .parse::<usize>()
                .context("invalid Content-Length response header");
        }
    }

    bail!("response has no Content-Length header")
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
    let status_line = headers
        .split("\r\n")
        .next()
        .context("response has no status line")?;
    ensure!(
        status_line == "HTTP/1.1 200 OK",
        "unexpected response status: {status_line}"
    );

    let content_length = response_content_length(header_bytes)?;
    let body_len = response.len() - header_end;
    ensure!(
        content_length == body_len,
        "Content-Length {content_length} does not match {body_len} response body bytes"
    );

    if let Some(expected_backend) = expected_backend {
        let backend = response_header(header_bytes, b"x-gput-backend")
            .and_then(|value| std::str::from_utf8(value).ok());
        ensure!(
            backend == Some(expected_backend),
            "expected backend {expected_backend}, got {}",
            backend.unwrap_or("<missing>")
        );
    }

    Ok(())
}

fn response_header<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for raw_line in headers.split(|byte| *byte == b'\n').skip(1) {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let colon = line.iter().position(|byte| *byte == b':')?;
        if line[..colon].eq_ignore_ascii_case(name) {
            return Some(trim_ascii(&line[colon + 1..]));
        }
    }
    None
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn percentile_nearest_rank(sorted_nanos: &[u64], percentile: u32) -> u64 {
    assert!(!sorted_nanos.is_empty());
    assert!(percentile <= 100);

    let last_index = sorted_nanos.len() - 1;
    let index = (last_index * percentile as usize + 50) / 100;
    sorted_nanos[index]
}

fn median_f64(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

fn median_u64(sorted: &[u64]) -> u64 {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        sorted[middle - 1].saturating_add(sorted[middle]) / 2
    } else {
        sorted[middle]
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn print_human_report(config: &BenchConfig, result: &PhaseResult) {
    let backend = config
        .target
        .expected_backend
        .map_or("unchecked", |backend| backend);

    println!("gput-bench");
    println!("  target:      {}", config.target.address);
    println!("  path:        {}", config.path);
    println!("  backend:     {backend}");
    println!("  requests:    {}", result.requests);
    println!("  concurrency: {}", result.concurrency);
    println!("  pipeline:    {}", result.pipeline);
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

fn print_suite_human(config: &BenchConfig, rows: &[SuiteRow]) {
    println!("gput-bench suite");
    println!("  label:    {}", config.label);
    println!("  target:   {}{}", config.target.address, config.path);
    println!("  requests: {} per repeat", config.requests);
    println!("  repeats:  {}", config.repeats);
    println!("  pipeline: {}", config.pipeline);
    println!();
    println!("concurrency | median req/s | best req/s | median p99");
    println!("-----------:|-------------:|-----------:|-----------:");
    for row in rows {
        println!(
            "{:11} | {:12.0} | {:10.0} | {:9.3} ms",
            row.concurrency,
            row.median_rps,
            row.best_rps,
            nanos_to_millis(row.median_p99_nanos),
        );
    }
}

fn print_json_report(result: &PhaseResult) {
    println!(
        concat!(
            "{{",
            "\"requests\":{},",
            "\"concurrency\":{},",
            "\"pipeline\":{},",
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
        result.pipeline,
        result.elapsed.as_secs_f64(),
        result.requests_per_second(),
        result.percentile_nanos(50),
        result.percentile_nanos(95),
        result.percentile_nanos(99),
        result.percentile_nanos(100),
        result.response_bytes,
    );
}

fn print_suite_json(config: &BenchConfig, rows: &[SuiteRow]) {
    let mut output = String::new();
    write!(
        output,
        "{{\"label\":{},\"path\":{},\"pipeline\":{},\"requests_per_repeat\":{},\"repeats\":{},\"results\":[",
        json_string(&config.label),
        json_string(&config.path),
        config.pipeline,
        config.requests,
        config.repeats,
    )
    .expect("writing to a String cannot fail");

    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(
            output,
            "{{\"concurrency\":{},\"median_requests_per_second\":{:.3},\"best_requests_per_second\":{:.3},\"median_p99_nanos\":{}}}",
            row.concurrency, row.median_rps, row.best_rps, row.median_p99_nanos,
        )
        .expect("writing to a String cannot fail");
    }
    output.push_str("]}");
    println!("{output}");
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", character as u32)
                    .expect("writing to a String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
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
    fn frames_one_response_without_eating_the_next_one() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok\n";
        let mut pipelined = response.to_vec();
        pipelined.extend_from_slice(response);

        assert_eq!(
            response_frame_len(&pipelined, DEFAULT_MAX_RESPONSE_BYTES)
                .expect("response frame parses"),
            Some(response.len())
        );
    }

    #[test]
    fn rejects_a_content_length_lie() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 9000\r\nX-Gput-Backend: cpu\r\n\r\nok\n";

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

    #[test]
    fn json_strings_do_not_escape_themselves_into_invalid_json() {
        assert_eq!(json_string("gput \"gpu\""), "\"gput \\\"gpu\\\"\"");
    }
}
