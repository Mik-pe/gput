#!/usr/bin/env bash
set -Eeuo pipefail

flows="${GPUT_PROOF_FLOWS:-1024}"
requests_per_flow="${GPUT_PROOF_REQUESTS_PER_FLOW:-100}"
warmup_requests_per_flow="${GPUT_PROOF_WARMUP_REQUESTS_PER_FLOW:-10}"
batch_size="${GPUT_PROOF_BATCH_SIZE:-256}"
flow_capacity="${GPUT_PROOF_FLOW_CAPACITY:-16384}"
flow_probe_limit="${GPUT_PROOF_FLOW_PROBE_LIMIT:-64}"

cargo build --release --locked --bins

printf '\n===== GPU packet semantics =====\n'
target/release/gput-packet-demo

printf '\n===== Same raw packets, CPU reference versus GPU =====\n'
target/release/gput-packet-bench \
  --backend both \
  --flows "$flows" \
  --requests-per-flow "$requests_per_flow" \
  --warmup-requests-per-flow "$warmup_requests_per_flow" \
  --batch-size "$batch_size" \
  --flow-capacity "$flow_capacity" \
  --flow-probe-limit "$flow_probe_limit"

cat <<'EOF'

The CPU row is a single-threaded semantic reference, not the Linux kernel TCP stack.
For public claims, repeat the run and publish hardware, driver, OS, power mode,
and every result. docs/PROOF.md contains the full evidence contract.
EOF
