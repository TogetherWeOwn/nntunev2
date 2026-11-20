#!/usr/bin/env python3
"""Pin bench.py's gate 2 verdict: fast successes must not hide failures.

Run: python3 workers/mb/spike/test_bench.py
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402


def endpoint(statuses: dict[str, int], p95: float | None = 100.0) -> dict:
    return {
        "requests": sum(statuses.values()),
        "statuses": statuses,
        "client_ms": {"p50": p95, "p95": p95, "p99": p95},
        "server_ms": {"p50": None, "p95": None, "p99": None},
        "reads": {"p50": 2, "p95": 3, "max": 4},
    }


def report(search: dict | None = None) -> dict:
    out = {e: endpoint({"200": bench.REQUESTS}) for e in bench.ENDPOINTS}
    if search is not None:
        out["search:release"] = search
    return out


class GateTest(unittest.TestCase):
    def test_all_ok_and_fast_passes(self):
        verdict = bench.gate(report())
        self.assertTrue(verdict["all_requests_ok"])
        self.assertTrue(verdict["pass"])

    def test_partial_failures_fail_despite_fast_p95(self):
        n = bench.REQUESTS
        for statuses in ({"200": n - 100, "500": 100}, {"200": n - 1, "0": 1}):
            with self.subTest(statuses=statuses):
                verdict = bench.gate(report(endpoint(statuses, p95=200.0)))
                self.assertFalse(verdict["all_requests_ok"])
                self.assertFalse(verdict["pass"])

    def test_short_run_fails(self):
        verdict = bench.gate(report(endpoint({"200": bench.REQUESTS - 1})))
        self.assertFalse(verdict["pass"])

    def test_slow_search_fails(self):
        verdict = bench.gate(report(endpoint({"200": bench.REQUESTS}, p95=600.0)))
        self.assertTrue(verdict["all_requests_ok"])
        self.assertFalse(verdict["pass"])


if __name__ == "__main__":
    unittest.main()
