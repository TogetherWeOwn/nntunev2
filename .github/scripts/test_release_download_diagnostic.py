"""Offline contracts: synthetic bytes only; no asset retrieval or execution."""

from contextlib import redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("release_download_diagnostic.py")
spec = importlib.util.spec_from_file_location("diagnostic", SCRIPT)
diagnostic = importlib.util.module_from_spec(spec)
spec.loader.exec_module(diagnostic)
ROOT = SCRIPT.parents[2]
SYNTHETIC = b"synthetic test bytes, not executable"
ASSET = ("synthetic", "https://github.com/example/project/releases/download/v1/test",
         hashlib.sha256(SYNTHETIC).hexdigest())


class DownloadDiagnosticTests(unittest.TestCase):
    def run_probe(self, responses):
        calls = []
        roots = []

        def request(url, output):
            calls.append(url)
            roots.append(output.parent)
            self.assertFalse(output.exists())
            rc, status, headers, location, data = responses[len(calls) - 1]
            if data is not None:
                output.write_bytes(data)
            return rc, {"status": status}, headers, location

        with tempfile.TemporaryDirectory() as root, io.StringIO() as out:
            with patch.object(diagnostic, "request", side_effect=request), redirect_stdout(out):
                result = diagnostic.probe(ASSET, root)
            self.assertEqual(list(Path(root).iterdir()), [])
            records = [json.loads(line) for line in out.getvalue().splitlines()]
        self.assertTrue(all(not p.exists() for p in roots))
        self.assertEqual(records[-1]["cleanup"], "passed")
        return result, records, calls

    def test_exact_bytes_and_cleanup(self):
        passed, records, _ = self.run_probe([(0, 200, {}, None, SYNTHETIC)])
        self.assertTrue(passed)
        self.assertEqual(records[-2]["checksum"], "passed")

    def test_redirect_query_and_headers_are_not_logged(self):
        signed = "https://release-assets.githubusercontent.com/asset?signature=DO_NOT_LOG"
        passed, records, calls = self.run_probe([
            (0, 302, {}, signed, b"redirect body"),
            (0, 200, {}, None, SYNTHETIC),
        ])
        self.assertTrue(passed)
        self.assertEqual(calls[-1], signed)
        self.assertNotIn("DO_NOT_LOG", json.dumps(records))
        self.assertEqual(records[-2]["final_host"], "release-assets.githubusercontent.com")

    def test_504_and_mismatched_bytes_fail_closed(self):
        for response in [(22, 504, {}, None, None), (0, 200, {}, None, b"wrong")]:
            with self.subTest(response=response):
                passed, records, calls = self.run_probe([response])
                self.assertFalse(passed)
                self.assertEqual(len(calls), 1)
                self.assertEqual(records[-1]["result"], "failed")

    def test_unapproved_redirect_is_not_requested(self):
        for location in ["http://github.com/no", "https://evil.example/a",
                         "https://user:pass@github.com/a", "https://github.com:444/a",
                         'https://github.com/"bad', "https://github.com/\ninvalid",
                         "https://github.com/\rinvalid", "https://github.com/\tinvalid"]:
            with self.subTest(location=location):
                passed, _, calls = self.run_probe([(0, 302, {}, location, b"")])
                self.assertFalse(passed)
                self.assertEqual(len(calls), 1)

    def test_redirect_limit_is_bounded(self):
        passed, _, calls = self.run_probe([
            (0, 302, {}, "https://github.com/loop", b"")
        ] * diagnostic.MAX_HOPS)
        self.assertFalse(passed)
        self.assertEqual(len(calls), diagnostic.MAX_HOPS)

    def test_curl_metadata_is_allowlisted_and_url_is_stdin_only(self):
        raw = (b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 504\r\n"
               b"x-github-request-id: AB12:CD34\r\nset-cookie: DO_NOT_LOG\r\n"
               b"authorization: DO_NOT_LOG\r\nx-cache: MISS\r\n"
               b"location: https://release-assets.githubusercontent.com/a?DO_NOT_LOG\r\n\r\n"
               b"\nNNTUNE_TIMING 504 0.001000 0.002000 0.003000 0.004000 0.005000\n")
        with patch.object(diagnostic.subprocess, "run", return_value=subprocess.CompletedProcess([], 22, raw)) as run:
            rc, timing, safe, location = diagnostic.request(ASSET[1], Path("unused"))
        self.assertEqual(rc, 22)
        self.assertEqual(timing["status"], 504)
        self.assertEqual(safe, {"x-github-request-id": "AB12:CD34", "x-cache": "MISS"})
        self.assertIn("DO_NOT_LOG", location)
        command = run.call_args.args[0]
        self.assertEqual(command[0:2], ["curl", "--disable"])
        self.assertNotIn(ASSET[1], command)
        self.assertNotIn("--location", command)
        self.assertNotIn("--retry", command)
        self.assertEqual(run.call_args.kwargs["timeout"], 32)
        self.assertEqual(run.call_args.kwargs["stderr"], subprocess.DEVNULL)

    def test_no_response_preserves_hop_evidence_and_cleanup(self):
        raw = b"\nNNTUNE_TIMING 000 0.010000 0.000000 0.000000 0.000000 10.001000\n"
        for rc in (6, 28, 35):
            with self.subTest(curl_exit=rc):
                response = subprocess.CompletedProcess([], rc, raw)
                with tempfile.TemporaryDirectory() as root, io.StringIO() as out:
                    with patch.object(diagnostic.subprocess, "run", return_value=response) as run:
                        with redirect_stdout(out):
                            self.assertFalse(diagnostic.probe(ASSET, root))
                    self.assertEqual(run.call_count, 1)
                    self.assertEqual(list(Path(root).iterdir()), [])
                    records = [json.loads(line) for line in out.getvalue().splitlines()]
                self.assertEqual(len(records), 2)
                self.assertEqual(records[0], {
                    "asset": "synthetic", "hop": 0, "host": "github.com",
                    "curl_exit": rc, "headers": {}, "status": 0,
                    "dns_s": 0.01, "connect_s": 0.0, "tls_s": 0.0,
                    "first_byte_s": 0.0, "total_s": 10.001,
                })
                self.assertEqual(records[-1]["cleanup"], "passed")
                self.assertEqual(records[-1]["result"], "failed")

    def test_raw_redirect_controls_are_not_normalized_or_requested(self):
        for control in [*range(32), 127]:
            for final in (False, True):
                for leading in (False, True):
                    with self.subTest(control=control, final=final, leading=leading):
                        location = b"https://github.com/example?DO_NOT_LOG"
                        byte = bytes([control])
                        location = byte + location if leading else location + byte
                        fields = [b"Location: " + location, b"x-cache: MISS"]
                        if final:
                            fields.reverse()
                        raw = (b"HTTP/1.1 302 Found\r\n" + b"\r\n".join(fields)
                               + b"\r\n\r\n\nNNTUNE_TIMING 302 0.001000 0.002000 "
                               b"0.003000 0.004000 0.005000\n")
                        response = subprocess.CompletedProcess([], 0, raw)
                        with tempfile.TemporaryDirectory() as root, io.StringIO() as out:
                            with patch.object(diagnostic.subprocess, "run", return_value=response) as run:
                                with redirect_stdout(out):
                                    self.assertFalse(diagnostic.probe(ASSET, root))
                            self.assertEqual(run.call_count, 1)
                            self.assertEqual(list(Path(root).iterdir()), [])
                            records = [json.loads(line) for line in out.getvalue().splitlines()]
                        self.assertEqual(records[-1]["cleanup"], "passed")
                        self.assertEqual(records[-2]["failure_type"], "ValueError")
                        self.assertNotIn("DO_NOT_LOG", json.dumps(records))

    def test_raw_valid_redirect_reaches_checksum_and_cleanup(self):
        signed = "https://release-assets.githubusercontent.com/asset?signature=DO_NOT_LOG"
        redirect = (b"HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 302\r\n"
                    + f"Location: {signed}\r\n\r\n".encode())
        success = b"HTTP/2 200\r\ncontent-length: 36\r\n\r\n"
        responses = [(302, redirect), (200, success)]
        calls = []

        def curl(command, **kwargs):
            calls.append(kwargs["input"])
            status, headers = responses[len(calls) - 1]
            output = Path(command[command.index("--output") + 1])
            output.write_bytes(SYNTHETIC if status == 200 else b"")
            raw = headers + f"\nNNTUNE_TIMING {status} 0.001000 0.002000 0.003000 0.004000 0.005000\n".encode()
            return subprocess.CompletedProcess(command, 0, raw)

        with tempfile.TemporaryDirectory() as root, io.StringIO() as out:
            with patch.object(diagnostic.subprocess, "run", side_effect=curl), redirect_stdout(out):
                self.assertTrue(diagnostic.probe(ASSET, root))
            self.assertEqual(list(Path(root).iterdir()), [])
            records = [json.loads(line) for line in out.getvalue().splitlines()]
        self.assertEqual(calls, [f'url = "{url}"\n'.encode() for url in (ASSET[1], signed)])
        self.assertEqual(records[-2]["checksum"], "passed")
        self.assertEqual(records[-1]["cleanup"], "passed")
        self.assertNotIn("DO_NOT_LOG", json.dumps(records))

    def test_curl_globbing_is_disabled_for_literal_redirects(self):
        raw = (b"HTTP/2 504\r\n\r\n\nNNTUNE_TIMING 504 0.001000 0.002000 "
               b"0.003000 0.004000 0.005000\n")
        for path in ("{first,second}", "[1-3]"):
            url = "https://github.com/" + path
            with self.subTest(path=path):
                self.assertEqual(diagnostic.host(url), "github.com")
                with patch.object(diagnostic.subprocess, "run", return_value=subprocess.CompletedProcess([], 22, raw)) as run:
                    diagnostic.request(url, Path("unused"))
                command = run.call_args.args[0]
                self.assertEqual(command[:3], ["curl", "--disable", "--globoff"])
                self.assertNotIn(url, command)
                self.assertEqual(run.call_args.kwargs["input"], f'url = "{url}"\n'.encode())

    def test_exception_cleanup_and_no_exception_text(self):
        with tempfile.TemporaryDirectory() as root, io.StringIO() as out:
            with patch.object(diagnostic, "request", side_effect=TimeoutError("DO_NOT_LOG")), redirect_stdout(out):
                self.assertFalse(diagnostic.probe(ASSET, root))
            self.assertEqual(list(Path(root).iterdir()), [])
            self.assertNotIn("DO_NOT_LOG", out.getvalue())
            self.assertIn('"cleanup": "passed"', out.getvalue())

    def test_hosted_only_and_all_assets_attempted(self):
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(SystemExit):
            diagnostic.main()
        env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
               "GITHUB_SHA": "a" * 40, "RUNNER_TEMP": "/unused"}
        with patch.dict(os.environ, env, clear=True), patch.object(diagnostic.signal, "signal"):
            with patch.object(diagnostic, "probe", side_effect=[False, True, True, True]) as probe:
                with redirect_stdout(io.StringIO()):
                    self.assertEqual(diagnostic.main(), 1)
                self.assertEqual(probe.call_count, 4)

    def test_manual_wiring_and_required_test_step(self):
        workflow = (ROOT / ".github/workflows/pr-lint.yml").read_text()
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.release_download_diagnostic", workflow)
        self.assertIn("default: false", workflow)
        self.assertIn("ref: ${{ github.sha }}", workflow)
        self.assertIn("persist-credentials: false", workflow)
        job = workflow.split("  release-download-diagnostic:", 1)[1]
        self.assertIn("name: Pinned release download diagnostic", job)
        self.assertNotIn("secrets.", job)
        self.assertNotIn("write", job)
        self.assertIn("contents: read", job)
        self.assertIn("timeout-minutes: 12", job)
        self.assertIn("python3 -I .github/scripts/test_release_download_diagnostic.py",
                      (ROOT / ".github/workflows/ci.yml").read_text())


if __name__ == "__main__":
    unittest.main()
