# Benchmarking gput without lying to ourselves

`gput` now has two separate benchmark arenas. Keep them separate because they answer different questions.

- `gput-bench` measures real HTTP clients against a listening server.
- `gput-packet-bench` feeds identical raw IPv4/TCP packets to a single-threaded CPU reference and the GPU packet engine.

The interesting number is not the luckiest run. Warm up first, repeat the measurement, publish latency beside throughput, and include the machine configuration.

## Socket-level HTTP benchmark

The `gput` binary exposes `/plaintext` with exactly `Hello, World!` so transport and dispatch overhead can be measured before selecting work that flatters the GPU.

Start gput:

```bash
cargo run --release --locked -- \
  --backend gpu \
  --bind 127.0.0.1:8080 \
  --batch-size 256 \
  --batch-wait-micros 50
```

Run the concurrency sweep:

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

This reports median requests/second, best requests/second and median p99 latency for each concurrency level. JSON output is available with `--json`.

### Smack another server with the same ruler

Run a competitor on another port with the same `/plaintext` body and a correct `Content-Length`, then point the exact same client at it:

```bash
cargo run --release --locked --bin gput-bench -- suite \
  --address 127.0.0.1:3000 \
  --path /plaintext \
  --requests 100000 \
  --suite-concurrency 1,16,64,256,1024 \
  --repeats 5 \
  --pipeline 1 \
  --label axum
```

Do not pass `--expected-backend` for non-gput servers. Keep request count, concurrency, pipeline depth, CPU affinity, power profile and background load identical. Corroborate public claims with an independent generator such as `wrk` or `oha`.

## Raw-packet CPU versus GPU benchmark

The packet benchmark removes the kernel server socket from the measured engine and exercises the same packet-level state machine on both backends:

```bash
cargo run --release --locked --bin gput-packet-bench -- \
  --backend both \
  --flows 65536 \
  --requests-per-flow 1000 \
  --warmup-requests-per-flow 20 \
  --batch-size 65536 \
  --flow-capacity 131072 \
  --flow-probe-limit 64
```

Every flow performs a raw SYN handshake, repeated persistent `/plaintext` exchanges and FIN cleanup. Every emitted IPv4/TCP checksum, sequence number, acknowledgement, status and body is validated outside the timed engine section.

The throughput profile keeps one independent-flow round in one dispatch. Reducing `--batch-size` is a useful latency and underfilled-ingress experiment, but it mostly measures repeated submit/readback overhead rather than saturated GPU packet work.

The default 65,536-flow profile is the measured throughput knee on an Apple M5. It is intentionally a saturation profile; publish smaller-batch latency results alongside it when latency matters.

The CPU reference is deliberately straightforward and single-threaded. It exists to answer the crossover question for this exact state machine. It is not a substitute for comparisons against an optimized kernel stack, DPDK, AF_XDP or a production framework.

The report includes:

- engine requests/s
- full harness requests/s
- represented wire packets/s
- response MiB/s
- p50 and p99 batch-round latency
- packets per GPU dispatch
- handshake flows/s
- adapter name
- per-packet scheduler, packing, upload, submit, GPU/readback and decode timing
- GPU-to-CPU-reference ratio when both are run

Use `--json` for machine-readable results.

### Apple M5 reference result (2026-08-22)

Hardware was a 10-core CPU / 10-core GPU Apple M5 with 24 GiB unified memory, running macOS 26.5 on AC power with Low Power Mode disabled. Metal debug environment overrides were removed. Each paired run processed 32,768,000 requests per backend:

```bash
env -u MTL_DEBUG_LAYER -u METAL_DEVICE_WRAPPER_TYPE \
  target/release/gput-packet-bench \
  --backend both \
  --flows 65536 \
  --requests-per-flow 500 \
  --warmup-requests-per-flow 50 \
  --batch-size 65536 \
  --flow-capacity 131072 \
  --flow-probe-limit 64 \
  --json
```

| Run | CPU reference req/s | GPU engine req/s before direct staging |
| ---: | ---: | ---: |
| 1 | 7,262,403 | 17,130,889 |
| 2 | 8,142,849 | 17,582,607 |
| 3 | 7,290,237 | 17,131,692 |
| 4 | 8,165,115 | 15,260,652 |
| 5 | 4,440,312 | 16,823,961 |
| **Median** | **7,290,237** | **17,130,889** |

The GPU median is 2.35x the single-threaded CPU semantic reference, represents 34.26 million request-plus-response packets/s, and has 3.82 ms p50 / 4.13 ms p99 batch-round latency. Full-harness GPU throughput, including packet construction and response validation, was 5.37 million requests/s. The slower runs remain visible rather than being polished into the floorboards.

This is a synthetic in-memory raw-packet engine result, not NIC throughput and not a socket HTTP result.

Packing packet bytes directly into `wgpu` upload staging removed a full host-side copy. Five GPU-only follow-up runs with the same 65,536-flow shape and 32,768,000 requests each produced:

| Run | GPU engine req/s |
| ---: | ---: |
| 1 | 17,995,502 |
| 2 | 18,216,351 |
| 3 | 18,970,775 |
| 4 | 17,898,723 |
| 5 | 18,278,389 |
| **Median** | **18,216,351** |

The follow-up median represents 36.43 million request-plus-response packets/s with 3.49 ms p50 / 5.05 ms p99 batch-round latency. Full-harness median throughput was 4.89 million requests/s. The engine median improved 6.3% over the paired pre-staging GPU result without changing the packet conversation or timed validation boundary.

## Benchmark the real TUN path

Start `gput-packetd`, then point `gput-bench` at the virtual peer with pipeline depth one:

```bash
sudo ./target/release/gput-packetd \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080 \
  --gpu-batch-capacity 256 \
  --batch-wait-micros 50
```

```bash
cargo run --release --locked --bin gput-bench -- \
  --address 10.77.0.2:8080 \
  --path /plaintext \
  --requests 100000 \
  --concurrency 256 \
  --pipeline 1
```

The packet fast path currently accepts one HTTP request per in-order TCP payload segment. Keep `--pipeline 1` until stream reassembly can split several pipelined requests carried by the same segment.

## Evidence checklist

For a result worth putting in a README, record:

- exact commit
- command line and all environment variables
- CPU and GPU model
- GPU driver and backend
- operating system and kernel
- power/performance mode
- CPU affinity and core count available to each process
- whether load generation shared the server machine
- all repeated runs, not only the peak
- throughput and p99 latency

See [docs/PROOF.md](docs/PROOF.md) for the correctness and portability proof pyramid. The goal is to find where the curves cross, not to invent a workload where the answer was decided before the benchmark started.
