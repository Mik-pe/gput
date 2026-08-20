# gput repository instructions

Commit completed work directly to `main` unless the user explicitly asks for a branch or pull request. This repository deliberately optimizes for fast experiments and easy reverts rather than review ceremony.

The GPU backend must perform HTTP request-line parsing, route selection, and complete HTTP response generation inside a compute shader. The CPU network layer may locate the end of the HTTP headers, batch opaque request bytes, submit GPU work, and write returned bytes to the socket.

Keep the CPU backend behaviorally comparable so benchmarks measure the processor backend rather than two unrelated servers. Never claim that a response used the GPU unless an actual compute dispatch completed successfully.

Run formatting, Clippy, and tests before pushing when possible. Keep direct-to-main commits coherent and reversible.
