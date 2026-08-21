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

Run `gput-packet-demo` to inject a synthetic SYN, complete the handshake, issue an HTTP request, verify both checksums, and close the flow. The response bytes came out of the packet shader, not the kernel TCP stack:

```bash
cargo run --release --locked --bin gput-packet-demo
```

This is not an RFC-complete TCP stack. Retransmission, congestion control, out-of-order reassembly, receive windows, SYN cookies, IPv6, fragmentation, and flow-hash collision resolution still need to be added before exposing it to hostile traffic.

## macOS

Apple's supported app-level packet ingress is a NetworkExtension packet tunnel. `NEPacketTunnelProvider.packetFlow` reads batches of IP packets from a virtual interface and writes batches back. That is the same L3 boundary `GpuPacketEngine` consumes. The Apple-specific adapter can stay a thin source/sink while parsing, flow lookup, TCP state, HTTP generation, and checksums remain GPU work.

For command-line development, a cross-platform TUN adapter is also viable and avoids requiring an app-extension entitlement. Apple Silicon still does not expose a public NIC-to-Metal DMA path, so macOS remains a hybrid: the OS owns the physical NIC while the TCP/HTTP fast path can live in Metal compute.

## AMD Linux

There are two modes:

1. **Portable:** feed IP packets through TUN/AF_XDP while this WGSL engine runs over Vulkan on the AMD GPU.
2. **Direct:** replace only the packet source/sink with a HIP/ROCm XIO transport that owns compatible NIC queue memory and GPU-side doorbells.

ROCm XIO provides accelerator-initiated IO and GPU-direct RDMA endpoints, but it is not a transparent normal-TCP socket API. The clean design is therefore to keep packet transport separate from TCP semantics. A HIP/XIO implementation can consume the same future packet-ring ABI without infecting the application/router API with vendor details.

## Why this boundary matters

```text
macOS NetworkExtension -> Metal/wgpu packet engine
Linux TUN / AF_XDP     -> Vulkan/wgpu packet engine
AMD ROCm XIO           -> HIP/XIO direct packet transport
NVIDIA DOCA            -> CUDA/DOCA direct packet transport
```

A direct backend should eventually use a compact shared packet-ring ABI. The WGSL implementation is the portable reference semantics; vendor kernels are optimizations of that contract, not separate network stacks allowed to drift.

## Hardening order

1. Flow-table collision handling and generation counters.
2. Retransmission timers and duplicate ACK handling.
3. Receive-window accounting and bounded out-of-order segments.
4. SYN cookies.
5. IPv6.
6. Linux/macOS TUN bridge for end-to-end packet benchmarks.
7. macOS NetworkExtension host target.
8. AMD ROCm XIO direct transport on compatible hardware.

Once the direct packet path exists, measure packets/s, established flows/s, GPU batch occupancy, CPU cycles/request, and HTTP RPS. The interesting question is where the packet engine overtakes the normal kernel socket path, not whether a carefully chosen synthetic microbenchmark can be made to look heroic.
