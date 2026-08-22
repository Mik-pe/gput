# gput

[![ci](https://github.com/Mik-pe/gput/actions/workflows/ci.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/ci.yml)
[![packet proof](https://github.com/Mik-pe/gput/actions/workflows/packet-proof.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/packet-proof.yml)
[![macOS](https://github.com/Mik-pe/gput/actions/workflows/macos.yml/badge.svg)](https://github.com/Mik-pe/gput/actions/workflows/macos.yml)

> HTTP, TCP, UTF-8, and other things WGSL was never emotionally prepared for.

`gput` is an experimental Rust web framework and tiny TCP fast path built from GPU compute shaders, bounded buffers, bit shifts, atomics, and a frankly unreasonable amount of stubbornness.

The CPU may still open doors, move packet batches, and make coffee. On the honest GPU paths, request parsing, routing, response construction, TCP flow state, and packet checksums happen in actual compute dispatches. No CUDA is required. No fake GPU moustache is permitted.

The project is a joke. The measurements are not.

## Five-minute bad decision

The repository ships local Cargo aliases, so the first encounter no longer requires memorising the names of six binaries.

```bash
cargo doctor
```

`cargo doctor` finds the selected adapter, executes a real GPU HTTP request, performs a raw SYN/ACK/HTTP exchange through the packet shader, validates the checksums, and tells you whether the adapter is hardware or software. Lavapipe is useful proof, but it does not receive a tiny benchmark trophy.

Run the ordinary socket server:

```bash
cargo serve --backend gpu
curl -i http://127.0.0.1:8080/plaintext
```

Run a small custom application:

```bash
cargo hello-gpu
curl -i 'http://127.0.0.1:8080/inspect?owl=yes'
```

Prove the raw packet state machine without creating a TUN interface:

```bash
cargo prove-gpu
```

Compare identical raw IPv4/TCP conversations on the CPU reference and GPU engine:

```bash
cargo packet-bench --backend both
```

## Write normal-looking server code, produce abnormal machinery

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
                    .path(128)
                    .push("\nquery=")
                    .query(128)
                    .push("\nbackend=")
                    .backend()
                    .push("\npath_hash=")
                    .path_hash()
                    .push("\n"),
            )),
        );

    gput::serve(ServerConfig::default(), app).await
}
```

This API is Axum-inspired, not Axum-compatible. Builders execute once at startup and compile the application into:

```text
exact-path route table
response descriptors
bounded body bytecode
UTF-8 string arena
```

The compute shader parses `GET /path?query HTTP/1.1`, performs collision-safe route lookup, interprets the response program, formats decimal lengths, and writes the complete HTTP response.

WGSL has no string type, so gput manufactures one from storage buffers, scalar validation, shifts, masks, and professional-grade stubbornness. It is not elegant in the conventional sense. It is, however, bounds checked.

See [`examples/hello_gpu.rs`](examples/hello_gpu.rs) for the complete tiny application.

## Choose your preferred level of poor judgement

### 1. GPU application logic behind normal TCP

```text
Linux/macOS TCP
      |
CPU frames complete requests
      |
GPU parses, routes, and builds HTTP
      |
CPU writes returned bytes
```

This is the portable everyday mode. It supports persistent HTTP/1.1 connections, basic pipelining, exact static routes, dynamic bounded response segments, JSON/HTML/text responses, a CPU baseline, and automatic fallback.

```bash
cargo serve --backend auto
```

### 2. GPU-owned TCP through TUN

```text
curl
  |
kernel route
  |
TUN raw IPv4 packets
  |
GPU TCP state machine and HTTP router
  |
checksummed IPv4/TCP response packets
  |
TUN
```

Start the packet daemon:

```bash
sudo cargo packetd \
  --backend gpu \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080 \
  --batch-capacity 256 \
  --batch-wait-micros 50
```

Then:

```bash
curl --noproxy '*' -i http://10.77.0.2:8080/plaintext
curl --noproxy '*' -i http://10.77.0.2:8080/health
```

The packet daemon also accepts `--backend cpu` and `--backend auto`. Auto tries the GPU, falls back to the CPU reference when necessary, prints the reason, and keeps the response header honest:

```text
X-Gput-Backend: gpu-packet
```

or:

```text
X-Gput-Backend: cpu-packet
```

That makes the same TUN application useful on machines without a working compute adapter, and makes CPU/GPU comparison less dependent on two unrelated programs.

### 3. Future NIC-to-GPU lunacy

The packet engine is deliberately separated from packet transport:

```text
RawPacket batch -> PacketEngine -> RawPacket batch
```

Today the source is TUN. Nearer-term Linux work can use AF_XDP. The AMD endgame is a HIP/ROCm XIO transport with compatible NIC queues and GPU-side doorbells. Apple Silicon remains a compelling unified-memory hybrid because public macOS APIs do not expose a general NIC-to-Metal DMA queue.

The TCP semantics do not get rewritten for every vendor. Transport adapters must feed the same conversations to the same contract, or explain themselves to the test suite.

## Is any of this useful?

Surprisingly, yes, in narrow places:

- GPU-heavy gateways where parsing, classification, inference, image work, hashing, or vector operations should continue without bouncing through several CPU abstractions
- experiments that measure how much networking machinery can follow application compute onto the accelerator
- deterministic CPU-versus-GPU crossover research using identical packet conversations
- teaching protocol state machines, GPU memory models, atomics, and the hidden amount of work inside a boring HTTP response
- making serious engineers briefly stare into the middle distance

It is not a sensible replacement for Axum plus a database. It becomes interesting when thousands of independent requests already want GPU work.

## Proof, not vibes

Run the local proof suite:

```bash
./scripts/prove-gpu.sh
```

Or enlarge the arena while keeping one full independent-flow round in each dispatch:

```bash
GPUT_PROOF_FLOWS=131072 \
GPUT_PROOF_REQUESTS_PER_FLOW=2000 \
GPUT_PROOF_BATCH_SIZE=131072 \
GPUT_PROOF_FLOW_CAPACITY=262144 \
./scripts/prove-gpu.sh
```

The suite validates CPU and GPU packet semantics, retransmitted SYNs, duplicate data, hash collisions, checksums, HTTP statuses, sequence numbers, FIN cleanup, and throughput. The benchmark reports engine RPS, end-to-end RPS, represented wire packets/s, response MiB/s, handshake flows/s, p50/p99 round latency, packets per dispatch, adapter name, per-stage timing, and the GPU-to-CPU-reference ratio.

The CPU packet engine is a deliberately straightforward single-threaded semantic reference. It is not Linux TCP, DPDK, AF_XDP, or the fastest CPU implementation we could write. A public victory claim also needs repeated hardware runs and an independent load generator. Fastest in the world remains a hypothesis with a benchmark harness attached, not a sticker we found behind the sofa.

See [the proof contract](docs/PROOF.md) and [benchmark rules](BENCHMARKING.md).

## What currently works

- Axum-style `Router`, `routing::get`, `Response`, and bounded `Body` programs
- exact collision-safe GPU routing
- complete HTTP response construction in WGSL
- cursed UTF-8 strings with malformed-input rejection
- persistent HTTP/1.1 and basic pipelining on the socket path
- CPU and GPU application processors consuming the same compiled router
- raw IPv4/TCP packet parsing in compute
- atomically claimed, collision-safe GPU flow slots
- SYN, ACK, persistent sequence state, duplicate SYN/data recovery, FIN, and RST cleanup
- `/plaintext`, `/health`, `400`, `404`, and `405` on the packet path
- GPU-generated IPv4 and TCP checksums, with precomputed static payload sums
- word-packed HTTP response templates copied four bytes at a time instead of knitted byte by byte
- compact request and packet uploads instead of padding every item to its maximum slot size
- atomic flow-slot claims with ordinary per-flow sequence fields once ownership is established
- ordered dispatch waves for packets belonging to the same flow
- batched TUN ingress with packet/dispatch telemetry
- CPU packet reference and identical-workload packet benchmark
- Linux TUN-to-GPU `curl` coverage and macOS build coverage

## The stubbornness stack

```text
WGSL has no strings
  -> byte arena + metadata + UTF-8 validation

WGSL has no sockets
  -> raw packets + flow table + checksums

GPU threads dislike serial dependencies
  -> independent-flow batching + ordered per-flow waves

The NIC cannot portably DMA into wgpu buffers
  -> TUN today, transport seam tomorrow

Somebody asks whether this should exist
  -> additional test coverage
```

## Honest limitations

This is not a production web server or an RFC-complete TCP stack.

The socket path has no TLS, request bodies, HTTP/2, HTTP/3, path parameters, arbitrary middleware, mutable GPU heap, or zero-copy NIC integration. The packet path still lacks retransmission timers, congestion control, receive-window accounting, bounded out-of-order reassembly, SYN-flood protection, IPv6, PMTU handling, and stream reassembly for several HTTP requests carried by one TCP segment.

TUN still crosses the host networking boundary. Software Vulkan proves semantics, not speed. A GPU result is only a GPU result when the selected adapter, driver, commands, and repeated measurements are published beside it.

## More entrails

- [GPU networking architecture](docs/GPU_NETWORKING.md)
- [Proof and claim boundaries](docs/PROOF.md)
- [Benchmark methodology](BENCHMARKING.md)
- [Repository rules](AGENTS.md)

Work normally goes directly to `main`. The repository is an experiment, not a parliament. Commits should still be coherent enough that future us can identify which particular act of stubbornness broke the owl. 🦉
