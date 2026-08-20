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

## Scope

The current target is intentionally narrow:

- HTTP/1.0 and HTTP/1.1 request lines
- one request per TCP connection
- `GET` only
- batched compute dispatch through `wgpu`
- CPU baseline and automatic fallback

Do not add TLS, HTTP/2, HTTP/3, a framework facade, or a database until measurements justify expanding the blast radius.
