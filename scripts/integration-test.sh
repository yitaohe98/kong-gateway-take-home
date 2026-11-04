#!/usr/bin/env bash
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

PROJECT_NAME="kong-rate-limiter-test-$$"
TMP_DIR="$(mktemp -d)"
PIDS=()
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

cleanup() {
  local exit_code=$?
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
  done
  if [[ "${KEEP_TEST_ENV:-0}" != "1" ]]; then
    docker compose -p "$PROJECT_NAME" down --volumes --remove-orphans >/dev/null 2>&1 || true
  else
    printf '[integration] preserving Compose project %s\n' "$PROJECT_NAME"
  fi
  rm -rf "$TMP_DIR"
  trap - EXIT INT TERM
  exit "$exit_code"
}
trap cleanup EXIT INT TERM

log() {
  printf '[integration] %s\n' "$*"
}

fail() {
  printf '[integration] FAILED: %s\n' "$*" >&2
  for log_file in "$TMP_DIR"/limiter-*.log; do
    if [[ -f "$log_file" ]]; then
      printf '\n--- %s ---\n' "$log_file" >&2
      cat "$log_file" >&2
    fi
  done
  docker compose -p "$PROJECT_NAME" logs redis --no-color >&2 || true
  exit 1
}

wait_ready() {
  local url=$1
  for _ in $(seq 1 90); do
    if [[ "$(curl -sS -o /dev/null -w '%{http_code}' "$url/readyz" || true)" == "200" ]]; then
      return 0
    fi
    sleep 1
  done
  fail "$url did not become ready"
}

request() {
  local name=$1
  local method=$2
  local url=$3
  local body=${4:-}
  local args=(-sS -D "$TMP_DIR/$name.headers" -o "$TMP_DIR/$name.body" -w '%{http_code}' -X "$method")
  if [[ -n "$body" ]]; then
    args+=(-H 'content-type: application/json' --data "$body")
  fi
  curl "${args[@]}" "$url"
}

assert_status() {
  local actual=$1
  local expected=$2
  local context=$3
  [[ "$actual" == "$expected" ]] || fail "$context: expected HTTP $expected, got $actual"
}

log "starting isolated Redis"
docker compose -p "$PROJECT_NAME" up -d redis
for _ in $(seq 1 30); do
  if docker compose -p "$PROJECT_NAME" exec -T redis redis-cli ping 2>/dev/null | grep -q PONG; then
    break
  fi
  sleep 1
done

log "building and starting three host Rate Limiter processes"
cargo build --locked
for index in 1 2 3; do
  port=$((8080 + index))
  BIND_ADDR="127.0.0.1:$port" \
  REDIS_URL="redis://127.0.0.1:6379" \
  REDIS_OPERATION_TIMEOUT_MS=500 \
  RUST_LOG=info \
    target/debug/distributed-rate-limiter >"$TMP_DIR/limiter-$index.log" 2>&1 &
  PIDS+=("$!")
done
wait_ready http://127.0.0.1:8081
wait_ready http://127.0.0.1:8082
wait_ready http://127.0.0.1:8083

log "verifying concurrent bootstrap produced generation 1"
status=$(request bootstrap GET http://127.0.0.1:8081/v1/config)
assert_status "$status" 200 bootstrap
python3 - "$TMP_DIR/bootstrap.body" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
assert data["generation"] == 1, data
PY

shared_config='{"path":{"limit":100,"window_seconds":3600},"client":{"limit":2,"window_seconds":3600},"ip":{"limit":100,"window_seconds":3600}}'
status=$(request configure PUT http://127.0.0.1:8081/v1/config "$shared_config")
assert_status "$status" 200 configure

payload='{"request_id":"shared-1","path":"/payments/{id}","client_id":"shared-client","source_ip":"203.0.113.10"}'
log "verifying one quota pool across three instances"
status1=$(request shared1 POST http://127.0.0.1:8081/v1/admissions "$payload")
status2=$(request shared2 POST http://127.0.0.1:8082/v1/admissions "$payload")
status3=$(request shared3 POST http://127.0.0.1:8083/v1/admissions "$payload")
assert_status "$status1" 200 'shared request 1'
assert_status "$status2" 200 'shared request 2'
assert_status "$status3" 429 'shared request 3'
grep -qi '^retry-after:' "$TMP_DIR/shared3.headers" || fail '429 response is missing Retry-After'
python3 - "$TMP_DIR/shared3.body" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
assert data["allowed"] is False, data
assert data["remaining"] == {"path": 98, "client": 0, "ip": 98}, data
assert [x["dimension"] for x in data["failed_dimensions"]] == ["client"], data
PY

log "verifying a configuration update resets bucket generation"
status=$(request reset PUT http://127.0.0.1:8082/v1/config "$shared_config")
assert_status "$status" 200 reset
status=$(request after_reset POST http://127.0.0.1:8083/v1/admissions "$payload")
assert_status "$status" 200 'request after generation reset'
python3 - "$TMP_DIR/after_reset.body" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
assert data["remaining"]["client"] == 1, data
PY

log "verifying strict request and configuration validation"
invalid_config='{"path":{"limit":0,"window_seconds":60},"client":{"limit":2,"window_seconds":60},"ip":{"limit":2,"window_seconds":60}}'
status=$(request invalid_config PUT http://127.0.0.1:8081/v1/config "$invalid_config")
assert_status "$status" 400 'invalid config'
invalid_request='{"path":"/x","client_id":"client","source_ip":"not-an-ip"}'
status=$(request invalid_request POST http://127.0.0.1:8081/v1/admissions "$invalid_request")
assert_status "$status" 400 'invalid admission request'

log "verifying all dimensions fail together without partial deduction"
all_config='{"path":{"limit":1,"window_seconds":3600},"client":{"limit":1,"window_seconds":3600},"ip":{"limit":1,"window_seconds":3600}}'
status=$(request all_config PUT http://127.0.0.1:8081/v1/config "$all_config")
assert_status "$status" 200 'all-dimension config'
all_payload='{"path":"/all","client_id":"all-client","source_ip":"203.0.113.20"}'
assert_status "$(request all_first POST http://127.0.0.1:8081/v1/admissions "$all_payload")" 200 'all first'
assert_status "$(request all_second POST http://127.0.0.1:8082/v1/admissions "$all_payload")" 429 'all second'
python3 - "$TMP_DIR/all_second.body" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    data = json.load(f)
assert {x["dimension"] for x in data["failed_dimensions"]} == {"path", "client", "ip"}, data
assert data["remaining"] == {"path": 0, "client": 0, "ip": 0}, data
PY

log "verifying fail-closed behavior while Redis is unavailable"
docker compose -p "$PROJECT_NAME" stop redis >/dev/null
status=$(request redis_down POST http://127.0.0.1:8083/v1/admissions "$all_payload")
assert_status "$status" 503 'Redis unavailable'
docker compose -p "$PROJECT_NAME" start redis >/dev/null
wait_ready http://127.0.0.1:8081

log "all integration tests passed"
