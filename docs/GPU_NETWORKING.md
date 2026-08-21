# GPU-native networking

`gput` treats packet IO and the TCP/HTTP engine as separate problems.

The core contract is `RawPacket -> GpuPacketEngine -> RawPacket`. The compute engine accepts batches of raw IPv4 packets and returns zero or one IPv4 packet per input packet. It does not know whether packets came from a virtual interface, an AMD queue, or a future NIC that DMA-writes a shader-visible ring.

```text
packet source
    |
    v
RawPacket batch
    |
    v
GpuPacketEngine
    |  IPv4 + TCP parsing
    |  persistent flow state on GPU
    |  SYN -> SYN/ACK
    |  ACK -> ESTABLISHED
    |  GET /plaintext -> HTTP response
    |  FIN -> ACK
    |  IPv4/TCP checksum generation
    v
RawPacket batch
    |
    v
packet sink
```

## What is real today

The packet engine is vendor-neutral WGSL running through `wgpu`, so the same protocol shader can execute through Metal on Apple Silicon and Vulkan on AMD, NVIDIA, or Intel. TCP state survives across dispatches in a GPU storage buffer.

The first vertical slice implements a deliberately tiny TCP fast path:

- IPv4 without IP options
- TCP with ingress TCP options tolerated via the data-offset field
- one listening port, default `8080`
- SYN/SYN-ACK/ACK establishment
- established `GET /plaintext`
- in-order client sequence validation
- persistent server sequence state
- FIN acknowledgement and flow release
- IPv4 and TCP checksum generation on the GPU
- fixed hash-addressed flow slots

There are now two ways to exercise it.

`gput-packet-demo` injects synthetic packets directly into the compute engine, completes the handshake, validates both checksums, requests `/plaintext`, and closes the flow:

```bash
cargo run --release --locked --bin gput-packet-demo
```

`gput-packetd` creates a real L3 TUN interface and forwards its IP packets through the same GPU engine. Build it, then run it with privileges required to create/configure a TUN device:

```bash
cargo build --release --locked --bin gput-packetd
sudo ./target/release/gput-packetd \
  --local 10.77.0.1 \
  --peer 10.77.0.2 \
  --listen-port 8080
```

From another terminal:

```bash
curl --noproxy '*' http://10.77.0.2:8080/plaintext
```

That request does **not** terminate in a normal server TCP socket. The host kernel routes the client packet into TUN, `gput-packetd` hands the raw IPv4 packet to the GPU, the shader owns the server-side TCP state and HTTP response, and the resulting raw packet is injected back through TUN.

CI proves this end to end on Linux with Vulkan/Lavapipe: a real `curl` completes the GPU-owned SYN/SYN-ACK/ACK exchange and receives `Hello, World!` from the packet shader.

The current TUN adapter deliberately dispatches one packet at a time. It is a correctness bridge, not the final high-throughput transport. Batching, persistent mapped rings, AF_XDP, and direct NIC queues are the performance path.

This is not an RFC-complete TCP stack. Retransmission, congestion control, out-of-order reassembly, receive windows, SYN cookies, IPv6, fragmentation, and flow-hash collision resolution still need to be added before exposing it to hostile traffic.

## macOS

The portable `gput-packetd` bridge uses the cross-platform `tun` crate and is designed to work with the same L3 packet boundary on macOS. On Apple Silicon the compute side runs through Metal via `wgpu`; macOS still owns the physical NIC and the TUN ingress/egress control path.

For an eventual packaged macOS app, `NEPacketTunnelProvider.packetFlow` is the native app-extension equivalent of the same packet-source/sink boundary. That adapter can remain thin while IPv4 parsing, TCP flow state, HTTP generation, and checksums stay in the GPU engine.

Apple does not expose a public general-purpose NIC-to-Metal DMA queue comparable to specialist Linux GPU-direct stacks, so macOS should be treated as the excellent hybrid/reference backend rather than the first zero-CPU NIC target.

## AMD Linux

There are now two concrete levels:

1. **Portable and runnable today:** `gput-packetd` reads/writes TUN packets while `GpuPacketEngine` runs through Vulkan on the AMD GPU.
2. **Direct target:** replace only the TUN packet source/sink with a HIP/ROCm XIO transport that owns compatible NIC queue memory and GPU-side doorbells.

ROCm XIO provides accelerator-initiated IO and GPU-direct RDMA endpoints, but it is not a transparent normal-TCP socket API. That is exactly why the packet boundary matters: the TCP semantics do not have to be rewritten when transport moves from TUN to XIO.

## Why this boundary matters

```text
macOS TUN / NetworkExtension -> Metal/wgpu packet engine
Linux TUN / AF_XDP           -> Vulkan/wgpu packet engine
AMD ROCm XIO                 -> HIP/XIO direct packet transport
NVIDIA DOCA                  -> CUDA/DOCA direct packet transport
```

A direct backend should eventually use a compact shared packet-ring ABI. The WGSL implementation is the portable reference semantics; vendor kernels are optimizations of that contract, not separate network stacks allowed to drift.

## Hardening order

1. Flow-table collision handling and generation counters.
2. Retransmission timers and duplicate ACK handling.
3. Receive-window accounting and bounded out-of-order segments.
4. SYN cookies.
5. IPv6.
6. Batch the TUN bridge and add packet/RPS benchmarks against the socket path.
7. Add AF_XDP as the first Linux kernel-bypass packet source.
8. Add a macOS NetworkExtension host target for packaged apps.
9. Add an AMD ROCm XIO direct transport on compatible hardware.

Once the direct packet path exists, measure packets/s, established flows/s, GPU batch occupancy, CPU cycles/request, and HTTP RPS. The interesting question is where the packet engine overtakes the normal kernel socket path, not whether a carefully chosen synthetic microbenchmark can be made to look heroic.
