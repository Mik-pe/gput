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

## Scope

The current target is intentionally narrow:

- HTTP/1.0 and HTTP/1.1 request lines
- one request per TCP connection
- exact static routes
- `GET` only
- bounded response programs
- batched compute dispatch through `wgpu`
- CPU baseline and automatic fallback

Do not add TLS, HTTP/2, HTTP/3, a database, arbitrary middleware, or a general GPU heap until measurements justify expanding the blast radius.
