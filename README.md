# gput

[![ci](https://github.com/Mik-pe/gput/actions/workflows/ci.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/ci.yml)

**GPU throughput applied to the least deserving workload imaginable.**

`gput` is an experimental HTTP/1.1 server written in Rust. The CPU accepts TCP connections and batches raw request bytes. A GPU compute shader parses each request line, selects an application route, executes a tiny response program, and writes the complete HTTP response into a storage buffer.

The name is the union of **GPU** and **throughput**, with enough resemblance to an unfortunate Unix command to be trustworthy.

## Write a GPU web app without writing WGSL

The public API is deliberately shaped like a small Rust web framework:

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

This is Axum-inspired, not Axum-compatible. The important difference is honesty: `get(...)` does not hide a normal CPU closure behind a GPU-shaped curtain. The builders run once at startup and compile the application into immutable GPU data:

```text
Rust Router
    |
    +--> exact-path route table: sorted hash, length, path string, response offset
    +--> response descriptors: status, content type, body program
    +--> UTF-8 string arena
    |
    v
storage buffers consumed by the compute shader
```

For every request, the compute shader still performs request parsing, route selection, hash-collision-safe path comparison, response-program execution, header assembly, UTF-8 output, and `Content-Length` formatting. The CPU implementation consumes the same compiled router as a baseline and automatic fallback. There is one application manifest, not two implementations waiting to drift apart.

### Response building blocks

A `Body` is a bounded program rather than a host callback. It can currently append:

- UTF-8 literals with `.push(...)` or `.literal(...)`
- the request path with `.path(max_bytes)`
- the query string with `.query(max_bytes)`
- the active backend with `.backend()`
- the framed request byte count with `.request_bytes()`
- the GPU-computed FNV-1a path hash with `.path_hash()`
- different immutable CPU/GPU literals with `.backend_variant(cpu, gpu)`

Responses support text, JSON, HTML, custom content types, and custom status codes. Every dynamic segment has a declared upper bound, so the router compiler can reject a response slot that is too small before serving traffic instead of discovering the problem through scenic buffer wreckage.

## Data path

```text
TCP socket
    |
    v
minimal Rust framing
    |
    v
bounded request batch
    |
    v
wgpu compute dispatch
    |  parse GET /path?query HTTP/1.1
    |  hash and exactly compare application routes
    |  interpret the compiled response program
    |  compose headers and UTF-8 bodies with a shader-side Writer
    v
GPU readback
    |
    v
TCP socket
```

## What works

- a real TCP listener built on Tokio
- an Axum-style Rust `Router` and `routing::get`
- one-call application startup through `gput::serve(config, router)`
- router compilation into a GPU route table, response descriptors, body bytecode, and string arena
- binary-searched FNV-1a routing with exact path comparison, so a hash collision cannot select the wrong route
- bounded dynamic response composition from request path, query, size, hash, and backend
- headless compute through `wgpu`
- raw HTTP request-line parsing in WGSL
- complete HTTP responses generated in WGSL
- host-built UTF-8 string storage with generated WGSL IDs
- a bounded shader-side `Writer` with string append, decimal formatting, UTF-8 decoding, and code-point encoding
- a CPU baseline using the same compiled router, network path, and batching path
- automatic CPU fallback when no compute-capable adapter is available
- limits for request size, response size, connection count, queue depth, and slow clients
- a built-in concurrent benchmark client with warmup, backend verification, percentiles, and JSON output
- unit tests, WGSL parse/validation, CPU socket smoke, and Vulkan compute smoke

The built-in application contains:

| Route | Response |
| --- | --- |
| `/` | project metadata as JSON |
| `/health` | `ok` |
| `/hello` | proof that the response came from the selected processor |
| `/utf8` | an unnecessarily GPU-generated Swedish, euro-denominated, owl-and-crab UTF-8 payload |
| `/inspect?anything` | path, query, request size, path hash, and backend assembled by the response program |
| anything else | `404 Not Found` |

Only `GET` is accepted. Each connection handles one request and closes. That remains deliberate while the GPU data path is being measured.

## The router compiler

Calling `gput::serve` compiles the `Router` before initializing the processor:

1. Route paths are validated, deduplicated, hashed, and interned.
2. Status reasons, content types, response literals, and route paths are deduplicated into one UTF-8 arena.
3. Each response body becomes fixed-width bytecode made from the supported `Body` operations.
4. The compiler calculates the maximum possible GPU response size, including headers and decimal widths.
5. The GPU receives the route table, response programs, string metadata, and packed string bytes as read-only storage buffers.
6. The CPU fallback retains the same compiled representation and interprets the same body operations.

Route declarations are static after startup. Request-derived values remain dynamic and are read directly from the request buffer by the compute shader.

## The cursed string model

WGSL does not provide a string type, so `gput` builds one out of storage buffers and stubbornness:

1. Rust gathers every router string into one tightly packed byte arena.
2. A parallel metadata table records each string's byte offset, byte length, and Unicode scalar count.
3. Rust prepends numeric IDs for protocol strings and body opcodes to the WGSL source before shader compilation.
4. The shader's bounds-checked `Writer` appends arena strings, request ranges, decimal integers, and encoded Unicode code points into each response slot.
5. Literal response strings are decoded and re-encoded on the GPU, catching truncated sequences, overlong encodings, surrogate code points, and values above `U+10FFFF`.

This is dynamic composition, not a general-purpose GPU heap. The router and string arena are immutable after GPU initialization, every writer has a fixed response-slot capacity, and overflow becomes an explicit shader failure.

## Run the built-in app

```bash
cargo run --release --locked -- --backend auto --bind 127.0.0.1:8080
```

Then:

```bash
curl -i http://127.0.0.1:8080/
curl -i http://127.0.0.1:8080/health
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

Start the server with either backend, then run the included load generator in another terminal:

```bash
cargo run --release --locked --bin gput-bench -- \
  --address 127.0.0.1:8080 \
  --path /hello \
  --requests 100000 \
  --concurrency 256 \
  --warmup 2000 \
  --expected-backend gpu
```

It reports elapsed time, requests per second, total response bytes, and p50/p95/p99/max latency. Machine-readable output is available with `--json`:

```bash
cargo run --release --locked --bin gput-bench -- \
  --requests 10000 \
  --concurrency 128 \
  --expected-backend cpu \
  --json
```

The benchmark opens one TCP connection per request because that is the server's current contract. It is intended for controlled CPU-versus-GPU comparisons of this repository, not as a replacement for mature HTTP benchmarking tools.

## Prove that it breathes

Build both binaries, then exercise the complete TCP path, malformed requests, dynamic response programs, query routing, UTF-8 byte lengths, a concurrent burst, and the built-in benchmark:

```bash
cargo build --locked --bins
bash scripts/smoke.sh cpu target/debug/gput
bash scripts/smoke.sh gpu target/debug/gput
```

The CI GPU smoke forces `wgpu` through Vulkan and Mesa Lavapipe. That executes the real upload, compute dispatch, shader parser/router, response interpreter, UTF-8 writer, readback, and socket response path on a deterministic software Vulkan adapter. It proves the GPU code path works end to end, but it is not a substitute for performance measurements on a discrete AMD, Intel, or NVIDIA GPU.

CI runs formatting, Clippy with warnings denied, all tests, both binaries, the CPU smoke suite, and the same smoke and benchmark suite against Vulkan/Lavapipe.

A red direct-to-main build opens an issue containing the failure log. The next green build closes superseded CI failure issues automatically.

## Development rule

Work goes directly into `main`. No mandatory branches, no default pull requests, no velvet rope. Commits should still be coherent, tested, and easy to revert. See [`AGENTS.md`](AGENTS.md).

## Honest limitations

This is not a production web server. It currently has no TLS, keep-alive, request bodies, HTTP/2, HTTP/3, path parameters, middleware, arbitrary headers, mutable GPU string heap, persistent mapped ring buffers, or zero-copy NIC integration. Routes are exact static paths and response programs are intentionally small and bounded.

The next sensible expansion is to add typed GPU-native response operations for work such as hashing, image processing, vector search, or inference without exposing raw WGSL to application authors. Until then, this is a surprisingly ergonomic electrical joke.
