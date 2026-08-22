#!/usr/bin/env bash
set -Eeuo pipefail

flows="${GPUT_PROOF_FLOWS:-65536}"
requests_per_flow="${GPUT_PROOF_REQUESTS_PER_FLOW:-1000}"
warmup_requests_per_flow="${GPUT_PROOF_WARMUP_REQUESTS_PER_FLOW:-20}"
batch_size="${GPUT_PROOF_BATCH_SIZE:-65536}"
flow_capacity="${GPUT_PROOF_FLOW_CAPACITY:-131072}"
flow_probe_limit="${GPUT_PROOF_FLOW_PROBE_LIMIT:-64}"
target_dir="${CARGO_TARGET_DIR:-target}"

cargo build --release --locked --bins

printf '\n===== GPU packet semantics =====\n'
"$target_dir/release/gput-packet-demo"

printf '\n===== Same raw packets, CPU reference versus GPU =====\n'
"$target_dir/release/gput-packet-bench" \
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
