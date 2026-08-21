# gput

[![ci](https://github.com/Mik-pe/gput/actions/workflows/ci.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/ci.yml)

**GPU throughput applied to the least deserving workload imaginable.**

`gput` is an experimental HTTP/1.1 server written in Rust. The normal mode lets the CPU accept and frame TCP traffic while a GPU compute shader parses request lines, selects application routes, executes tiny response programs, and writes complete HTTP responses into storage buffers. The more unreasonable mode also contains an experimental GPU-owned IPv4/TCP fast path fed by raw packets.

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

For every GPU request the compute shader still performs request-line parsing, route selection, exact collision-safe path comparison, response-program execution, header assembly, UTF-8 output, and `Content-Length` formatting. The CPU implementation consumes the same compiled router as a baseline and fallback.

## Response building blocks

A `Body` is a bounded program rather than a host callback. It can append:

- UTF-8 literals with `.push(...)` or `.literal(...)`
- the request path with `.path(max_bytes)`
- the query string with `.query(max_bytes)`
- the active backend with `.backend()`
- the framed request byte count with `.request_bytes()`
- the GPU-computed FNV-1a path hash with `.path_hash()`
- different immutable CPU/GPU literals with `.backend_variant(cpu, gpu)`

Responses support text, JSON, HTML, custom content types, and custom status codes. Dynamic segments declare upper bounds so the router compiler can reject an undersized GPU response slot before traffic starts.

## Data path

The normal portable server path is:

```text
persistent TCP connection
    |
    | HTTP/1.1 framing + pipelined request boundaries
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

HTTP/1.1 connections stay open by default. Multiple complete requests already waiting on a socket are preserved rather than discarded, so ordinary keep-alive and HTTP/1.1 pipelining work. HTTP/1.0 and explicit `Connection: close` requests close after their response.

## GPU-native TCP path

`GpuPacketEngine` moves the experiment one layer lower. Its portability boundary is deliberately tiny:

```text
raw IPv4 packets
    |
    v
GPU compute
    |  IPv4 parsing
    |  TCP flow lookup + persistent state
    |  SYN / SYN-ACK / ACK
    |  in-order sequence validation
    |  GET /plaintext
    |  HTTP response
    |  IPv4 + TCP checksums
    |  FIN acknowledgement
    v
raw IPv4 packets
```

`gput-packetd` connects that engine to a cross-platform L3 TUN interface. On Linux the CI suite proves a real `curl` can establish TCP with the GPU state machine and receive `Hello, World!` without terminating in a normal server TCP socket.

```bash
cargo build --release --locked --bin gput-packetd
sudo ./target/release/gput-packetd \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080
```

Then from another terminal:

```bash
curl --noproxy '*' http://10.77.0.2:8080/plaintext
```

The same WGSL protocol engine can run through Metal on Apple Silicon and Vulkan on AMD, NVIDIA, or Intel. TUN is currently the portable packet transport. The AMD direct target is a ROCm XIO/HIP transport that replaces the TUN source/sink while preserving the same TCP semantics.

See [docs/GPU_NETWORKING.md](docs/GPU_NETWORKING.md) for the packet architecture, macOS path, AMD direct path, and TCP hardening roadmap.

## What works

- Tokio TCP listener
- persistent HTTP/1.1 connections
- basic HTTP/1.1 pipelining
- an Axum-style Rust `Router` and `routing::get`
- router compilation into GPU route tables, response descriptors, body bytecode, and a string arena
- binary-searched FNV-1a routing with exact path comparison
- bounded dynamic response composition from request path, query, size, hash, and backend
- headless compute through `wgpu`
- raw HTTP request-line parsing in WGSL
- complete HTTP responses generated in WGSL
- cursed but bounds-checked shader-side UTF-8 strings
- CPU baseline using the same compiled router
- automatic CPU fallback
- limits for request size, response size, connection count, queue depth, and slow clients
- a persistent concurrent benchmark client with backend verification, percentiles, pipelining, concurrency sweeps, repeated runs, medians, and JSON output
- a vendor-neutral raw IPv4/TCP compute engine with persistent GPU flow state
- a Linux/macOS-capable TUN bridge into that packet engine
- GPU-generated IPv4 and TCP checksums
- synthetic GPU TCP handshake validation and real Linux TUN-to-GPU `curl` coverage in CI
- CPU socket smoke and Vulkan/Lavapipe GPU smoke

The `gput` binary exposes:

| Route | Response |
| --- | --- |
| `/` | project metadata as JSON |
| `/health` | `ok` |
| `/hello` | proof of the selected processor |
| `/plaintext` | exactly `Hello, World!` for boring throughput measurements |
| `/utf8` | an unnecessarily GPU-generated Swedish owl-and-crab UTF-8 payload |
| `/inspect?anything` | path, query, request size, path hash, and backend assembled by the response program |
| anything else | `404 Not Found` |

Only `GET` is accepted.

## Run it

```bash
cargo run --release --locked -- --backend auto --bind 127.0.0.1:8080
```

Then:

```bash
curl -i http://127.0.0.1:8080/plaintext
curl -i http://127.0.0.1:8080/hello
curl -i http://127.0.0.1:8080/utf8
curl -i 'http://127.0.0.1:8080/inspect?owl=yes'
```

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

## Measure the damage

One run:

```bash
cargo run --release --locked --bin gput-bench -- \
  --address 127.0.0.1:8080 \
  --path /plaintext \
  --requests 100000 \
  --concurrency 256 \
  --pipeline 1 \
  --expected-backend gpu
```

The useful mode is the suite:

```bash
cargo run --release --locked --bin gput-bench -- suite \
  --address 127.0.0.1:8080 \
  --path /plaintext \
  --requests 100000 \
  --suite-concurrency 1,16,64,256,1024 \
  --repeats 5 \
  --label gput-gpu \
  --expected-backend gpu
```

It reports median and best RPS plus median p99 latency for every concurrency level. `--pipeline N` controls requests written before the matching responses are read. `--json` provides machine-readable output.

The same benchmark can point at another server. Do not pass `--expected-backend` for competitors. That makes it possible to benchmark an Axum, Actix, Hyper, C++, Go, or other `/plaintext` implementation with the same request generator and sweep instead of grading gput with a private ruler.

See [BENCHMARKING.md](BENCHMARKING.md) for the benchmark rules and comparison workflow. Public performance claims should also be corroborated with an independent load generator such as `wrk` or `oha`.

## The cursed string model

WGSL does not provide a string type, so gput builds one out of storage buffers and stubbornness:

1. Rust gathers router strings into one tightly packed byte arena.
2. A metadata table records byte offset, byte length, and Unicode scalar count.
3. Rust prepends numeric IDs and bytecode constants to the WGSL source.
4. The shader's bounds-checked `Writer` appends strings, request ranges, decimal integers, and Unicode code points into response slots.
5. Literal response strings are decoded and re-encoded on the GPU with malformed UTF-8 rejected.

This is dynamic composition, not a general-purpose GPU heap. The router and string arena are immutable after GPU initialization and every writer has a fixed response-slot capacity.

## Prove that it breathes

```bash
cargo build --locked --bins
bash scripts/smoke.sh cpu target/debug/gput
bash scripts/smoke.sh gpu target/debug/gput
cargo run --release --locked --bin gput-packet-demo
```

CI goes further than the synthetic packet demo. It creates a real Linux TUN interface, starts `gput-packetd` against Vulkan/Lavapipe, and curls the virtual peer through the GPU-owned TCP path. Lavapipe proves correctness, not discrete-GPU performance.

A red direct-to-main build opens a failure-log issue; the next green build closes superseded failure issues automatically.

## Development rule

Work goes directly into `main`. No mandatory branches, no default pull requests, no velvet rope. Commits should still be coherent and easy to revert. See [AGENTS.md](AGENTS.md).

## Honest limitations

This is not a production web server. The socket path has no TLS, request bodies, HTTP/2, HTTP/3, path parameters, arbitrary middleware, mutable GPU string heap, persistent mapped packet rings, or zero-copy NIC integration. The raw-packet TCP path deliberately lacks retransmission, congestion control, receive-window management, out-of-order reassembly, SYN-flood protection, IPv6, fragmentation handling, and collision-safe flow-table probing.

The portable TUN bridge currently handles one packet per GPU dispatch, so it exists to prove the architecture, not to claim peak packet throughput. The next performance work is batching/ring buffers, then AF_XDP and AMD ROCm XIO/direct NIC transport. The project has officially escaped the HTTP layer and started chewing on the network stack itself.
