# Benchmarking gput without lying to ourselves

`gput-bench` is both a quick local load generator and a neutral little referee that can point at another HTTP server. The interesting number is not the luckiest run. The suite reports the median across repeats and sweeps persistent HTTP/1.1 connections across several concurrency levels.

The `gput` binary exposes `/plaintext` with the body `Hello, World!` specifically so boring HTTP throughput can be measured before reaching for workloads that flatter the GPU.

## Start gput

```bash
cargo run --release --locked -- \
  --backend gpu \
  --bind 127.0.0.1:8080 \
  --batch-size 256 \
  --batch-wait-micros 50
```

## One measurement

```bash
cargo run --release --locked --bin gput-bench -- \
  --address 127.0.0.1:8080 \
  --path /plaintext \
  --requests 100000 \
  --concurrency 256 \
  --pipeline 1 \
  --expected-backend gpu
```

The client keeps one TCP connection per worker alive for the phase. `--pipeline N` writes up to `N` requests before reading the corresponding responses, preserving response order and measuring latency from the start of each pipeline burst.

## The useful command

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

This prints median requests/second, best requests/second, and median p99 latency for each concurrency level. JSON output is available with `--json`.

## Smack another server with the same ruler

Run the competitor on another port with a `GET /plaintext` endpoint that returns a fixed body and a correct `Content-Length`, then point the exact same client at it:

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

Do not pass `--expected-backend` for non-gput servers. Keep the machine, request count, concurrency sweep, pipeline depth, CPU affinity, power profile, and background load identical between contestants.

For public bragging rights, corroborate the result with an independent load generator such as `wrk` or `oha`. `gput-bench` is intentionally dependency-light and excellent for repeatable repo-local comparisons, but the defendant should not also be the only judge.

## What the suite is trying to reveal

Plaintext answers whether the network/framing/GPU round trip is competitive at all. Then point the suite at `/hello`, `/utf8`, or `/inspect?owl=bench` to see how the crossover moves as response work becomes more interesting. A future GPU-native hashing, vector, image, or inference endpoint should use the same harness rather than inventing a friendlier benchmark.
