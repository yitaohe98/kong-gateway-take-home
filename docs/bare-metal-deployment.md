# Multi-host deployment without Docker

Docker is a packaging and local orchestration convenience, not a requirement
for distributed rate limiting. The service can run as a native binary on
several machines as long as every instance connects to the same authoritative
Redis primary.

```text
Trusted Gateway or internal load balancer
                  |
        +---------+---------+
        |         |         |
   Limiter A  Limiter B  Limiter C
   :8080      :8080      :8080
        \         |         /
         authoritative Redis primary
```

The limiter processes are stateless with respect to quota. Redis owns the
configuration and token buckets, and its Lua execution is the linearization
point. Consequently, a request may be sent to any healthy limiter instance
without splitting the quota pool.

## 1. Build a compatible release binary

Build on the target machine or on a build host with the same operating system,
CPU architecture, and compatible libc:

```sh
cargo build --release --locked
```

The resulting artifact is:

```text
target/release/distributed-rate-limiter
```

A macOS binary cannot be copied directly to Linux. For example, an x86-64
Linux deployment requires an x86-64 Linux build or an explicitly configured
cross-compilation toolchain.

## 2. Install the binary on every limiter host

Example hosts:

```text
10.0.1.11  limiter-a
10.0.1.12  limiter-b
10.0.1.13  limiter-c
10.0.2.10  Redis primary
```

On each limiter host, install the same release artifact at:

```text
/usr/local/bin/distributed-rate-limiter
```

Run it under a dedicated, unprivileged service account. Each instance uses the
same Redis URL but may use its own logging configuration:

```sh
export BIND_ADDR=0.0.0.0:8080
export REDIS_URL=redis://10.0.2.10:6379
export REDIS_OPERATION_TIMEOUT_MS=500
export RUST_LOG=info

/usr/local/bin/distributed-rate-limiter
```

Only trusted gateway hosts or an internal load balancer should be able to reach
port 8080. Only limiter hosts should be able to reach the Redis port.

## 3. Manage each process with systemd

Example `/etc/systemd/system/distributed-rate-limiter.service`:

```ini
[Unit]
Description=Distributed Rate Limiter
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rate-limiter
Group=rate-limiter
Environment=BIND_ADDR=0.0.0.0:8080
Environment=REDIS_URL=redis://10.0.2.10:6379
Environment=REDIS_OPERATION_TIMEOUT_MS=500
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/distributed-rate-limiter
Restart=always
RestartSec=2
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

Enable and inspect the service on every host:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now distributed-rate-limiter
sudo systemctl status distributed-rate-limiter
journalctl -u distributed-rate-limiter -f
```

## 4. Register the instances with the gateway or load balancer

Configure the three private endpoints as one backend pool and use
`GET /readyz` as the health check:

```text
http://10.0.1.11:8080
http://10.0.1.12:8080
http://10.0.1.13:8080
```

An instance should receive admission traffic only while `/readyz` returns
`200`. The gateway calls `POST /v1/admissions` before forwarding the business
request. Configuration can be read or replaced through any healthy instance
because `GET/PUT /v1/config` operates on the shared Redis configuration.

## Deliberately omitted authentication

This take-home implementation does **not** validate an API key. No
`Authorization` or `X-API-Key` header is required for either the Admission API
or Configuration API. The MVP assumes a trusted internal network and a trusted
gateway that has already authenticated the business client and derived the
authoritative `client_id`, `source_ip`, and normalized `path`.

This is a scope decision, not a recommended production security model. Without
caller authentication, an untrusted caller could spoof identity metadata,
consume another client's quota, bypass its own quota by changing identities,
or replace the global configuration.

A production baseline should therefore:

- require a gateway service API key or equivalent workload credential;
- validate the credential before reading configuration or executing admission;
- use a separate, more privileged credential for `PUT /v1/config`;
- store credentials outside source code, support rotation, and avoid logging
  secret values;
- restrict the service to private networks and encrypt traffic in transit; and
- prefer mTLS or service-mesh workload identity when the platform provides it.

For a simple API-key design, the gateway could send:

```http
Authorization: Bearer <service-api-key>
```

The limiter would compare the presented credential with a secret loaded from a
secret manager or environment injection and return `401` for a missing or
invalid credential and `403` when a valid caller lacks configuration-write
permission. That authentication layer is intentionally outside this MVP so the
assignment remains focused on Redis atomicity, token-bucket correctness, and
multi-instance consistency.

## Other production gaps

The native multi-host procedure does not by itself provide:

- Redis consensus or lossless failover;
- Redis/client transport encryption in the current build;
- idempotent retries for ambiguous admission outcomes;
- metrics, alerting, distributed tracing, or audit logging; or
- automated binary rollout, rollback, and configuration management.

These limitations do not change the active-primary consistency model, but they
must be addressed before exposing the service as production infrastructure.
