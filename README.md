# gput

[![ci](https://github.com/Mik-pe/gput/actions/workflows/ci.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/ci.yml)

**GPU throughput applied to the least deserving workload imaginable.**

`gput` is an experimental HTTP/1.1 server written in Rust. The CPU accepts TCP connections and batches raw request bytes. A GPU compute shader parses the request line, selects a route, and writes the complete HTTP response into a storage buffer.

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
    |  parse GET /path HTTP/1.1
    |  route by FNV-1a hash
    |  compose headers and UTF-8 bodies with a shader-side Writer
    v
GPU readback
    |
    v
TCP socket
```

The name is the union of **GPU** and **throughput**, with enough resemblance to an unfortunate Unix command to be trustworthy.

## What works

- real TCP listener built on Tokio
- bounded request queue with configurable micro-batching
- headless compute through `wgpu`
- raw HTTP request-line parsing in WGSL
- query-string stripping and FNV-1a route selection in WGSL
- complete HTTP responses generated in WGSL
- host-built UTF-8 string arena with generated WGSL string IDs
- bounded shader-side `Writer` with string append, decimal formatting, UTF-8 decoding, and code-point encoding
- CPU baseline backend using the same network and batching path
- automatic CPU fallback when no compute-capable adapter is available
- limits for request size, connection count, queue depth, and slow clients
- pinned Rust toolchain and committed dependency lockfile
- built-in concurrent benchmark client with warmup, backend verification, percentiles, and JSON output
- unit tests, WGSL parse/validation, CPU socket smoke, and Vulkan compute smoke

Current routes:

| Route | Response |
| --- | --- |
| `/` | project metadata as JSON |
| `/health` | `ok` |
| `/hello` | proof that the response came from the selected processor |
| `/utf8` | an unnecessarily GPU-generated Swedish, euro-denominated, owl-and-crab UTF-8 payload |
| anything else | `404 Not Found` |

Only `GET` is accepted. Each connection handles one request and closes. That is deliberate while the GPU data path is being measured.

## The cursed string model

WGSL does not provide a string type, so `gput` builds one out of storage buffers and stubbornness:

1. Rust gathers named UTF-8 literals into one tightly packed byte arena.
2. A parallel metadata table records each string's byte offset, byte length, and Unicode scalar count.
3. Rust prepends generated numeric string IDs to the WGSL source before shader compilation.
4. The shader's bounds-checked `Writer` appends arena strings, bytes, decimal integers, and encoded Unicode code points into each response slot.
5. `/utf8` deliberately decodes every scalar and re-encodes it on the GPU instead of merely copying bytes. This catches truncated sequences, overlong encodings, surrogate code points, and values above `U+10FFFF`.

Adding a response literal now means adding a named Rust string definition and referring to its generated WGSL ID. Header assembly and `Content-Length` formatting happen at runtime in the compute shader.

This is dynamic composition, not a general-purpose GPU heap. The string arena is immutable after GPU initialization, every writer has a fixed response-slot capacity, and overflow becomes an explicit shader failure instead of a scenic memory-corruption excursion.

## Run it

```bash
cargo run --release --locked -- --backend auto --bind 127.0.0.1:8080
```

Then:

```bash
curl -i http://127.0.0.1:8080/
curl -i http://127.0.0.1:8080/health
curl -i http://127.0.0.1:8080/hello
curl -i http://127.0.0.1:8080/utf8
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
  --max-request-bytes 4096
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

The benchmark opens one TCP connection per request because that is the server's current contract. It is intended for controlled CPU-versus-GPU comparisons of this repository, not as a general replacement for mature HTTP benchmarking tools.

## Prove that it breathes

Build both binaries, then exercise the complete TCP path, malformed requests, error responses, query routing, UTF-8 byte lengths, a concurrent burst, and the built-in benchmark:

```bash
cargo build --locked --bins
bash scripts/smoke.sh cpu target/debug/gput
bash scripts/smoke.sh gpu target/debug/gput
```

The CI GPU smoke forces `wgpu` through Vulkan and Mesa Lavapipe. That executes the real upload, compute dispatch, shader parser/router, UTF-8 writer, readback, and socket response path on a deterministic software Vulkan adapter. It proves the GPU code path works end to end, but it is not a substitute for performance measurements on a discrete AMD, Intel, or NVIDIA GPU.

CI runs:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo build --locked --bins
bash scripts/smoke.sh cpu target/debug/gput
# plus the same smoke and benchmark suite against Vulkan/Lavapipe
```

A red direct-to-main build opens an issue containing the failure log. The next green build closes superseded CI failure issues automatically.

## Development rule

Work goes directly into `main`. No mandatory branches, no default pull requests, no velvet rope. Commits should still be coherent, tested, and easy to revert. See [`AGENTS.md`](AGENTS.md).

## Honest limitations

This is not a production web server. It currently has no TLS, keep-alive, request bodies, HTTP/2, HTTP/3, dynamic route table, mutable GPU string heap, persistent mapped ring buffers, or zero-copy NIC integration. The first objective is to obtain defensible CPU-versus-GPU measurements without hiding upload, dispatch, synchronization, and readback costs.

The sensible future endpoint is one where parsing and routing lead directly into GPU-native work such as image processing, hashing, vector search, or inference. Until then, this is a very polished electrical joke.
