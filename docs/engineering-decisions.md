# Engineering Decision Log

This document defines the solution boundary for the distributed rate-limiting
assignment: what was engineered in depth, what was deliberately simplified,
and what remains before the service is production-ready.

## Architecture

```text
                              admission request
Client ----> Trusted Gateway ----------------------+
                  |                                |
                  |                       +--------v--------+
                  |                       | Load Balancer   |  optional
                  |                       +--------+--------+
                  |                                |
                  |                  +-------------+-------------+
                  |                  |             |             |
                  |             Limiter 1     Limiter 2     Limiter 3
                  |                  \             |             /
                  |                   +------------+------------+
                  |                                |
                  |                         Redis Primary
                  |                                |
                  |                    production replica(s)
                  |
                  +---- allowed ----> Upstream Service
                  +---- denied  ----> 429 to Client

Operator/Control Plane ---- PUT /v1/config ----> any Limiter replica
```

The Gateway calls the Admission API before forwarding a business request. The
Limiter replicas keep no authoritative quota state; they share configuration
and buckets through one Redis Primary. The current Compose demo implements
three Limiters and one Redis Primary, but not the optional load balancer or
Redis replicas shown as production extensions.

## Engineered in depth

- **Admission correctness:** Path, Client ID, and IP are evaluated as one
  all-or-nothing decision; rejected traffic consumes no quota.
- **Distributed consistency:** all replicas share one authoritative quota pool,
  preventing the quota split caused by per-instance memory limits.
- **Token Bucket precision:** refill uses one Redis time source, integer
  microtokens, and preserved remainders.
- **Failure semantics:** uncertain Redis outcomes fail closed with `503`, while
  confirmed quota exhaustion returns `429` and `Retry-After`.
- **Policy lifecycle:** configuration initializes safely, updates create a new
  generation, inactive buckets expire, and values are bounded.
- **Verification:** unit and integration tests cover shared quota, atomic
  deduction, reset behavior, validation, retries, and Redis failure.

## Deliberately simplified

- The deliverable is a standalone admission service, not a Kong PDK plugin or
  an inline reverse proxy.
- One global policy contains three mandatory dimensions; selectable dimensions,
  weighted requests, and multiple named policies are omitted.
- Every request costs one token, and policy updates reset effective bucket
  state instead of migrating it.
- The service trusts normalized identity metadata from an internal Gateway and
  does not validate an API key, authorization policy, or mTLS identity.
- The demo uses one Redis Primary and a simple JSON interface; full Gateway,
  control-plane, and upstream integration are outside the MVP.

## Required for production

- **Redis availability:** add replicas with Sentinel or managed failover,
  reconnect clients after promotion, and accept or address the possibility of
  losing recent updates during asynchronous failover.
- **Security:** authenticate callers, use a separate privileged credential for
  configuration writes, add transport encryption, rotate secrets, and restrict
  network access.
- **Integration contract:** define a versioned API or Gateway adapter with
  explicit timeout, retry, identity, and error semantics; add end-to-end tests
  from Gateway through Limiter and Redis to the upstream service.
- **Ambiguous retries:** add request idempotency so a caller can safely retry
  when a response is lost after quota may already have been deducted.
- **Observability:** add metrics, tracing, dashboards, alerts, audit logs, and
  capacity/load tests with production traffic profiles.
- **Operations:** provide automated rollout and rollback, configuration change
  controls, backup/recovery procedures, and multi-host failure testing.
- **Scale beyond one Primary:** if one Redis Primary becomes the bottleneck,
  define a partitioning model or adopt a coordination design that preserves the
  required cross-dimension consistency.

## Ranked engineering decisions

Decisions are ordered from the most architecturally important to the most
implementation-specific.

## ED-001: All-or-nothing admission

A request is allowed only when Path, Client ID, and IP all permit it. Rejected
requests consume no quota.

## ED-002: Centralize quota coordination

Limiter replicas share one Redis quota pool, making the Redis Primary a single
point of failure. Production should use Redis replicas with Sentinel or managed
failover so clients reconnect to a promoted replica when the Primary fails.
Asynchronous failover may still lose recently acknowledged quota updates.

## ED-003: Fail closed on uncertain quota state

If Redis is unavailable, times out, or cannot return a trustworthy decision,
the service returns `503`. It never allows a request when quota cannot be
verified.

## ED-004: Derive Token Bucket behavior from limit and window

Each dimension exposes only `limit` and `window_seconds`. The bucket capacity
and maximum burst equal `limit`, while tokens refill at
`limit / window_seconds`; every request costs one token.

## ED-005: Use Redis as the time authority

All replicas use Redis Primary time to avoid host clock differences. An
alternative is to trust an upstream timestamp, but that requires pre-aligned
clocks and lets caller timing affect quota state, so it was not selected.
Integer microtokens preserve refill precision.

## ED-006: Reset quota on policy updates

Every successful policy update creates a new generation with fresh buckets.
This keeps update behavior simple and predictable, but resets existing quota.

## ED-007: Trust gateway-supplied identity

The MVP trusts Path, Client ID, and source IP supplied by an internal gateway.
Production deployments should add caller authentication and authorization.

## ED-008: Keep the MVP integration interface simple

The MVP exposes a small standalone admission and configuration API without
implementing full gateway or upstream integration. Production should define a
versioned standard contract and add end-to-end tests covering the gateway,
limiter, Redis, and upstream service.

## ED-009: Use Rust and Redis

Rust provides low memory overhead, predictable performance, memory safety, and
simple native deployment. Redis provides fast shared state, atomic updates,
automatic expiry, and a common time source across replicas.

## ED-010: Initialize configuration once

The first starting replica creates the default policy only when none exists.
Concurrent starts do not overwrite each other, and later restarts preserve the
existing policy.

## ED-011: Validate request structure

Invalid requests include missing required fields, wrong JSON types, empty or
oversized values, malformed IP addresses, and unknown fields. They return `400`
without checking or consuming quota.

## ED-012: Retry after the slowest limiting dimension

When multiple dimensions reject a request, the response identifies all of them
and sets `Retry-After` to the longest required wait. This avoids recommending a
retry while any quota is still unavailable.

## ED-013: Expire inactive buckets

A bucket expires after two refill windows without activity, bounding Redis
memory usage. Active buckets refresh their expiry whenever they are evaluated.

## ED-014: Bound policy values

`limit` is restricted to `1`–`1,000,000` and `window_seconds` to `1`–`86,400`.
These bounds reject invalid or extreme policies and keep refill arithmetic
predictable.
