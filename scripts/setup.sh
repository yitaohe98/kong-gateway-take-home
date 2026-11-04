#!/usr/bin/env bash
set -Eeuo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

log() {
  printf '[setup] %s\n' "$*"
}

fail() {
  printf '[setup] error: %s\n' "$*" >&2
  exit 1
}

command -v docker >/dev/null 2>&1 || fail "Docker is required: https://docs.docker.com/get-docker/"
docker compose version >/dev/null 2>&1 || fail "Docker Compose v2+ is required"
docker info >/dev/null 2>&1 || fail "Docker daemon is not running"
command -v python3 >/dev/null 2>&1 || fail "Python 3 is required for traffic simulation"

if ! command -v cargo >/dev/null 2>&1; then
  if command -v brew >/dev/null 2>&1; then
    log "installing rustup with Homebrew"
    brew install rustup
    export PATH="$(brew --prefix rustup)/bin:$PATH"
  elif command -v curl >/dev/null 2>&1; then
    log "installing rustup with the official non-interactive installer"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
      sh -s -- -y --no-modify-path
    export PATH="$HOME/.cargo/bin:$PATH"
  else
    fail "cargo is missing and neither Homebrew nor curl is available"
  fi
fi

if command -v rustup >/dev/null 2>&1; then
  log "installing the stable Rust toolchain with rustfmt and clippy"
  rustup toolchain install stable --profile minimal --component rustfmt --component clippy
  rustup default stable
fi

command -v cargo >/dev/null 2>&1 || fail "cargo is still unavailable after Rust setup"

log "fetching locked Rust dependencies"
cargo fetch --locked

log "pulling the Redis image"
docker compose pull redis

log "building the local rate-limiter binary"
cargo build --locked

if [[ "${BUILD_DOCKER_IMAGE:-0}" == "1" ]]; then
  log "building the optional deployment image"
  docker compose build
else
  log "skipping optional deployment image; set BUILD_DOCKER_IMAGE=1 to build it"
fi

log "environment is ready"
printf '\nNext commands:\n'
printf '  make dev-up\n'
printf '  make test-all\n'
printf '  make simulate PATTERN=burst REQUESTS=500 CONCURRENCY=64\n'
