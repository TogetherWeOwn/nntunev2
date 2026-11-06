#!/usr/bin/env python3
"""Uncached latency for the R2 snapshot spike Worker (gate 2).

Usage: bench.py <base-url> <lookup-mbids-dir> <golden.tsv> <out.json>

Method (`workers/mb/README.md` "p95 methodology"): per endpoint, 1,000
requests at 50 concurrent. The spike Worker has no KV or edge cache, and
every request uses a distinct MBID or query, so each is a backend read.
`MB_SNAPSHOT_BENCH_TOKEN` (env) is sent in `x-mb-snapshot-bench`.
Records client wall time and the Worker's `Server-Timing` duration and
R2 read count. `<lookup-mbids-dir>` holds `artist.txt`, `release.txt`
and `recording.txt` (one MBID per line).
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REQUESTS = 1000
CONCURRENCY = 50
TIMING = re.compile(r'snapshot;dur=([0-9.]+);desc="reads=([0-9]+)"')


def fetch(url: str, token: str) -> dict:
    request = urllib.request.Request(url, headers={
        "x-mb-snapshot-bench": token,
        "user-agent": "nntune-mb-snapshot-spike-bench",
    })
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            response.read()
            status, timing = response.status, response.headers.get("server-timing", "")
    except urllib.error.HTTPError as err:
        err.read()
        status, timing = err.code, err.headers.get("server-timing", "")
    except Exception:  # noqa: BLE001 - a transport failure is a data point
        status, timing = 0, ""
    elapsed = (time.perf_counter() - started) * 1000
    match = TIMING.search(timing or "")
    return {
        "status": status,
        "client_ms": elapsed,
        "server_ms": float(match.group(1)) if match else None,
        "reads": int(match.group(2)) if match else None,
    }


def pct(values: list[float], p: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, int(round(p / 100 * (len(ordered) - 1))))], 1)


def run(urls: list[str], token: str) -> dict:
    with ThreadPoolExecutor(max_workers=CONCURRENCY) as pool:
        results = list(pool.map(lambda url: fetch(url, token), urls))
    ok = [r for r in results if r["status"] == 200]
    statuses: dict[str, int] = {}
    for r in results:
        statuses[str(r["status"])] = statuses.get(str(r["status"]), 0) + 1
    client = [r["client_ms"] for r in ok]
    server = [r["server_ms"] for r in ok if r["server_ms"] is not None]
    reads = [r["reads"] for r in ok if r["reads"] is not None]
    return {
        "requests": len(results),
        "statuses": statuses,
        "client_ms": {"p50": pct(client, 50), "p95": pct(client, 95), "p99": pct(client, 99)},
        "server_ms": {"p50": pct(server, 50), "p95": pct(server, 95), "p99": pct(server, 99)},
        "reads": {"p50": pct(reads, 50), "p95": pct(reads, 95), "max": max(reads, default=None)},
    }


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__.splitlines()[2], file=sys.stderr)
        return 2
    base, mbids_dir, golden, out = sys.argv[1].rstrip("/"), Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4])
    token = os.environ["MB_SNAPSHOT_BENCH_TOKEN"]
    report = {}
    for kind in ("artist", "release", "recording"):
        mbids = mbids_dir.joinpath(f"{kind}.txt").read_text().split()[:REQUESTS]
        report[f"lookup:{kind}"] = run([f"{base}/v1/mb/{kind}/{m}" for m in mbids], token)
    queries = [line.split("\t")[1] for line in golden.read_text(encoding="utf-8").splitlines()][:REQUESTS]
    report["search:release"] = run(
        [f"{base}/v1/mb/search?type=release&q={urllib.parse.quote(q)}" for q in queries], token)
    lookup_p95 = max(report[f"lookup:{k}"]["client_ms"]["p95"] or 1e9 for k in ("artist", "release", "recording"))
    search = report["search:release"]
    report["gate_2"] = {
        "lookup_client_p95_ms": lookup_p95,
        "search_client_p95_ms": search["client_ms"]["p95"],
        "search_reads_p95": search["reads"]["p95"],
        "pass": lookup_p95 < 250 and (search["client_ms"]["p95"] or 1e9) < 600,
    }
    out.write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
