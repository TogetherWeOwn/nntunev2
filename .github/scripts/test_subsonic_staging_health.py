"""Tests use fake curl processes; never read from staging."""

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
sample = partial(HEALTH["sample_build_health"], access=ACCESS, monotonic=lambda: 1)
UnsafeHealth = HEALTH["UnsafeHealth"]
SHA = "a" * 40
BODY = b'{"ok":true,"build_sha":"' + SHA.encode() + b'"}'
VERSION = b"curl 8.4.0 (test) libcurl/8.4.0\n"
FOOTER = b'\x1e200\x1e{"cache-control":["no-store"]}'


def process(output, returncode=0):
    return subprocess.CompletedProcess([], returncode, stdout=output)


def curl(output=BODY + FOOTER):
    return Mock(side_effect=[process(VERSION), process(output)])


class HealthTests(unittest.TestCase):
    def test_fixed_single_transfer_sends_access_only_on_stdin(self):
        run = curl()
        clock = iter((1.0, 1.1, 1.2, 1.3)).__next__
        self.assertEqual(
            sample(SHA, deadline_ms=1_500, run=run, monotonic=clock), (1_100, SHA)
        )
        preflight, probe = run.call_args_list
        self.assertEqual(preflight.args[0], ["curl", "--disable", "--version"])
        self.assertIsNone(preflight.kwargs["input"])
        self.assertEqual(preflight.kwargs["timeout"], 0.5)
        (argv,), kwargs = probe
        self.assertEqual(argv[:2], ["curl", "--disable"])
        self.assertRegex(
            argv[-1], r"^https://staging\.nntune\.com/healthz\?probe=[a-f0-9]{32}$"
        )
        for flag, value in (
            ("--max-filesize", "1024"),
            ("--proto", "=https"),
            ("--noproxy", "*"),
        ):
            self.assertEqual(argv[argv.index(flag) + 1], value)
        for option in ("--http1.1", "@-", "Cache-Control: no-cache, no-store"):
            self.assertIn(option, argv)
        for option in ("--location", "--retry", *ACCESS.values()):
            self.assertNotIn(option, " ".join(argv))
        self.assertEqual(set(kwargs["env"]), {"PATH", "LC_ALL"})
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
        self.assertEqual(kwargs["timeout"], 0.4)
        self.assertEqual(
            kwargs["input"].splitlines(),
            [
                b"CF-Access-Client-Id: fixture-id",
                b"CF-Access-Client-Secret: fixture-secret",
            ],
        )

    def test_bad_access_or_sha_never_dispatches(self):
        run = curl()
        for access in (
            {},
            {ID_ENV: "fixture-id"},
            {**ACCESS, SECRET_ENV: "line\r\nbreak"},
            {**ACCESS, ID_ENV: "x" * 513},
        ):
            with self.assertRaisesRegex(UnsafeHealth, "^access-config$"):
                sample(SHA, deadline_ms=2_000, access=access, run=run)
        with self.assertRaisesRegex(UnsafeHealth, "^expected-build$"):
            sample("unknown", deadline_ms=2_000, run=run)
        run.assert_not_called()

    def test_bad_health_evidence_is_rejected(self):
        for reply, reason in (
            (BODY + FOOTER.replace(b"200", b"302"), "status"),
            (BODY + FOOTER[:-1] + b',"age":[""]}', "cache-unverified"),
            (
                BODY + FOOTER[:-1] + b',"cf-cache-status":["DYNAMIC","HIT"]}',
                "cache-unverified",
            ),
            (BODY + FOOTER[:-1] + b',"cf-cache-status":["HIT"]}', "cache-unverified"),
            (
                BODY + FOOTER.replace(b'"no-store"', b'"no-store","max-age=60"'),
                "cache-unverified",
            ),
            (BODY + FOOTER.replace(b"no-store", b"max-age=60"), "cache-unverified"),
            (BODY + FOOTER[:-1], "cache-unverified"),
            (b"x" * 1025 + FOOTER, "shape"),
            (BODY.replace(b"true", b'true,"ok":true') + FOOTER, "shape"),
            (BODY.replace(b"true", b"1") + FOOTER, "unhealthy-or-wrong-build"),
            (
                BODY.replace(SHA.encode(), b"unknown") + FOOTER,
                "unhealthy-or-wrong-build",
            ),
        ):
            with self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"):
                sample(SHA, deadline_ms=2_000, run=curl(reply))

    def test_deadline_includes_validation_and_caps_freshness(self):
        for deadline, clocks, calls in (
            (1_000, (1,), 0),
            (1_200, (1, 1.2), 1),
            (1_200, (1, 1.025, 1.1, 1.2), 2),
            (9_000, (1, 1.1, 3.9, 4), 2),
        ):
            run = curl()
            with self.assertRaisesRegex(UnsafeHealth, "^deadline$"):
                sample(
                    SHA, deadline_ms=deadline, run=run, monotonic=iter(clocks).__next__
                )
            self.assertEqual(run.call_count, calls)

    def test_old_or_unreadable_curl_cannot_dispatch(self):
        for version in (
            VERSION.replace(b"8.4.0", b"8.3.0", 1),
            VERSION.replace(b"libcurl/8.4.0", b"libcurl/8.3.0"),
            b"unknown",
        ):
            run = Mock(return_value=process(version))
            with self.assertRaisesRegex(UnsafeHealth, "^curl-version$"):
                sample(SHA, deadline_ms=2_000, run=run)
            run.assert_called_once()

    def test_client_errors_are_sanitized_and_not_retried(self):
        for failure, reason in (
            (process(b"raw-response", 63), "transport"),
            (OSError("raw-transport"), "transport"),
            (subprocess.TimeoutExpired("curl", 1), "deadline"),
        ):
            run = Mock(side_effect=[process(VERSION), failure])
            with self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"):
                sample(SHA, deadline_ms=2_000, run=run)
            self.assertEqual(run.call_count, 2)

    def test_health_secrets_stay_in_the_manual_protected_main_job(self):
        path = (
            Path(__file__).resolve().parents[1]
            / "workflows/subsonic-staging-health.yml"
        )
        text = path.read_text()
        for guard in (
            "\non:\n  workflow_dispatch:\n    inputs:\n",
            "\npermissions:\n  contents: read\n",
            "github.ref == 'refs/heads/main' && github.ref_protected",
            "github.workflow_sha == github.sha && github.run_attempt == 1",
            "environment:\n      name: staging\n      deployment: false",
            "ref: ${{ github.sha }}\n          persist-credentials: false",
        ):
            self.assertIn(guard, text)
        self.assertNotIn("upload-artifact", text)
        for name in (ID_ENV, SECRET_ENV):
            matches = [
                p
                for p in path.parent.glob("*.yml")
                if f"secrets.{name}" in p.read_text()
            ]
            self.assertEqual(matches, [path])


if __name__ == "__main__":
    unittest.main()
