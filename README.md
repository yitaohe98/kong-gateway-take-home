# Distributed Rate Limiter

[中文说明](README.zh-CN.md)

A standalone Redis-backed admission service implementing option 1 of the Kong
Gateway take-home assignment.

Multiple stateless service replicas share one authoritative Redis quota pool.
Each request is evaluated against Request Path, Client ID, and source IP Token
Buckets in one atomic Redis Lua execution.

## Architecture

```text
Trusted Gateway
      |
      +---- Limiter 1 ----\
      +---- Limiter 2 -----+---- Redis Primary
      +---- Limiter 3 ----/
```

- Limiter replicas keep no authoritative quota state.
- Redis Lua atomically refills, checks, and conditionally deducts all three
  buckets.
- Redis `TIME` is the only refill clock.
- A request consumes one token from every bucket only when all three permit it.
- Redis failures return `503 Service Unavailable` (fail-closed).

The consistency guarantee applies while the active Redis Primary is available.
See [Engineering Decisions](docs/engineering-decisions.md) for detailed
tradeoffs.

## Install and run

### Docker Compose — recommended

Requirements:

- Docker Engine or Docker Desktop with the `docker compose` command
- Free host ports `8081`–`8083`
- Free host port `6379`, because the development Compose file exposes Redis on
  `127.0.0.1` for host-side testing and debugging

Start one Redis Primary and three Rate Limiter replicas:

```sh
docker compose up -d --build
docker compose ps
```

Verify all replicas:

```sh
for port in 8081 8082 8083; do
  curl --fail "http://127.0.0.1:${port}/readyz"
done
```

The instances are available at `http://127.0.0.1:8081`, `:8082`, and `:8083`.

Stop the stack while preserving Redis data:

```sh
docker compose down
```

Delete the stack and all persisted Redis state:

```sh
docker compose down -v
```

### Platform support

- Tested on macOS with Apple Silicon using Docker Desktop and native binaries.
- Docker deployment is expected to work on Linux and macOS wherever the
  `docker compose` command is available.
- Native binaries can run on macOS or Linux when built for the target OS and
  CPU architecture; the systemd example is Linux-specific.
- Native Windows and PowerShell workflows were not tested. WSL2 is the
  recommended Windows environment.

### Host development — optional

Host development requires Rust 1.85+, Python 3, Docker, Bash, `make`, and
`curl`.

```sh
make setup
```

`make setup` checks Docker and Python, installs Rust through `rustup` when Cargo
is missing, fetches locked dependencies, pulls Redis, and builds the host
binary. It does not install Docker or Python.

Run the application locally with Redis in Docker:

```sh
docker compose up -d redis
export REDIS_URL=redis://127.0.0.1:6379
cargo run
```

For deployment as native binaries on separate machines, see
[Multi-host deployment without Docker](docs/bare-metal-deployment.md).

## Configuration

### Process configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `BIND_ADDR` | `0.0.0.0:8080` | HTTP listen address |
| `REDIS_URL` | `redis://127.0.0.1:6379` | Authoritative Redis endpoint |
| `REDIS_OPERATION_TIMEOUT_MS` | `500` | Redis operation timeout |
| `RUST_LOG` | `info` | Structured log filter |

All replicas must connect to the same Redis Primary. Separate Redis instances
would split the quota pool.

### Rate-limit policy

Read the current policy:

```sh
curl --fail http://127.0.0.1:8081/v1/config
```

Replace it through any healthy replica:

```sh
curl -i -X PUT http://127.0.0.1:8081/v1/config \
  -H 'Content-Type: application/json' \
  -d '{
    "path":   { "limit": 100, "window_seconds": 60 },
    "client": { "limit": 50,  "window_seconds": 60 },
    "ip":     { "limit": 20,  "window_seconds": 10 }
  }'
```

All three dimensions are mandatory and always enabled:

- `limit`: Token Bucket capacity and maximum burst, from `1` to `1,000,000`
- `window_seconds`: refill window, from `1` to `86,400`
- refill rate: `limit / window_seconds`
- request cost: one token from each dimension

A request is admitted only when all three buckets have a token. A denial
deducts nothing. Every successful policy update increments `generation` and
starts fresh buckets.

## API

### Health

```http
GET /healthz
GET /readyz
```

`healthz` checks the process. `readyz` checks Redis and the shared policy.

### Admission

```sh
curl -i -X POST http://127.0.0.1:8081/v1/admissions \
  -H 'Content-Type: application/json' \
  -d '{
    "request_id": "req-123",
    "path": "/payments/{payment_id}",
    "client_id": "merchant-123",
    "source_ip": "203.0.113.10"
  }'
```

| Status | Meaning |
| --- | --- |
| `200` | Allowed; one token deducted from every bucket |
| `400` | Invalid request |
| `429` | One or more dimensions lack quota |
| `503` | Redis could not provide a trustworthy decision |

A `429` includes `Retry-After` and every constraining dimension. Redis failures
return `503` rather than an incorrect `200` or `429`.

The gateway calls this API before forwarding the business request; the service
does not proxy upstream traffic.

## Test

```sh
make test-unit
make test-integration
make test-all
```

The integration suite starts three Rate Limiter processes sharing one isolated
Redis and verifies shared quota, atomic multi-dimensional deduction, policy
generation reset, validation, `Retry-After`, and fail-closed behavior.

## Traffic simulation

Send one request to a running Compose replica:

```sh
curl -i -X POST http://127.0.0.1:8081/v1/admissions \
  -H 'Content-Type: application/json' \
  -d '{
    "request_id": "simulation-1",
    "path": "/simulation/path-0",
    "client_id": "simulation-client-0",
    "source_ip": "203.0.0.1"
  }'
```

Use ports `8082` or `8083` to target the other replicas. All three share the
same Redis quota pool.

Run a burst against the Compose stack:

```sh
python3 scripts/simulate_traffic.py \
  --base-urls http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083 \
  --pattern burst \
  --requests 100 \
  --concurrency 16
```

The simulator reports status counts, per-instance distribution, throughput, and
p50/p95/p99/max latency. View the `burst`, `steady`, and `ramp` options with:

```sh
python3 scripts/simulate_traffic.py --help
```

To create an isolated Redis and three host processes automatically:

```sh
make simulate PATTERN=burst REQUESTS=500 CONCURRENCY=64
```

`make test-integration` and `make simulate` use host ports `6379` and
`8081`–`8083`. Stop the regular Compose stack before running them.

## Technical documentation

- [Engineering Decisions](docs/engineering-decisions.md)
- [Native Multi-host Deployment](docs/bare-metal-deployment.md)
