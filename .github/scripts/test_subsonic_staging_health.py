"""Tests use a fake curl process; never read from staging."""

import runpy
import subprocess
import unittest
from functools import partial
from pathlib import Path
from unittest.mock import Mock

HEALTH = runpy.run_path(str(Path(__file__).with_name("subsonic_staging_health.py")))
ID_ENV = "SUBSONIC_HEALTH_ACCESS_CLIENT_ID"
SECRET_ENV = "SUBSONIC_HEALTH_ACCESS_CLIENT_SECRET"
ACCESS = {ID_ENV: "fixture-id", SECRET_ENV: "fixture-secret"}
sample = partial(HEALTH["sample_build_health"], access=ACCESS)
UnsafeHealth = HEALTH["UnsafeHealth"]
SHA = "a" * 40
BODY = b'{"ok":true,"build_sha":"' + SHA.encode() + b'"}'
VERSION = b"curl 8.4.0 (test) libcurl/8.4.0\n"
FOOTER = b'\x1e200\x1e{"cache-control":["no-store"],\n"cf-cache-status":["DYNAMIC"]}'


def process(output, returncode=0):
    return subprocess.CompletedProcess([], returncode, stdout=output)


def curl(output=BODY + FOOTER, returncode=0):
    return Mock(side_effect=[process(VERSION), process(output, returncode)])


class HealthTests(unittest.TestCase):
    def test_single_fixed_origin_probe_uses_start_time_and_caller_deadline(self):
        run = curl()
        observed = sample(
            SHA,
            deadline_ms=1_500,
            run=run,
            monotonic=iter((1.0, 1.1, 1.2, 1.3)).__next__,
        )
        self.assertEqual((observed.observed_ms, observed.build_sha), (1_100, SHA))
        self.assertEqual(run.call_count, 2)
        preflight, probe = run.call_args_list
        self.assertEqual(preflight.args[0], ["curl", "--disable", "--version"])
        self.assertEqual(preflight.kwargs["timeout"], 0.5)
        (argv,), kwargs = probe
        self.assertEqual(argv[:2], ["curl", "--disable"])
        self.assertIn("--http1.1", argv)
        self.assertEqual(
            argv[argv.index("--write-out") + 1], "\x1e%{http_code}\x1e%{header_json}"
        )
        self.assertRegex(
            argv[-1], r"^https://staging\.nntune\.com/healthz\?probe=[a-f0-9]{32}$"
        )
        self.assertIn("Cache-Control: no-cache, no-store", argv)
        self.assertEqual(argv[argv.index("--max-filesize") + 1], "1024")
        self.assertEqual(argv[argv.index("--proto") + 1], "=https")
        self.assertEqual(argv[argv.index("--noproxy") + 1], "*")
        self.assertNotIn("--location", argv)
        self.assertNotIn("--retry", argv)
        self.assertEqual(kwargs["timeout"], 0.4)
        self.assertEqual(set(kwargs["env"]), {"PATH", "LC_ALL"})
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
        self.assertIsNone(preflight.kwargs["input"])
        self.assertEqual(preflight.kwargs["stdin"], subprocess.DEVNULL)
        self.assertIn("@-", argv)
        self.assertEqual(
            kwargs["input"],
            b"CF-Access-Client-Id: fixture-id\nCF-Access-Client-Secret: fixture-secret\n",
        )
        for value in ACCESS.values():
            self.assertNotIn(value, " ".join(argv))
            self.assertNotIn(value, str(kwargs["env"]))

    def test_missing_or_malformed_token_cannot_dispatch(self):
        run = curl()
        for access in (
            {},
            {ID_ENV: "fixture-id"},
            {SECRET_ENV: "fixture-secret"},
            {**ACCESS, SECRET_ENV: "fixture-secret\r\nAdded-Header: value"},
            {**ACCESS, SECRET_ENV: "fixture-secret\n"},
            {**ACCESS, ID_ENV: "id with whitespace"},
            {**ACCESS, ID_ENV: "x" * 513},
        ):
            with self.assertRaisesRegex(UnsafeHealth, "^access-config$"):
                sample(SHA, deadline_ms=2_000, access=access, run=run)
        run.assert_not_called()

    def test_direct_worker_response_does_not_need_edge_cache_status(self):
        # The API Worker renders no-store, but does not add CF-Cache-Status.
        footer = (
            b'\x1e200\x1e{"content-type":["application/json; charset=utf-8"],'
            b'"cache-control":["no-store"]}'
        )
        observed = sample(
            SHA, deadline_ms=2_000, run=curl(BODY + footer), monotonic=lambda: 1
        )
        self.assertEqual((observed.observed_ms, observed.build_sha), (1_000, SHA))

    def test_status_and_cache_fail_closed(self):
        for footer, reason in (
            (FOOTER.replace(b"200", b"302"), "status"),
            (FOOTER.replace(b"DYNAMIC", b"HIT"), "cache-unverified"),
            (FOOTER[:-1] + b',"age":["0"]}', "cache-unverified"),
            (FOOTER[:-1] + b',"age":[""]}', "cache-unverified"),
            (FOOTER.replace(b'"DYNAMIC"', b'"DYNAMIC","HIT"'), "cache-unverified"),
            (
                FOOTER.replace(b'"no-store"', b'"no-store","max-age=60"'),
                "cache-unverified",
            ),
            (FOOTER.replace(b'"DYNAMIC"', b'""'), "cache-unverified"),
            (FOOTER.replace(b'"DYNAMIC"', b""), "cache-unverified"),
            (b"\x1e200\x1e{}", "cache-unverified"),
            (FOOTER.replace(b"no-store", b"max-age=60"), "cache-unverified"),
            (FOOTER[:-1] + b',"cf-cache-status":["HIT"]}', "cache-unverified"),
            (FOOTER[:-1], "cache-unverified"),
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
        for deadline, clocks, calls, timeout in (
            (1_200, (1, 1.2), 1, 0.2),
            (1_200, (1, 1.1, 1.2), 2, 0.1),
            (1_200, (1, 1.025, 1.1, 1.2), 2, 0.175),
            (9_000, (1, 1.1, 3.9, 4), 2, 2.9),
        ):
            run = curl()
            with (
                self.subTest(clocks=clocks),
                self.assertRaisesRegex(UnsafeHealth, "^deadline$"),
            ):
                sample(
                    SHA, deadline_ms=deadline, run=run, monotonic=iter(clocks).__next__
                )
            self.assertEqual(run.call_count, calls)
            self.assertAlmostEqual(run.call_args.kwargs["timeout"], timeout)
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

    def test_old_or_unreadable_curl_cannot_dispatch(self):
        for version in (
            process(b"curl 8.3.0 (test) libcurl/8.3.0\n"),
            process(b"curl 8.4.0 (test) libcurl/8.3.0\n"),
            process(b"unrecognized version output"),
            process(VERSION, returncode=1),
        ):
            with self.subTest(version=version):
                run = Mock(return_value=version)
                with self.assertRaisesRegex(UnsafeHealth, "^curl-version$"):
                    sample(SHA, deadline_ms=2_000, run=run, monotonic=lambda: 1)
                self.assertEqual(run.call_count, 1)
                self.assertEqual(
                    run.call_args.args[0], ["curl", "--disable", "--version"]
                )

    def test_health_secrets_stay_in_the_manual_protected_main_job(self):
        workflows = Path(__file__).resolve().parents[1] / "workflows"
        workflow = workflows / "subsonic-staging-health.yml"
        text = workflow.read_text()
        self.assertRegex(text, r"\non:\n  workflow_dispatch:\n    inputs:\n")
        self.assertIn("\npermissions:\n  contents: read\n", text)
        for guard in (
            "github.repository == 'TogetherWeOwn/nntunev2'",
            "github.ref == 'refs/heads/main' && github.ref_protected",
            "github.workflow_sha == github.sha && github.run_attempt == 1",
            "environment:\n      name: staging\n      deployment: false",
            "ref: ${{ github.sha }}\n          persist-credentials: false",
        ):
            self.assertIn(guard, text)
        self.assertNotIn("upload-artifact", text)
        for name in (ID_ENV, SECRET_ENV):
            self.assertEqual(
                [
                    p.name
                    for p in workflows.glob("*.yml")
                    if f"secrets.{name}" in p.read_text()
                ],
                [workflow.name],
            )

    def test_client_errors_are_sanitized_and_not_retried(self):
        for run, reason in (
            (curl(b"raw-response", returncode=63), "transport"),
            (
                Mock(side_effect=[process(VERSION), OSError("raw-transport")]),
                "transport",
            ),
            (
                Mock(
                    side_effect=[process(VERSION), subprocess.TimeoutExpired("curl", 1)]
                ),
                "deadline",
            ),
        ):
            with (
                self.subTest(reason=reason),
                self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"),
            ):
                sample(SHA, deadline_ms=2_000, run=run, monotonic=lambda: 1)
            self.assertEqual(run.call_count, 2)


if __name__ == "__main__":
    unittest.main()
