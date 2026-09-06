# HTTP hot path: less work, same processor boundary

## Declaring routes

```rust
use gput::{Body, Response, Router};

let app = Router::new()
    .get("/plaintext", "Hello, World!\n")
    .get("/query", Body::new().push("query=").query(64))
    .get("/json", Response::json("{\"ok\":true}"));
```

`&str`, `String`, and `Body` convert to text responses. Explicit `Response` values retain their status and content type. The existing `.route(path, routing::get(response))` API remains supported; `routing::get` accepts the same conversions. There are no host closures or per-request Rust callbacks hiding behind this syntax.

Adjacent literals are merged when building a body. Zero-byte path/query operations and backend variants with identical values fold away. Dynamic operations remain bounded and retain their order. Both processors consume the resulting program; no alternative CPU route implementation is introduced.

## Writing words instead of repeatedly editing bytes

One compute invocation owns one complete response slot. Its `Writer` retains a partially assembled `u32` in function-local storage. Complete words are stored once; the final partial word is flushed with zero padding. The shader never reads old output bytes, so a shorter response can safely reuse a longer response's slot.

Literal and request-range copies use four-byte chunks plus a bounded tail. Both the immutable string arena and request ranges can be unaligned; complete-word loads join adjacent words only when four logical bytes remain. Aligned loads take a separate branch, avoiding a shift by 32. Route lookup still performs exact comparison after its hash search, including collisions.

Literal strings already passed Rust's UTF-8 invariant before being uploaded. Decoding and re-encoding them for every request adds no validation of request data, so the shader copies their original bytes instead. Request-derived path/query operations retain their existing raw-byte limits; they do not gain URL decoding, JSON escaping, or Unicode-boundary truncation. `Response::json` sets a content type, not an escaping policy.

At the WGSL source level, response output now needs approximately `ceil(bytes / 4)` stores and no output loads, rather than one read-modify-write per byte. This is not a claim of four times higher HTTP throughput: compiler transformations, transfer costs, dispatch, occupancy, readback, and the socket path still matter.

## Batching without mandatory sleeping

`--batch-wait-micros 0` drains already queued work up to the configured batch size, then dispatches without waiting for more. Previously the deadline was checked before reading the queue, which reduced zero-wait batches to one request even with queued work.

Positive waits still have a bounded idle collection budget. Ready work is drained without consulting a timer for each item. Queue order and batch capacity are preserved. Collection timing stops before processor timing begins.

## Correctness gates

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
# Requires an actual wgpu adapter; a missing adapter fails this command.
cargo test --locked --test gpu_writer -- --ignored
```

Pull-request CI executes the shader through Lavapipe, compares full HTTP responses against the CPU reference, and runs socket smoke tests for both processors. Coverage includes unaligned sources, one- to four-byte Unicode scalars, embedded NUL, bounded dynamic data, hash collisions, status codes, Content-Length, partial workgroups, and buffer reuse across changing batch sizes. Lavapipe is a software Vulkan implementation, not evidence of discrete-GPU performance. Metal and hardware Vulkan should run the same differential test before publishing hardware results.

## Measuring the change

Compare the PR against `6c5b14cacc35f24b1f34b27eb9b9c68053dcbe35` on the same machine, driver, power state, release profile, and command lines. Use separate worktrees and output directories; alternate baseline/candidate order. Keep the load-generator binary identical for both runs. Record the server's adapter log, backend, commit, batch size, wait, concurrency, pipeline depth, response bytes, and errors.

For each revision, build with `cargo build --locked --release --bins`. Run one server at a time, initially with:

```sh
target/release/gput --backend gpu --bind 127.0.0.1:8080 \
  --batch-size 256 --batch-wait-micros 0 --queue-depth 8192 \
  --max-connections 2048
```

From the same fixed load-generator build, run:

```sh
target/release/gput-bench suite --address 127.0.0.1:8080 \
  --path /health --requests 100000 --warmup 10000 \
  --suite-concurrency 1,16,64,256,1024 --pipeline 1 \
  --repeats 5 --expected-backend gpu --label candidate --json
```

Repeat with `/utf8` and `/inspect?owl=yes`, pipeline depths 1 and 16, and waits 0, 100, and 1000 microseconds. The small `/health` response exposes transport and dispatch overhead. For `/plaintext`, the updated `hello_gpu` example explicitly declares that route; do not accidentally time a 404 from a server lacking it. Check status, body, and backend before timing any custom route.

Report median throughput and tail latency together, including regressions and failures. Repeat equivalent workloads against the CPU backend and competitive CPU servers; omit `--expected-backend` for external servers. Use a separate load-generation machine for competitive network measurements and verify it is not saturated. Same-host loopback is a development comparison, not a world record.

This change does not pipeline multiple in-flight GPU dispatches, bypass the kernel/NIC transport, or rewrite the experimental raw-packet TCP engine. Those remain separate optimization questions to answer with profiles. The fastest-in-the-world hypothesis still needs a stopwatch.
