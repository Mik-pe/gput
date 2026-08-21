# AGENTS.md

## Repository temperament

This repository is an experiment, not a parliament.

- Work goes directly to `main` unless the user explicitly requests another workflow.
- Do not open pull requests by default.
- Do not create ceremonial branches, draft plans, or approval gates.
- Keep commits small enough to revert when the GPU develops opinions.
- Before pushing, run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` whenever the environment permits it.
- A failing `main` is allowed briefly while repairing an experiment, but leaving it broken is not.
- Prefer a complete vertical slice over placeholder abstractions.
- The CPU may frame TCP traffic and provide a baseline or fallback. HTTP parsing, routing, and response generation belong on the GPU path.
- No fake GPU backend. A request reported as GPU processed must have passed through an actual compute dispatch and readback.
- Avoid unsafe Rust unless there is a measured, documented reason and the user explicitly approves it.

## Router contract

The Rust router API is allowed to feel like a normal web framework. Its implementation must remain honest about where request work happens.

- `Router`, `routing::get`, `Response`, and `Body` declarations compile once at startup into GPU route tables, response descriptors, body bytecode, and an immutable UTF-8 string arena.
- The compute shader must parse the request, select the route, execute the response program, and assemble the response for the GPU backend.
- Do not accept arbitrary host closures and present them as GPU handlers.
- The CPU baseline must consume the same compiled router and body operations. Do not maintain a second handwritten route implementation.
- Every request-derived body operation must be bounded so response-slot requirements can be validated before serving traffic.
- New body opcodes require the Rust compiler, CPU interpreter, WGSL interpreter, size calculation, tests, and documentation in the same change.
- Route hashes are only an index hint. Exact byte comparison must remain in place so collisions cannot select the wrong route.

## Benchmark contract

- HTTP/1.1 connections are persistent by default and may contain pipelined requests. The CPU may frame those request boundaries, but request-line parsing and routing still belong to the processor path.
- The built-in HTTP benchmark should be usable against non-gput servers. Do not bake competitor-specific shortcuts into the client.
- The raw-packet benchmark must feed equivalent packet conversations to CPU and GPU backends and validate responses outside the timed engine section.
- State clearly that `CpuPacketEngine` is a single-threaded semantic reference, not the Linux kernel or the best possible CPU stack.
- Prefer medians across repeated runs over cherry-picked peaks. Report latency beside throughput and print the GPU adapter.
- Keep `/plaintext` boring. It exists to expose transport and dispatch overhead, not to manufacture a GPU-friendly victory.
- Benchmark changes need socket or packet-level coverage, not only parser unit tests.
- CI correctness results from Lavapipe are not discrete-GPU performance numbers.

## GPU packet contract

- Keep packet ingress and egress separate from protocol semantics. `RawPacket -> PacketEngine -> RawPacket` is the portability boundary.
- The portable packet engine is WGSL through `wgpu`, so the same protocol state machine can run on Metal and Vulkan.
- TCP flow state belongs in persistent GPU-visible storage. Do not quietly hand established-flow state back to a CPU socket implementation.
- Vendor-direct transports such as ROCm XIO or DOCA are packet sources and sinks, not excuses to fork TCP semantics.
- A flow hash is an index hint only. Full tuple comparison and collision-safe probing must remain in place.
- Packets from one flow must preserve order. Independent flows should be batched together aggressively.
- New TCP behavior requires matching CPU reference semantics, synthetic packet coverage, GPU shader validation, and TUN coverage when applicable.
- IPv4 and TCP checksums for GPU-generated packets belong in the packet shader path.
- Duplicate SYN and duplicate data handling must not allocate duplicate flows or advance sequence state twice.
- The packet engine is an experimental fast path, not an RFC-complete TCP stack. Do not expose it as production-safe until timers, congestion control, windows, bounded out-of-order handling, SYN-flood protection, IPv6, and stream reassembly are addressed.

## Scope

The current target is intentionally narrow:

- HTTP/1.0 and HTTP/1.1 request lines
- persistent HTTP/1.1 connections with basic pipelining support on the socket path
- exact static routes
- `GET` only on the application router
- bounded response programs
- batched compute dispatch through `wgpu`
- CPU baseline and automatic fallback
- an experimental raw IPv4/TCP GPU packet path with `/plaintext`, `/health`, and honest error statuses
- a batched TUN adapter plus a single-threaded CPU packet reference

Do not add TLS, HTTP/2, HTTP/3, a database, arbitrary middleware, or a general GPU heap until measurements justify expanding the blast radius.
