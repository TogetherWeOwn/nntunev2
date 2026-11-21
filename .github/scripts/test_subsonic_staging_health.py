"""Tests use a fake curl process; never read from staging."""

import runpy
import subprocess
import unittest
from pathlib import Path
from unittest.mock import Mock

HEALTH = runpy.run_path(str(Path(__file__).with_name("subsonic_staging_health.py")))
sample = HEALTH["sample_build_health"]
UnsafeHealth = HEALTH["UnsafeHealth"]
SHA = "a" * 40
BODY = b'{"ok":true,"build_sha":"' + SHA.encode() + b'"}'
FOOTER = b"\n200\nno-store\nDYNAMIC\n"


def curl(output=BODY + FOOTER, returncode=0):
    return Mock(return_value=subprocess.CompletedProcess([], returncode, stdout=output))


class HealthTests(unittest.TestCase):
    def test_single_fixed_origin_probe_uses_start_time_and_caller_deadline(self):
        run = curl()
        observed = sample(
            SHA, deadline_ms=1_500, run=run, monotonic=iter((1.0, 1.2, 1.3)).__next__
        )
        self.assertEqual((observed.observed_ms, observed.build_sha), (1_000, SHA))
        self.assertEqual(run.call_count, 1)
        (argv,), kwargs = run.call_args
        self.assertEqual(argv[:2], ["curl", "--disable"])
        self.assertRegex(
            argv[-1], r"^https://staging\.nntune\.com/healthz\?probe=[a-f0-9]{32}$"
        )
        self.assertIn("Cache-Control: no-cache, no-store", argv)
        self.assertEqual(argv[argv.index("--max-filesize") + 1], "1024")
        self.assertEqual(argv[argv.index("--proto") + 1], "=https")
        self.assertEqual(argv[argv.index("--noproxy") + 1], "*")
        self.assertNotIn("--location", argv)
        self.assertNotIn("--retry", argv)
        self.assertEqual(kwargs["timeout"], 0.5)
        self.assertEqual(set(kwargs["env"]), {"PATH", "LC_ALL"})
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)

    def test_status_and_cache_fail_closed(self):
        for footer, reason in (
            (b"\n302\nno-store\nDYNAMIC\n", "status"),
            (b"\n200\nno-store\nHIT\n0", "cache-unverified"),
            (b"\n200\nno-store\nDYNAMIC\n0", "cache-unverified"),
            (b"\n200\nno-store\n\n", "cache-unverified"),
            (b"\n200\nmax-age=60\nDYNAMIC\n", "cache-unverified"),
        ):
            with (
                self.subTest(footer=footer),
                self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"),
            ):
                sample(
                    SHA, deadline_ms=2000, run=curl(BODY + footer), monotonic=lambda: 1
                )

    def test_body_shape_liveness_and_sha_fail_closed(self):
        for body, reason in (
            (b"x" * 1025, "shape"),
            (b"\xff", "shape"),
            (BODY.replace(b"true", b'true,"ok":true'), "shape"),
            (BODY.replace(b"true", b"1"), "unhealthy-or-wrong-build"),
            (BODY.replace(SHA.encode(), b"unknown"), "unhealthy-or-wrong-build"),
        ):
            with (
                self.subTest(reason=reason),
                self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"),
            ):
                sample(
                    SHA, deadline_ms=2000, run=curl(body + FOOTER), monotonic=lambda: 1
                )

    def test_deadline_includes_validation_and_caps_freshness(self):
        for deadline, clocks, timeout in (
            (1_200, (1, 1.2), 0.2),
            (1_200, (1, 1.1, 1.2), 0.2),
            (9_000, (1, 3.9, 4), 3),
        ):
            run = curl()
            with (
                self.subTest(clocks=clocks),
                self.assertRaisesRegex(UnsafeHealth, "^deadline$"),
            ):
                sample(
                    SHA, deadline_ms=deadline, run=run, monotonic=iter(clocks).__next__
                )
            self.assertEqual(run.call_args.kwargs["timeout"], timeout)
        run = curl()
        with self.assertRaisesRegex(UnsafeHealth, "^deadline$"):
            sample(SHA, deadline_ms=1_000, run=run, monotonic=lambda: 1)
        run.assert_not_called()

    def test_invalid_build_or_deadline_cannot_dispatch(self):
        run = curl()
        for sha, deadline, reason in (
            ("unknown", 2_000, "expected-build"),
            ("A" * 40, 2_000, "expected-build"),
            (SHA, None, "deadline"),
            (SHA, True, "deadline"),
            (SHA, "2000", "deadline"),
            (SHA, -1, "deadline"),
        ):
            with (
                self.subTest(reason=reason),
                self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"),
            ):
                sample(sha, deadline_ms=deadline, run=run, monotonic=lambda: 1)
        run.assert_not_called()

    def test_client_errors_are_sanitized_and_not_retried(self):
        for run, reason in (
            (curl(b"raw-response", returncode=63), "transport"),
            (Mock(side_effect=OSError("raw-transport")), "transport"),
            (Mock(side_effect=subprocess.TimeoutExpired("curl", 1)), "deadline"),
        ):
            with (
                self.subTest(reason=reason),
                self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"),
            ):
                sample(SHA, deadline_ms=2_000, run=run, monotonic=lambda: 1)
            self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()
