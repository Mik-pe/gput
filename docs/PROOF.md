# Proving the GPU path without selling snake oil

`gput` makes three different claims. They should not be mixed together.

1. **Correctness:** a compute shader can own useful HTTP and TCP work.
2. **Portability:** the same WGSL packet state machine can compile for Metal and Vulkan.
3. **Performance:** real hardware may beat a CPU reference or reduce CPU work when enough independent packets are batched.

CI proves the first claim and continuously checks the second. Performance is deliberately measured on the machine making the claim, with the adapter and workload printed beside the result.

## Proof layer 1: protocol semantics in compute

```bash
cargo run --release --locked --bin gput-packet-demo
```

The demo does not use a server socket. It injects raw IPv4/TCP packets into `GpuPacketEngine` and verifies:

- SYN, SYN-ACK and ACK establishment
- repeated SYN recovery with a stable server sequence number
- persistent per-flow state across dispatches
- duplicate data retransmission recovery
- `200`, `404` and `405` HTTP routing
- `/health` and `/plaintext`
- collision-safe open-addressed flow lookup using two real colliding flow hashes
- FIN acknowledgement and flow release
- IPv4 and TCP checksums on every generated packet

The process exits non-zero if any packet or sequence number lies.

## Proof layer 2: a real operating-system TCP client

On Linux, `gput-packetd` creates a TUN interface. The kernel routes client packets into that interface, but it does not terminate the server-side TCP connection. The shader owns the server sequence state and emits the response packets.

```bash
cargo build --release --locked --bin gput-packetd
sudo ./target/release/gput-packetd \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080 \
  --gpu-batch-capacity 256 \
  --batch-wait-micros 50
```

From another terminal:

```bash
curl --noproxy '*' -i http://10.77.0.2:8080/plaintext
curl --noproxy '*' -i http://10.77.0.2:8080/health
```

The response identifies itself with `X-Gput-Backend: gpu-packet`. Linux CI performs this path with a real `curl` and also drives persistent requests through `gput-bench`.

## Proof layer 3: the same packet workload on CPU and GPU

```bash
cargo run --release --locked --bin gput-packet-bench -- \
  --backend both \
  --flows 4096 \
  --requests-per-flow 1000 \
  --warmup-requests-per-flow 20 \
  --batch-size 256 \
  --flow-capacity 16384 \
  --flow-probe-limit 64
```

Both backends receive the same generated raw packets and implement the same deliberately narrow TCP/HTTP semantics. The CPU reference is intentionally straightforward and single-threaded. It is a semantic and crossover baseline, not a claim that `HashMap` plus one core represents the best possible CPU network stack.

The report separates:

- **engine requests/s:** time spent inside `PacketEngine::process_batch`
- **end-to-end requests/s:** packet construction, validation and engine time
- **wire packets/s:** request and response packets represented by the measured HTTP exchanges
- **response MiB/s**
- **p50/p99 batch-round latency**
- **dispatch fill:** packets processed per GPU dispatch
- **handshake flows/s**
- **adapter name**

JSON output is available with `--json`.

## One-command local proof

```bash
./scripts/prove-gpu.sh
```

Environment variables can enlarge the arena:

```bash
GPUT_PROOF_FLOWS=8192 \
GPUT_PROOF_REQUESTS_PER_FLOW=2000 \
GPUT_PROOF_BATCH_SIZE=512 \
./scripts/prove-gpu.sh
```

Run it several times on an otherwise idle machine. Publish the full command, GPU, CPU, driver, OS, power mode and all runs. Do not publish only the luckiest number.

## What a green CI badge means

A green Linux CI run means:

- formatting, Clippy, tests and all binaries passed
- the normal CPU and GPU socket modes passed
- the packet shader parsed and validated
- the GPU packet demo passed through Vulkan/Lavapipe
- a real `curl` traversed TUN into the GPU-owned TCP path
- the CPU and GPU raw-packet benchmark completed and validated every response

A green macOS status means the full Rust surface and packet binaries build against macOS APIs and the WGSL packet semantics validate. Hosted macOS CI is not presented as a physical Apple-GPU performance result.

## Claims not yet earned

The current implementation does not claim production TCP, internet-facing safety, zero-copy NIC DMA, congestion-control parity with the kernel, or a universal GPU win. Receive windows, bounded out-of-order reassembly, retransmission timers, SYN-flood protection, IPv6 and direct NIC transports remain separate milestones.

What is earned is narrower and still delightfully strange: a vendor-neutral compute shader can own persistent TCP flow state, parse HTTP, route requests, recover common retransmissions, generate checksummed packets and serve a real TCP client. The benchmark tells us where that architecture becomes faster instead of asking anyone to believe a mascot.
