#!/usr/bin/env bash
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

PROJECT_NAME="kong-rate-limiter-sim-$$"
TMP_DIR="$(mktemp -d)"
PIDS=()
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

cleanup() {
  local exit_code=$?
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
  done
  docker compose -p "$PROJECT_NAME" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$TMP_DIR"
  trap - EXIT INT TERM
  exit "$exit_code"
}
trap cleanup EXIT INT TERM

wait_ready() {
  local url=$1
  for _ in $(seq 1 30); do
    if [[ "$(curl -sS -o /dev/null -w '%{http_code}' "$url/readyz" || true)" == "200" ]]; then
      return 0
    fi
    sleep 1
  done
  printf 'simulation environment did not become ready\n' >&2
  for log_file in "$TMP_DIR"/limiter-*.log; do
    [[ -f "$log_file" ]] && cat "$log_file" >&2
  done
  exit 1
}

printf '[simulate] starting isolated Redis and three limiter processes\n'
docker compose -p "$PROJECT_NAME" up -d redis >/dev/null
for _ in $(seq 1 30); do
  if docker compose -p "$PROJECT_NAME" exec -T redis redis-cli ping 2>/dev/null | grep -q PONG; then
    break
  fi
  sleep 1
done

cargo build --locked >/dev/null
for index in 1 2 3; do
  port=$((8080 + index))
  BIND_ADDR="127.0.0.1:$port" \
  REDIS_URL="redis://127.0.0.1:6379" \
  REDIS_OPERATION_TIMEOUT_MS=500 \
  RUST_LOG=warn \
    target/debug/distributed-rate-limiter >"$TMP_DIR/limiter-$index.log" 2>&1 &
  PIDS+=("$!")
done

wait_ready http://127.0.0.1:8081
wait_ready http://127.0.0.1:8082
wait_ready http://127.0.0.1:8083

python3 scripts/simulate_traffic.py "$@"
