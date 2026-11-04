#!/usr/bin/env python3
"""Generate deterministic burst, steady, or ramp admission traffic."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import statistics
import sys
import time
import urllib.error
import urllib.request
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path


@dataclass(frozen=True)
class Result:
    index: int
    instance: str
    status: str
    latency_ms: float


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base-urls",
        default="http://127.0.0.1:8081,http://127.0.0.1:8082,http://127.0.0.1:8083",
        help="comma-separated limiter base URLs",
    )
    parser.add_argument("--pattern", choices=("burst", "steady", "ramp"), default="burst")
    parser.add_argument("--requests", type=int, default=200, help="burst request count")
    parser.add_argument("--duration", type=float, default=10.0, help="steady/ramp duration")
    parser.add_argument("--rate", type=float, default=20.0, help="steady requests per second")
    parser.add_argument("--start-rate", type=float, default=5.0, help="ramp starting rate")
    parser.add_argument("--end-rate", type=float, default=100.0, help="ramp ending rate")
    parser.add_argument("--concurrency", type=int, default=32)
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("--clients", type=int, default=1)
    parser.add_argument("--paths", type=int, default=1)
    parser.add_argument("--ips", type=int, default=1)
    parser.add_argument("--output", type=Path, help="optional JSON results file")
    args = parser.parse_args()
    if min(args.requests, args.concurrency, args.clients, args.paths, args.ips) < 1:
        parser.error("requests, concurrency, clients, paths, and ips must be positive")
    if args.duration <= 0 or args.rate <= 0 or args.start_rate <= 0 or args.end_rate <= 0:
        parser.error("duration and rates must be positive")
    return args


def schedule(args: argparse.Namespace) -> list[float]:
    if args.pattern == "burst":
        return [0.0] * args.requests
    if args.pattern == "steady":
        count = max(1, math.floor(args.duration * args.rate))
        return [i / args.rate for i in range(count)]

    events: list[float] = []
    credit = 0.0
    step = 0.02
    elapsed = 0.0
    while elapsed < args.duration:
        fraction = elapsed / args.duration
        rate = args.start_rate + (args.end_rate - args.start_rate) * fraction
        credit += rate * step
        while credit >= 1.0:
            events.append(elapsed)
            credit -= 1.0
        elapsed += step
    return events


def source_ip(index: int, count: int) -> str:
    identity = index % count
    third = (identity // 250) % 250
    fourth = identity % 250 + 1
    return f"203.0.{third}.{fourth}"


def send(index: int, urls: list[str], args: argparse.Namespace) -> Result:
    base_url = urls[index % len(urls)]
    payload = {
        "request_id": f"simulation-{index}",
        "path": f"/simulation/path-{index % args.paths}",
        "client_id": f"simulation-client-{index % args.clients}",
        "source_ip": source_ip(index, args.ips),
    }
    request = urllib.request.Request(
        f"{base_url}/v1/admissions",
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=args.timeout) as response:
            status = str(response.status)
            response.read()
    except urllib.error.HTTPError as error:
        status = str(error.code)
        error.read()
    except Exception as error:  # Report transport errors as a separate result class.
        status = f"error:{type(error).__name__}"
    latency_ms = (time.perf_counter() - started) * 1000
    return Result(index=index, instance=base_url, status=status, latency_ms=latency_ms)


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, math.ceil(len(ordered) * fraction) - 1)
    return ordered[index]


def main() -> int:
    args = parse_args()
    urls = [url.rstrip("/") for url in args.base_urls.split(",") if url.strip()]
    if not urls:
        print("no base URLs supplied", file=sys.stderr)
        return 2

    offsets = schedule(args)
    print(
        f"pattern={args.pattern} requests={len(offsets)} concurrency={args.concurrency} "
        f"instances={len(urls)}"
    )

    results: list[Result] = []
    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        futures: list[concurrent.futures.Future[Result]] = []
        for index, offset in enumerate(offsets):
            delay = started + offset - time.perf_counter()
            if delay > 0:
                time.sleep(delay)
            futures.append(executor.submit(send, index, urls, args))
        for future in concurrent.futures.as_completed(futures):
            results.append(future.result())

    elapsed = time.perf_counter() - started
    latencies = [result.latency_ms for result in results]
    statuses = Counter(result.status for result in results)
    per_instance = Counter(result.instance for result in results)
    summary = {
        "pattern": args.pattern,
        "requests": len(results),
        "elapsed_seconds": round(elapsed, 3),
        "achieved_requests_per_second": round(len(results) / elapsed, 2) if elapsed else 0,
        "statuses": dict(sorted(statuses.items())),
        "requests_per_instance": dict(sorted(per_instance.items())),
        "latency_ms": {
            "mean": round(statistics.fmean(latencies), 3) if latencies else 0,
            "p50": round(percentile(latencies, 0.50), 3),
            "p95": round(percentile(latencies, 0.95), 3),
            "p99": round(percentile(latencies, 0.99), 3),
            "max": round(max(latencies), 3) if latencies else 0,
        },
    }
    print(json.dumps(summary, indent=2, sort_keys=True))

    if args.output:
        args.output.write_text(
            json.dumps(
                {"summary": summary, "results": [asdict(result) for result in results]},
                indent=2,
                sort_keys=True,
            )
            + "\n"
        )
        print(f"wrote {args.output}")

    return 1 if any(status.startswith("error:") for status in statuses) else 0


if __name__ == "__main__":
    raise SystemExit(main())

