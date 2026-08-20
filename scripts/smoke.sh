#!/usr/bin/env bash
set -Eeuo pipefail

backend="${1:-}"
binary="${2:-target/debug/gput}"
bench_binary="$(dirname "$binary")/gput-bench"
port="${GPUT_SMOKE_PORT:-18080}"
address="127.0.0.1:${port}"
base_url="http://${address}"

if [[ "$backend" != "cpu" && "$backend" != "gpu" ]]; then
  echo "usage: $0 <cpu|gpu> [path-to-gput]" >&2
  exit 2
fi

if [[ ! -x "$binary" ]]; then
  echo "gput binary is missing or not executable: $binary" >&2
  exit 2
fi

if [[ ! -x "$bench_binary" ]]; then
  echo "gput-bench binary is missing or not executable: $bench_binary" >&2
  exit 2
fi

work_dir="$(mktemp -d)"
server_log="$work_dir/server.log"
server_pid=""

cleanup() {
  local status=$?
  trap - EXIT

  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -INT "$server_pid" 2>/dev/null || true
    for _ in $(seq 1 50); do
      if ! kill -0 "$server_pid" 2>/dev/null; then
        break
      fi
      sleep 0.02
    done
    if kill -0 "$server_pid" 2>/dev/null; then
      kill -KILL "$server_pid" 2>/dev/null || true
    fi
    wait "$server_pid" 2>/dev/null || true
  fi

  if (( status != 0 )); then
    echo >&2
    echo "===== gput ${backend} smoke-test server log =====" >&2
    cat "$server_log" >&2 || true
  fi

  rm -rf "$work_dir"
  exit "$status"
}
trap cleanup EXIT

"$binary" \
  --backend "$backend" \
  --bind "$address" \
  --batch-size 64 \
  --batch-wait-micros 5000 \
  --queue-depth 512 \
  --max-connections 256 \
  >"$server_log" 2>&1 &
server_pid=$!

ready=false
for _ in $(seq 1 200); do
  if curl --silent --show-error --fail --max-time 0.25 \
    "$base_url/health" >"$work_dir/readiness-body" 2>/dev/null; then
    ready=true
    break
  fi

  if ! kill -0 "$server_pid" 2>/dev/null; then
    break
  fi

  sleep 0.05
done

if [[ "$ready" != true ]]; then
  echo "gput did not become ready on $address" >&2
  exit 1
fi

printf 'ok\n' >"$work_dir/expected-health"
cmp "$work_dir/expected-health" "$work_dir/readiness-body"

curl --silent --show-error --fail \
  --dump-header "$work_dir/hello-headers" \
  --output "$work_dir/hello-body" \
  "$base_url/hello"
tr -d '\r' <"$work_dir/hello-headers" >"$work_dir/hello-headers-clean"
grep --fixed-strings --line-regexp "X-Gput-Backend: $backend" \
  "$work_dir/hello-headers-clean" >/dev/null

if [[ "$backend" == "gpu" ]]; then
  printf 'hello from a compute shader\n' >"$work_dir/expected-hello"
else
  printf 'hello from the CPU baseline\n' >"$work_dir/expected-hello"
fi
cmp "$work_dir/expected-hello" "$work_dir/hello-body"

curl --silent --show-error --fail "$base_url/" >"$work_dir/root-body"
grep --fixed-strings "\"backend\":\"$backend\"" "$work_dir/root-body" >/dev/null

curl --silent --show-error --fail "$base_url/health?probe=smoke" \
  >"$work_dir/query-body"
cmp "$work_dir/expected-health" "$work_dir/query-body"

not_found_status="$(
  curl --silent --show-error \
    --output "$work_dir/not-found-body" \
    --write-out '%{http_code}' \
    "$base_url/there-is-no-sensible-reason-for-this"
)"
[[ "$not_found_status" == "404" ]]
printf 'not found\n' >"$work_dir/expected-not-found"
cmp "$work_dir/expected-not-found" "$work_dir/not-found-body"

method_status="$(
  curl --silent --show-error \
    --request POST \
    --output "$work_dir/method-body" \
    --write-out '%{http_code}' \
    "$base_url/"
)"
[[ "$method_status" == "405" ]]
printf 'method not allowed\n' >"$work_dir/expected-method"
cmp "$work_dir/expected-method" "$work_dir/method-body"

GPUT_SMOKE_ADDRESS="$address" GPUT_SMOKE_BACKEND="$backend" python3 - <<'PY'
import os
import socket

host, port_text = os.environ["GPUT_SMOKE_ADDRESS"].rsplit(":", 1)
port = int(port_text)
backend = os.environ["GPUT_SMOKE_BACKEND"].encode()

cases = [
    (b"GET /health HTTP/1.0\r\n\r\n", b"HTTP/1.1 200 OK\r\n"),
    (b"GET /hello?source=raw HTTP/1.1\r\nHost: smoke\r\n\r\n", b"HTTP/1.1 200 OK\r\n"),
    (b"GET / GPUT/6.6\r\n\r\n", b"HTTP/1.1 400 Bad Request\r\n"),
    (b"GET / HTTP/1.1\rX\nHost: smoke\r\n\r\n", b"HTTP/1.1 400 Bad Request\r\n"),
    (b"POST / HTTP/1.1\r\n\r\n", b"HTTP/1.1 405 Method Not Allowed\r\n"),
]

for request, expected_status in cases:
    with socket.create_connection((host, port), timeout=2.0) as stream:
        stream.sendall(request)
        stream.shutdown(socket.SHUT_WR)
        chunks = []
        while True:
            chunk = stream.recv(4096)
            if not chunk:
                break
            chunks.append(chunk)

    response = b"".join(chunks)
    assert response.startswith(expected_status), (request, response)
    assert b"\r\nX-Gput-Backend: " + backend + b"\r\n" in response, response
PY

export GPUT_SMOKE_ENDPOINT="$base_url/hello"
if [[ "$backend" == "gpu" ]]; then
  export GPUT_SMOKE_EXPECTED="hello from a compute shader"
else
  export GPUT_SMOKE_EXPECTED="hello from the CPU baseline"
fi

seq 1 96 | xargs -P 32 -I '{}' bash -c '
  [[ "$(curl --silent --show-error --fail --max-time 2 "$GPUT_SMOKE_ENDPOINT")" == "$GPUT_SMOKE_EXPECTED" ]]
'

"$bench_binary" \
  --address "$address" \
  --path /hello \
  --requests 128 \
  --concurrency 32 \
  --warmup 16 \
  --timeout-millis 2000 \
  --expected-backend "$backend" \
  --json \
  >"$work_dir/bench.json"

BENCH_RESULT="$work_dir/bench.json" python3 - <<'PY'
import json
import os

with open(os.environ["BENCH_RESULT"], encoding="utf-8") as result_file:
    result = json.load(result_file)

assert result["requests"] == 128, result
assert result["concurrency"] == 32, result
assert result["elapsed_seconds"] > 0, result
assert result["requests_per_second"] > 0, result
assert result["latency_nanos"]["p50"] > 0, result
assert result["latency_nanos"]["max"] >= result["latency_nanos"]["p99"], result
assert result["response_bytes"] > 0, result
PY

kill -INT "$server_pid"
wait "$server_pid"
server_pid=""

grep --fixed-strings "shutdown signal received" "$server_log" >/dev/null
echo "gput $backend smoke test passed"
