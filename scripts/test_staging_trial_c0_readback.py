"""Exercise the operator command with synthetic summaries."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
FIXTURE = (SCRIPTS / "fixtures/staging_trial_c0/synthetic.json").read_bytes()


class ReadbackTest(unittest.TestCase):
    def run_readback(self, payload, *args):
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPTS / "staging_trial_c0_readback.py"), *args],
            input=payload, capture_output=True, env={}, timeout=5, check=False,
        )

    def report(self, **changes):
        payload = FIXTURE if not changes else json.dumps({**json.loads(FIXTURE), **changes}).encode()
        result = self.run_readback(payload)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, b"")
        return result.stdout.decode().splitlines()

    def test_exact_headroom_with_unconfirmed_scope_and_combination(self):
        self.assertEqual(self.report(), [
            "Hyperdrive configs: CAPACITY (21 used of 25, 4 needed)",
            "Access apps: CAPACITY (8 used of 10, 2 needed)",
            "Dedicated tunnel: CAPACITY (4 used of 5, 1 needed)",
            "One token with Tunnel, Access and Hyperdrive write scope: PENDING (unconfirmed)",
            "Hyperdrive verify-full through Tunnel and Access: PENDING (unconfirmed)",
        ])

    def test_quota_shortages_and_confirmed_stop_conditions(self):
        self.assertEqual(self.report(
            hyperdrive_used=22, access_apps_used=9, tunnels_used=5,
            token_write_scope=False, tunnel_verify_full=False,
        ), [
            "Hyperdrive configs: STOP (22 used of 25, 4 needed)",
            "Access apps: NO CAPACITY (9 used of 10, 2 needed)",
            "Dedicated tunnel: NO CAPACITY (5 used of 5, 1 needed)",
            "One token with Tunnel, Access and Hyperdrive write scope: "
            "STOP (required scopes unavailable without widening a token)",
            "Hyperdrive verify-full through Tunnel and Access: "
            "STOP (combined route does not support verify-full)",
        ])

    def test_unknown_capacity_and_explicit_confirmations(self):
        self.assertEqual(self.report(
            hyperdrive_used=None, access_apps_limit=None, tunnels_used=None,
            token_write_scope=True, tunnel_verify_full=True,
        ), [
            "Hyperdrive configs: PENDING (count or limit unconfirmed)",
            "Access apps: PENDING (count or limit unconfirmed)",
            "Dedicated tunnel: PENDING (count or limit unconfirmed)",
            "One token with Tunnel, Access and Hyperdrive write scope: CONFIRMED",
            "Hyperdrive verify-full through Tunnel and Access: CONFIRMED",
        ])

    def test_partial_hyperdrive_inventory_stays_pending(self):
        first_page = {
            "success": True, "errors": [], "messages": [],
            "result_info": {"page": 1, "per_page": 20, "count": 20, "total_count": 21},
            "result": [{"id": f"{index:032x}"} for index in range(1, 21)],
        }
        last_page = {
            "success": True, "errors": [], "messages": [],
            "result_info": {"page": 2, "per_page": 20, "count": 1, "total_count": 21},
            "result": [{"id": f"{21:032x}"}],
        }
        failure = {"success": False, "errors": [{"code": 1000, "message": "Synthetic failure"}]}
        duplicate = {**last_page, "result": [first_page["result"][0]]}
        cases = [
            ("failed", [first_page, failure], "PENDING (count or limit unconfirmed)"),
            ("unavailable", [first_page], "PENDING (count or limit unconfirmed)"),
            ("duplicate", [first_page, duplicate], "PENDING (count or limit unconfirmed)"),
            ("complete", [first_page, last_page], "CAPACITY (21 used of 25, 4 needed)"),
        ]
        # The stale summary count differs from the verified replay count.
        summary = json.dumps({**json.loads(FIXTURE), "hyperdrive_used": 20}).encode()
        with tempfile.TemporaryDirectory() as scratch:
            replay = Path(scratch) / "synthetic-pages.json"
            for name, pages, verdict in cases:
                with self.subTest(continuation=name):
                    replay.write_text(json.dumps(pages), encoding="utf-8")
                    result = self.run_readback(summary, "--hyperdrive-pages", str(replay))
                    self.assertEqual(result.returncode, 0)
                    self.assertEqual(result.stderr, b"")
                    self.assertEqual(result.stdout.decode().splitlines(), [
                        f"Hyperdrive configs: {verdict}",
                        "Access apps: CAPACITY (8 used of 10, 2 needed)",
                        "Dedicated tunnel: CAPACITY (4 used of 5, 1 needed)",
                        "One token with Tunnel, Access and Hyperdrive write scope: PENDING (unconfirmed)",
                        "Hyperdrive verify-full through Tunnel and Access: PENDING (unconfirmed)",
                    ])

    def test_invalid_input_reports_only_a_generic_error(self):
        summary = json.loads(FIXTURE)
        bad_summaries = [
            b"", b"\xff", b"[]", b"{}", b" " * 4097, b"[" * 2000 + b"]" * 2000,
            FIXTURE.replace(b'"hyperdrive_used": 21', b'"hyperdrive_used": 22, "hyperdrive_used": 21'),
            json.dumps({**summary, "extra_field": "example-only"}).encode(),
            json.dumps({**summary, "hyperdrive_used": True}).encode(),
            json.dumps({**summary, "access_apps_limit": -1}).encode(),
            json.dumps({**summary, "tunnels_used": 4.0}).encode(),
            json.dumps({**summary, "token_write_scope": "true"}).encode(),
            json.dumps({**summary, "tunnel_verify_full": 1}).encode(),
        ]
        for index, payload in enumerate(bad_summaries):
            with self.subTest(case=index):
                result = self.run_readback(payload)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertEqual(result.stderr, b"Invalid C0 readback. Supply a redacted JSON summary as documented.\n")


if __name__ == "__main__":
    unittest.main()
