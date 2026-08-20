# gput

**GPU throughput applied to the least deserving workload imaginable.**

`gput` is an experimental HTTP/1.1 server written in Rust. The CPU accepts TCP connections and batches raw request bytes. A GPU compute shader then parses the request line, selects a route, and writes the complete HTTP response back into a storage buffer.

```text
TCP socket
    |
    v
minimal Rust framing
    |
    v
request batch
    |
    v
wgpu compute dispatch
    |  parse GET /path HTTP/1.1
    |  route by FNV-1a hash
    |  write status, headers, and body
    v
readback buffer
    |
    v
TCP socket
```

The name is the union of **GPU** and **throughput**, with enough resemblance to an unfortunate Unix command to be trustworthy.

## What works

- Real TCP listener built on Tokio
- bounded request queue with micro-batching
- headless compute through `wgpu`
- raw HTTP request-line parsing in WGSL
- hashed routing in WGSL
- complete HTTP responses generated in WGSL
- CPU baseline backend
- automatic CPU fallback when no compute-capable adapter is available
- request size, connection count, queue depth, and slow-client limits
- unit tests for protocol behavior, batching, configuration, and WGSL validation

Current routes:

| Route | Response |
| --- | --- |
| `/` | project metadata as JSON |
| `/health` | `ok` |
| `/hello` | proof that the response came from the selected processor |
| anything else | `404 Not Found` |

Only `GET` is accepted. Each connection handles one request and closes. That is deliberate while the GPU data path is being measured.

## Run it

```bash
cargo run --release -- --backend auto --bind 127.0.0.1:8080
```

Then:

```bash
curl -i http://127.0.0.1:8080/
curl -i http://127.0.0.1:8080/health
curl -i http://127.0.0.1:8080/hello
```

Force a backend:

```bash
cargo run --release -- --backend gpu
cargo run --release -- --backend cpu
```

Useful tuning knobs:

```bash
cargo run --release -- \
  --backend gpu \
  --batch-size 256 \
  --batch-wait-micros 50 \
  --queue-depth 8192 \
  --max-request-bytes 4096
```

`WGPU_BACKEND` and `WGPU_ADAPTER_NAME` can be used to influence adapter selection.

## Development rule

Work goes directly into `main`. No mandatory branches, no default pull requests, no velvet rope. Commits should still be coherent, tested, and easy to revert. See [`AGENTS.md`](AGENTS.md).

## Honest limitations

This is not a production web server. It currently has no TLS, keep-alive, request bodies, HTTP/2, HTTP/3, dynamic route table, persistent mapped ring buffers, or zero-copy NIC integration. The first objective is to obtain defensible CPU-versus-GPU measurements without hiding the expensive upload, dispatch, synchronization, and readback steps.

The sensible future endpoint is one where parsing and routing lead directly into GPU-native work such as image processing, hashing, vector search, or inference. Until then, this is a very polished electrical joke.
