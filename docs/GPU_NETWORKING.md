# GPU-native networking

`gput` treats packet IO and TCP/HTTP semantics as separate problems.

The portable contract is:

```text
packet source
    |
    v
RawPacket batch
    |
    v
PacketEngine
    |  IPv4 + TCP parsing
    |  persistent flow state
    |  HTTP request-line routing
    |  response packet generation
    |  IPv4/TCP checksums
    v
RawPacket batch
    |
    v
packet sink
```

`GpuPacketEngine` implements that contract in vendor-neutral WGSL through `wgpu`. `CpuPacketEngine` is a deliberately direct single-threaded semantic reference used for validation and crossover measurements. Packet transports can change without forking TCP semantics.

## What is real today

The GPU packet engine now implements a narrow but useful TCP fast path:

- IPv4 with variable header length and fragmented datagrams rejected
- TCP with ingress options tolerated through the data-offset field
- one configurable listening port
- SYN, SYN-ACK and ACK establishment
- stable SYN retransmission recovery
- collision-safe open-addressed flow lookup using the complete IPv4 four-tuple
- atomically claimed flow slots and tombstones for released slots
- persistent receive and send sequence state in GPU storage
- in-order payload validation
- duplicate data detection with retransmission of the last response
- acknowledgement of unexpected sequence numbers
- reset and FIN flow cleanup
- `/plaintext` and `/health`
- `400 Bad Request`, `404 Not Found` and `405 Method Not Allowed`
- query strings ignored for exact path selection
- IPv4 and TCP checksum generation on the GPU
- batching of independent flows in one dispatch
- ordered dispatch waves when several packets from the same flow arrive together

This is enough for a real TCP client to establish a persistent connection and issue repeated HTTP requests through the compute shader. It is not an RFC-complete or internet-safe TCP implementation.

## Three ways to exercise it

### Synthetic protocol proof

```bash
cargo run --release --locked --bin gput-packet-demo
```

The demo verifies checksums, sequence numbers, retransmitted SYNs, duplicated HTTP segments, status routing, FIN cleanup and two simultaneously active flows whose hashes select the same initial slot.

### Real TUN traffic

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

The request does not terminate in a normal server TCP socket. The host kernel routes the client packet into TUN, `gput-packetd` batches raw packets, the shader owns server-side flow state, and generated packets are injected back into TUN.

`gput-packetd` reports incoming/outgoing packets, packets/s, average and peak ingress batch, GPU dispatches and packets per dispatch.

### Same packet workload on CPU and GPU

```bash
cargo run --release --locked --bin gput-packet-bench -- \
  --backend both \
  --flows 4096 \
  --requests-per-flow 1000 \
  --batch-size 256 \
  --flow-capacity 16384 \
  --flow-probe-limit 64
```

The benchmark constructs and validates identical raw packet conversations for both engines. It reports engine-only throughput separately from full harness throughput so packet generation and correctness checks do not disappear into a heroic number.

See [PROOF.md](PROOF.md) for the evidence contract.

## Batching model

A GPU dispatch may process many independent flows. Packets from one TCP flow are serial by definition, so `GpuPacketEngine::process_batch` schedules them into ordered waves:

```text
input batch:
  A1 B1 A2 C1 B2

GPU wave 1:
  A1 B1 C1

GPU wave 2:
  A2 B2
```

Different flows run in parallel; the ordering dependency within each flow remains explicit. The TUN adapter gathers packets for a small configurable window before calling the engine, which finally gives the GPU a chance to eat a meal rather than being served one byte canapé per dispatch.

## Flow-table design

Each GPU flow slot stores:

```text
state + generation
full source/destination IPv4 addresses
source/destination ports
receive-next
send-next
send-unacknowledged
last client segment coordinates
last emitted response coordinates
```

The FNV hash selects an initial slot only. Bounded linear probing and exact four-tuple comparison decide identity. Slots are claimed with storage atomics, and released entries become tombstones so deleting one connection cannot make a later colliding flow unreachable.

The probe bound is configurable. Exhausting it drops the new flow rather than corrupting another connection.

## macOS

The same WGSL packet engine compiles through Metal on macOS. The portable command-line bridge uses the cross-platform TUN boundary; an eventual packaged app can replace it with `NEPacketTunnelProvider.packetFlow` while keeping the compute state machine intact.

Apple Silicon does not expose a public general-purpose NIC-to-Metal DMA queue. macOS is therefore the excellent unified-memory hybrid and portability backend, not currently the zero-CPU NIC backend. The macOS workflow builds every packet binary and validates shader semantics without presenting a hosted runner as a performance result.

## AMD Linux

There are three concrete stages:

1. **Runnable now:** TUN feeds the WGSL engine over Vulkan on an AMD GPU.
2. **Kernel-bypass:** AF_XDP owns packet rings while the same engine contract is retained.
3. **Direct target:** a HIP/ROCm XIO transport owns compatible NIC queue memory and GPU-side doorbells.

ROCm XIO is a transport building block, not a transparent TCP socket. Keeping `RawPacket -> PacketEngine -> RawPacket` as the seam allows direct IO to arrive without replacing the protocol state machine or application API.

## Transport map

```text
macOS TUN / NetworkExtension -> Metal/wgpu packet engine
Linux TUN                    -> Vulkan/wgpu packet engine
Linux AF_XDP                 -> packet-ring adapter
AMD ROCm XIO                 -> HIP/XIO direct packet adapter
NVIDIA DOCA                  -> CUDA/DOCA direct packet adapter
```

The WGSL implementation remains the portable reference semantics. Vendor kernels may optimize the ring and protocol implementation, but they must be tested against the same packet conversations rather than quietly becoming separate stacks.

## Remaining TCP work

The next correctness milestones are:

1. retransmission timers and an explicit unacknowledged-send queue
2. receive-window accounting
3. bounded out-of-order segment storage and reassembly
4. duplicate ACK tracking and a congestion-control policy
5. SYN cookies or another bounded SYN-flood defense
6. IPv6
7. MTU discovery and fragmentation policy
8. HTTP stream reassembly for several requests in one TCP segment

The next transport milestones are:

1. persistent mapped host/GPU packet rings
2. multi-queue TUN/AF_XDP ingestion
3. CPU-cycle and energy accounting beside RPS
4. AF_XDP on Linux
5. a packaged macOS NetworkExtension host
6. AMD ROCm XIO on compatible hardware

The point is not to pretend the kernel is obsolete. The point is to make the boundary measurable: when thousands of independent flows already need GPU work, how much transport and protocol machinery can follow them onto the device before the crossover becomes undeniable?
