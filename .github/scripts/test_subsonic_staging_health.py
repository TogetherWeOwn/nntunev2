"""Tests use fake curl processes; never read from staging."""

import runpy
import subprocess
import unittest
from functools import partial
from pathlib import Path
from unittest.mock import MagicMock, Mock

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
    proc = MagicMock(returncode=returncode)
    proc.__enter__.return_value = proc
    proc.communicate.return_value = (output, None)
    return proc


def curl(output=BODY + FOOTER):
    preflight, probe = process(VERSION), process(output)
    return Mock(side_effect=[preflight, probe]), probe


class HealthTests(unittest.TestCase):
    def refuses(self, reason, popen, deadline=2_000, sha=SHA, **kwargs):
        with self.assertRaisesRegex(UnsafeHealth, f"^{reason}$"):
            sample(sha, deadline_ms=deadline, popen=popen, **kwargs)

    def test_fixed_single_transfer_sends_access_only_on_stdin(self):
        popen, probe = curl()
        clock = iter((1.0, 1.01, 1.1, 1.125, 1.2, 1.3)).__next__
        self.assertEqual(
            sample(SHA, deadline_ms=1_500, popen=popen, monotonic=clock), (1_100, SHA)
        )
        preflight, request = popen.call_args_list
        self.assertEqual(preflight.args[0], ["curl", "--disable", "--version"])
        self.assertEqual(preflight.kwargs["stdin"], subprocess.DEVNULL)
        (argv,), kwargs = request
        self.assertEqual(argv[:2], ["curl", "--disable"])
        self.assertEqual(argv[-1], "https://staging.nntune.com/healthz")
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
        self.assertEqual(kwargs["stdin"], subprocess.PIPE)
        probe.communicate.assert_called_once_with(
            b"CF-Access-Client-Id: fixture-id\nCF-Access-Client-Secret: fixture-secret\n",
            timeout=0.375,
        )

    def test_bad_access_or_sha_never_dispatches(self):
        popen = Mock()
        for access in (
            {},
            {ID_ENV: "fixture-id"},
            {**ACCESS, SECRET_ENV: "x\r\ny"},
            {**ACCESS, ID_ENV: "x" * 513},
            {**ACCESS, ID_ENV: None},
        ):
            self.refuses("access-config", popen, access=access)
        self.refuses("expected-build", popen, sha="unknown")
        popen.assert_not_called()

    def test_bad_health_evidence_is_rejected(self):
        for extra in (
            b',"age":[""]}',
            b',"cache-status":["edge; hit"]}',
            b',"cf-cache-status":["DYNAMIC","HIT"]}',
            b',"cf-cache-status":["HIT"]}',
            b',"content-type":NaN}',
            b',"content-type":[123]}',
            b"",
        ):
            with self.subTest(extra=extra):
                self.refuses("cache-unverified", curl(BODY + FOOTER[:-1] + extra)[0])
        for reply, reason in (
            (BODY + FOOTER.replace(b"200", b"302"), "status"),
            (BODY + FOOTER.replace(b"no-store", b"max-age=60"), "cache-unverified"),
            (b"x" * 1025 + FOOTER, "shape"),
            (BODY.replace(b"true", b'true,"ok":true') + FOOTER, "shape"),
            (BODY.replace(b"true", b"1") + FOOTER, "unhealthy-or-wrong-build"),
            (
                BODY.replace(SHA.encode(), b"unknown") + FOOTER,
                "unhealthy-or-wrong-build",
            ),
        ):
            self.refuses(reason, curl(reply)[0])

    def test_nonfinite_preflight_clock_never_dispatches(self):
        popen = Mock()
        for reading in (float("nan"), float("inf")):
            with self.subTest(reading=reading):
                self.refuses("deadline", popen, monotonic=lambda: reading)
        popen.assert_not_called()

    def test_launch_overrun_never_releases_credentials(self):
        popen, probe = curl()
        clock = iter((1, 1.025, 1.1, 1.2)).__next__
        self.refuses("deadline", popen, 1_200, monotonic=clock)
        probe.communicate.assert_not_called()
        probe.kill.assert_called_once()
        probe.wait.assert_called_once()

    def test_deadline_includes_validation_and_caps_freshness(self):
        for deadline, clocks, calls in (
            (1_000, (1,), 0),
            (1_200, (1, 1.025, 1.2), 1),
            (1_200, (1, 1.025, 1.1, 1.15, 1.2), 2),
            (9_000, (1, 1.05, 1.1, 1.15, 3.9, 4), 2),
        ):
            popen, _ = curl()
            self.refuses("deadline", popen, deadline, monotonic=iter(clocks).__next__)
            self.assertEqual(popen.call_count, calls)

    def test_old_or_unreadable_curl_cannot_dispatch(self):
        for version in (
            VERSION.replace(b"8.4.0", b"8.3.0", 1),
            VERSION.replace(b"libcurl/8.4.0", b"libcurl/8.3.0"),
            b"unknown",
        ):
            popen = Mock(return_value=process(version))
            self.refuses("curl-version", popen)
            popen.assert_called_once()

    def test_client_errors_are_sanitized_and_not_retried(self):
        timed_out = process(b"")
        timed_out.communicate.side_effect = subprocess.TimeoutExpired("curl", 1)
        for failure, reason in (
            (process(b"raw-response", 63), "transport"),
            (OSError("raw-transport"), "transport"),
            (timed_out, "deadline"),
        ):
            popen = Mock(side_effect=[process(VERSION), failure])
            self.refuses(reason, popen)
            self.assertEqual(popen.call_count, 2)
        timed_out.kill.assert_called_once()
        timed_out.wait.assert_called_once()

    def test_health_secrets_stay_in_the_manual_protected_main_job(self):
        path = Path(__file__).parents[1] / "workflows/subsonic-staging-health.yml"
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
        step = "      - name: Sample staging build health\n"
        step += "        env:\n          EXPECTED_STAGING_BUILD_SHA: ${{ inputs.expected_health_sha }}\n"
        for name in ACCESS:
            step += f"          {name}: ${{{{ secrets.{name} }}}}\n"
            self.assertEqual(text.count(name), 2)
        self.assertIn(
            step
            + "        run: python3 -I .github/scripts/subsonic_staging_health.py\n",
            text,
        )
        for other in path.parent.iterdir():
            if other != path and other.suffix in {".yml", ".yaml"}:
                source = other.read_text()
                self.assertFalse(any(name in source for name in ACCESS), other.name)


if __name__ == "__main__":
    unittest.main()
