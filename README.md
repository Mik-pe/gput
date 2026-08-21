# gput

[![ci](https://github.com/Mik-pe/gput/actions/workflows/ci.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/ci.yml)
[![macOS](https://github.com/Mik-pe/gput/actions/workflows/macos.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/macos.yml)

**GPU throughput applied to the least deserving workload imaginable.**

`gput` is an experimental HTTP/1.1 server written in Rust. Its portable socket mode lets the CPU own TCP framing while a GPU compute shader parses request lines, routes requests, executes bounded response programs and writes complete HTTP responses. Its more unreasonable packet mode also owns a useful slice of IPv4/TCP state in compute and can serve a real client through raw packets instead of a server socket.

The name is the union of **GPU** and **throughput**, with enough resemblance to an unfortunate Unix command to be trustworthy.

## Write a GPU web app without writing WGSL

```rust
use anyhow::Result;
use gput::{
    Router,
    config::ServerConfig,
    response::{Body, Response},
    routing::get,
};

#[tokio::main]
async fn main() -> Result<()> {
    let app = Router::new()
        .route("/health", get(Response::text("ok\n")))
        .route(
            "/inspect",
            get(Response::text(
                Body::new()
                    .push("path=")
                    .path(64)
                    .push("\nquery=")
                    .query(64)
                    .push("\nrequest_bytes=")
                    .request_bytes()
                    .push("\npath_hash=")
                    .path_hash()
                    .push("\nbackend=")
                    .backend()
                    .push("\n"),
            )),
        );

    gput::serve(ServerConfig::default(), app).await
}
```

This is Axum-inspired, not Axum-compatible. The builders run once at startup and compile the application into immutable GPU data:

```text
Rust Router
    |
    +--> exact-path route table
    +--> response descriptors
    +--> bounded body bytecode
    +--> UTF-8 string arena
    |
    v
GPU storage buffers
```

For every GPU request the shader still performs request-line parsing, route selection, exact collision-safe path comparison, response-program execution, header assembly, UTF-8 output and `Content-Length` formatting. The CPU implementation consumes the same compiled router as a baseline and fallback.

## Two data paths

### Portable socket path

```text
persistent TCP connection
    |
    | CPU frames complete HTTP requests
    v
bounded request batches
    |
    v
wgpu compute dispatch
    |  parse GET /path?query HTTP/1.1
    |  route
    |  execute response bytecode
    |  build HTTP response
    v
GPU readback
    |
    v
persistent TCP connection
```

HTTP/1.1 connections stay open by default. Multiple requests already waiting on a socket are preserved, so keep-alive and basic HTTP/1.1 pipelining work.

### GPU-owned packet path

```text
TUN / future NIC packet ring
    |
    v
raw IPv4 packet batches
    |
    v
GPU compute
    |  IPv4 and TCP parsing
    |  collision-safe flow lookup
    |  SYN / SYN-ACK / ACK
    |  sequence and acknowledgement state
    |  retransmitted SYN/data recovery
    |  HTTP request-line routing
    |  200 / 400 / 404 / 405 responses
    |  IPv4 and TCP checksums
    |  FIN / RST cleanup
    v
raw IPv4 response batches
```

`GpuPacketEngine` stores the complete IPv4 four-tuple and sequence state in GPU-visible storage. A hash only chooses the first slot; bounded open addressing and exact tuple comparison keep colliding connections separate. Independent flows run in parallel, while packets belonging to one flow are scheduled into ordered dispatch waves.

`gput-packetd` connects that engine to a cross-platform L3 TUN interface and gathers packets into configurable GPU batches. On Linux, CI proves that a real `curl` can establish TCP with this state machine and receive `Hello, World!` without terminating in a normal server TCP socket.

```bash
cargo build --release --locked --bin gput-packetd
sudo ./target/release/gput-packetd \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080 \
  --gpu-batch-capacity 256 \
  --batch-wait-micros 50
```

Then:

```bash
curl --noproxy '*' -i http://10.77.0.2:8080/plaintext
curl --noproxy '*' -i http://10.77.0.2:8080/health
```

The response identifies itself as `X-Gput-Backend: gpu-packet`.

## Prove it instead of believing it

One command builds the project, runs the packet-level correctness gauntlet, then feeds the same synthetic IPv4/TCP conversations to a single-threaded CPU reference and the GPU engine:

```bash
./scripts/prove-gpu.sh
```

The raw-packet championship reports:

- engine requests/s
- end-to-end harness requests/s
- represented wire packets/s
- response MiB/s
- p50 and p99 batch-round latency
- handshake flows/s
- packets per GPU dispatch
- the selected GPU adapter
- GPU-to-CPU-reference ratio

Use a larger arena on real hardware:

```bash
GPUT_PROOF_FLOWS=8192 \
GPUT_PROOF_REQUESTS_PER_FLOW=2000 \
GPUT_PROOF_BATCH_SIZE=512 \
./scripts/prove-gpu.sh
```

The CPU row is a straightforward single-threaded semantic reference, not the Linux kernel and not the best possible CPU stack. CI uses Lavapipe to prove correctness, not discrete-GPU speed. See [docs/PROOF.md](docs/PROOF.md) for the evidence contract and [BENCHMARKING.md](BENCHMARKING.md) for fair comparison rules.

## What works

- a Tokio TCP listener with persistent HTTP/1.1 connections
- basic HTTP/1.1 pipelining on the socket path
- an Axum-style Rust `Router` and `routing::get`
- router compilation into GPU route tables, response descriptors, bytecode and a string arena
- binary-searched FNV-1a routing with exact collision-safe path comparison
- bounded dynamic response composition from path, query, request size, path hash and backend
- complete HTTP responses generated in WGSL
- cursed but bounds-checked shader-side UTF-8 strings
- CPU socket baseline and automatic fallback
- `gput-bench` with persistent connections, concurrency sweeps, repeated runs, medians and JSON
- vendor-neutral raw IPv4/TCP compute through Metal or Vulkan
- atomically claimed, collision-safe GPU flow slots
- persistent sequence state and retransmitted SYN/data recovery
- packet routes for `/plaintext` and `/health`, plus honest `400`, `404` and `405`
- GPU-generated IPv4 and TCP checksums
- batched TUN ingestion and per-dispatch telemetry
- a CPU packet reference implementing the same narrow semantics
- `gput-packet-bench` for identical raw packet workloads on CPU and GPU
- synthetic collision/retransmission tests and real Linux TUN-to-GPU `curl` coverage
- macOS build and shader-validation coverage

## Run the socket application

```bash
cargo run --release --locked -- --backend auto --bind 127.0.0.1:8080
```

The built-in application exposes:

| Route | Response |
| --- | --- |
| `/` | project metadata as JSON |
| `/health` | `ok` |
| `/hello` | proof of the selected processor |
| `/plaintext` | exactly `Hello, World!` |
| `/utf8` | an unnecessarily GPU-generated Swedish owl-and-crab payload |
| `/inspect?anything` | path, query, request size, path hash and backend |
| anything else | `404 Not Found` |

Force a backend:

```bash
cargo run --release --locked -- --backend gpu
cargo run --release --locked -- --backend cpu
```

Useful tuning knobs:

```bash
cargo run --release --locked -- \
  --backend gpu \
  --batch-size 256 \
  --batch-wait-micros 50 \
  --queue-depth 8192 \
  --max-request-bytes 4096 \
  --response-slot-bytes 512
```

`WGPU_BACKEND` and `WGPU_ADAPTER_NAME` can influence adapter selection.

## Benchmark real HTTP servers

```bash
cargo run --release --locked --bin gput-bench -- suite \
  --address 127.0.0.1:8080 \
  --path /plaintext \
  --requests 100000 \
  --suite-concurrency 1,16,64,256,1024 \
  --repeats 5 \
  --pipeline 1 \
  --label gput-gpu \
  --expected-backend gpu
```

Point the same client at Axum, Actix, Hyper, Go, C++ or another implementation without `--expected-backend`. Public claims should also be corroborated with an independent generator such as `wrk` or `oha`.

To benchmark the real TUN/GPU TCP path, start `gput-packetd` and point `gput-bench` at `10.77.0.2:8080` with `--pipeline 1`.

## Documentation

- [Benchmark rules](BENCHMARKING.md)
- [GPU networking architecture](docs/GPU_NETWORKING.md)
- [Proof and claim boundaries](docs/PROOF.md)
- [Repository rules](AGENTS.md)

## Honest limitations

This is not a production web server or an RFC-complete TCP stack.

The socket path has no TLS, request bodies, HTTP/2, HTTP/3, path parameters, arbitrary middleware, mutable GPU string heap or zero-copy NIC integration. The packet path still lacks retransmission timers, congestion control, receive-window accounting, bounded out-of-order reassembly, SYN-flood protection, IPv6, PMTU handling and HTTP stream reassembly for several requests carried by one TCP segment.

TUN still crosses the host networking boundary. The direct AMD destination is a HIP/ROCm XIO packet transport, with AF_XDP as the nearer Linux kernel-bypass step. macOS remains a unified-memory hybrid because Apple does not expose a public general-purpose NIC-to-Metal DMA queue.

What exists today is intentionally narrower but real: persistent TCP state, HTTP routing and checksummed packet generation execute in a portable compute shader, survive common retransmissions, separate colliding flows, batch independent connections and serve an ordinary TCP client. The project has escaped the HTTP layer and started eating the network stack one measurable bite at a time.
