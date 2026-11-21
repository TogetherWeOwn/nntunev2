"""Offline tests; the provider boundary is always mocked."""

import importlib.util
import io
import json
import subprocess
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory
from textwrap import dedent
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location("identity", Path(__file__).with_name("staging_api_identity.py"))
identity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(identity)

ACCOUNT = "a" * 32
SHA = "b" * 40
DEPLOYMENT = "11111111-1111-4111-8111-111111111111"
VERSION = "22222222-2222-4222-8222-222222222222"
OTHER = "33333333-3333-4333-8333-333333333333"
MATCHED = {"identity": "matched", "deployment_id": DEPLOYMENT, "version_id": VERSION, "build_sha": SHA,
           "enable_flag": "off", "lease": "absent", "measurement_db_binding": "absent"}


def output(**changes):
    deploy = {"type": "deploy", "version": 1, "worker_name": "nntune-api-staging",
              "wrangler_environment": "staging", "version_id": VERSION}
    return json.dumps(deploy | changes).encode() + b"\n"


def current(deployment_id=DEPLOYMENT, version_id=VERSION, percentage=100):
    versions = [{"version_id": version_id, "percentage": percentage}]
    return {"deployments": [{"id": deployment_id, "strategy": "percentage", "versions": versions}]}


def version(*bindings, sha=SHA):
    build = {"name": "BUILD_SHA", "type": "plain_text", "text": sha}
    return {"id": VERSION, "resources": {"bindings": [build, *bindings]}}


class IdentityTests(unittest.TestCase):
    def test_cli_reports_matched_or_unavailable(self):
        deployment = identity.decode(json.dumps(current()).replace("100}", "100.0}").encode(), "bad fixture")
        deployment["deployments"] += current(OTHER, OTHER)["deployments"]
        read = MagicMock(side_effect=[deployment, version(), deployment])
        stream = io.StringIO()
        with TemporaryDirectory() as temp, patch.object(identity, "provider_result", read):
            Path(temp, "wrangler-output-test.json").write_bytes(b'{"type":"wrangler-session","version":1}\n' + output())
            env = {"WRANGLER_OUTPUT_FILE_DIRECTORY": temp, "GITHUB_SHA": SHA,
                   "CLOUDFLARE_ACCOUNT_ID": ACCOUNT, "CLOUDFLARE_API_TOKEN": "synthetic-token"}
            with patch.dict(identity.os.environ, env, clear=True), redirect_stdout(stream):
                self.assertEqual(identity.main(), 0)
        self.assertEqual(json.loads(stream.getvalue()), MATCHED)
        path = f"/client/v4/accounts/{ACCOUNT}/workers/scripts/nntune-api-staging"
        self.assertEqual([c.args[0] for c in read.call_args_list],
                         [path + endpoint for endpoint in ("/deployments", f"/versions/{VERSION}", "/deployments")])
        with patch.dict(identity.os.environ, {}, clear=True), redirect_stdout(io.StringIO()) as unavailable:
            self.assertEqual(identity.main(), 1)
        self.assertEqual(json.loads(unavailable.getvalue()), {"identity": "unavailable", "reason": "credential-config"})

    def test_bad_deploy_output_never_reads_provider(self):
        for body in (
            b"", b"{" + b"x" * identity.MAX_DEPLOY_OUTPUT, output()[:-1], output() * 2,
            output(version=True), output(version_id=None),
            output(worker_name="wrong"), output(wrangler_environment="production"),
            output().replace(b'"version": 1', b'"version": 2, "version": 1'),
        ):
            with self.subTest(body=body[:30]):
                read = MagicMock()
                with self.assertRaisesRegex(identity.Unavailable, "deploy-output-unavailable"):
                    identity.receipt(body, SHA, ACCOUNT, read)
                read.assert_not_called()

    def test_traffic_and_drift_rejected(self):
        split = current(percentage=99)
        split["deployments"][0]["versions"].append({"version_id": OTHER, "percentage": 1})
        ambiguous = current()
        ambiguous["deployments"] *= 2
        near_miss = json.dumps(current()).replace("100}", "99.999999999999999}")
        for observed, reason in (
            (identity.decode(near_miss.encode(), "bad fixture"), "deployment-traffic-shape"),
            (split, "deployment-not-single-version"),
            (ambiguous, "deployment-shape"),
            ({"deployments": []}, "deployment-shape"),
            (current(percentage="100"), "deployment-traffic-shape"),
            (current(version_id=OTHER), "deployed-version-mismatch"),
        ):
            with self.subTest(reason=reason), self.assertRaisesRegex(identity.Unavailable, reason):
                identity.receipt(output(), SHA, ACCOUNT, MagicMock(return_value=observed))
        read = MagicMock(side_effect=[current(), version(), current(OTHER)])
        with self.assertRaisesRegex(identity.Unavailable, "deployment-changed-during-readback"):
            identity.receipt(output(), SHA, ACCOUNT, read)

    def test_version_mismatch_or_unsupported_readback(self):
        for observed, reason in (
            (dict(version(), id=OTHER), "provider-version-mismatch"),
            ({"id": VERSION}, "version-bindings-unavailable"),
            (version({"name": "BUILD_SHA", "type": "secret_text"}), "version-bindings-shape"),
            (version(sha="not-a-sha"), "version-build-unavailable"),
            (version(sha="e" * 40), "version-build-mismatch"),
        ):
            with self.subTest(reason=reason), self.assertRaisesRegex(identity.Unavailable, reason):
                identity.verify_version(observed, VERSION, SHA)

    def test_default_off_never_inferred_from_incompatible_bindings(self):
        for name, binding in (
            ("ENABLE", {"type": "plain_text", "text": "1"}),
            ("ENABLE", {"type": "secret_text"}),
            ("LEASE", {"type": "secret_text"}),
            ("DB", {"type": "hyperdrive"}),
        ):
            with self.subTest(name=name), self.assertRaisesRegex(identity.Unavailable, "default-off-not-verified"):
                identity.verify_version(version(dict(binding, name="SUBSONIC_MEASUREMENT_" + name)), VERSION, SHA)

    @patch.object(identity.http.client, "HTTPSConnection")
    def test_provider_bounds_and_no_redirect_or_raw_response(self, connect):
        conn = connect.return_value
        valid = b'{"success":true,"errors":[],"result":{}}'
        chunked = "Transfer-Encoding: chunked"
        chunk = b"5\r\n" + valid[:5] + f"\r\n{len(valid) - 5:x}\r\n".encode() + valid[5:] + b"\r\n0\r\n"
        for status, body, framing, reason in (
            ("1.1 403", b"synthetic sensitive response", None, "version-auth-denied"),
            ("1.1 302", b"not followed", None, "provider-http-status"),
            ("1.1 200", b"x" * (identity.MAX_BODY + 1), None, "provider-body-limit"),
            ("1.1 200", b'{"success":true,"success":true}', None, "provider-response-shape"),
            ("1.1 200", valid, None, None),
            ("1.1 200", valid, f"Content-Length: {len(valid) + 1}", "provider-transport"),
            ("1.1 200", valid, f"Content-Length: {len(valid)}\r\nMalformed header", "provider-transport"),
            ("1.1 200", valid, f"X-Test: one\rContent-Length: {len(valid)}", "provider-transport"),
            ("1.1 200", valid, "Connection: close", "provider-transport"),
            ("1.1 200", chunk, chunked, "provider-transport"),
            ("1.1 200", chunk + b"\r\n", chunked, None),
            ("1.0 200", chunk + b"\r\n", chunked, "provider-transport"),
            ("1.1 200", chunk + b"\r\n", chunked + "\r\nContent-Length: 1", "provider-transport"),
            ("1.1 200", f"{identity.MAX_BODY + 1:x}\r\n".encode(), chunked, "provider-body-limit"),
        ):
            framing = framing or f"Content-Length: {len(body)}"
            wire = f"HTTP/{status} Test\r\n{framing}\r\n\r\n".encode() + body
            conn.sock.makefile.return_value = io.BytesIO(wire)
            conn.request.reset_mock()
            with self.subTest(status=status, framing=framing, reason=reason):
                if reason:
                    with self.assertRaisesRegex(identity.Unavailable, f"^{reason}$"):
                        identity.provider_result("/mock", "synthetic-token")
                else:
                    self.assertEqual(identity.provider_result("/mock", "synthetic-token"), {})
            connect.assert_called_with("api.cloudflare.com", timeout=5)
            conn.request.assert_called_once_with("GET", "/mock", headers={
                "Authorization": "Bearer synthetic-token", "Cache-Control": "no-cache, no-store",
            })

    def test_workflow_keeps_deploy_marker_separate_from_identity(self):
        wiring = (Path(__file__).parents[1] / "workflows/deploy.yml").read_text()
        deploy, report = wiring.split("- name: Deploy api to staging\n", 1)[1].split("- name: Record API deployment outcome\n", 1)
        hook = " ".join(dedent(deploy.split("postCommands: >-\n", 1)[1].split("\n      #", 1)[0]).splitlines())
        script = dedent(report.split("        run: |\n", 1)[1].split("\n\n", 1)[0])
        matched = json.dumps(MATCHED)
        missing = {"identity": "unavailable", "reason": "no-completed-probe-receipt"}
        denied = {"identity": "unavailable", "reason": "version-auth-denied"}
        for status, rc, payload, expected in (
            ("success", 0, matched, MATCHED),
            ("success", 1, json.dumps(denied), denied),
            ("success", 124, matched, missing),
            ("success", 0, "synthetic unparseable output", missing),
            ("success", 0, '"synthetic non-object output"', missing),
            ("success", 0, f"{matched}\n{matched}", missing),
            ("success", 0, '{"identity":"matched"}', missing),
            ("success", 0, json.dumps(MATCHED | {"deployment_id": "not-an-id"}), missing),
            ("success", 0, json.dumps(MATCHED | {"build_sha": "e" * 40}), missing),
            ("success", 0, json.dumps(MATCHED | {"lease": "present"}), missing),
            ("success", 0, json.dumps(MATCHED | {"unexpected": "synthetic private field"}), missing),
            ("success", 0, matched.replace('"version_id":', '"version_id":null,"version_id":'), missing),
            ("success", 1, json.dumps(denied | {"reason": "synthetic-private-detail"}), missing),
            ("success", 1, json.dumps(denied | {"unexpected": "synthetic private field"}), missing),
            ("success", 0, matched + " " * 512, missing),
            ("failure", 0, matched, missing),
            ("cancelled", 0, matched, missing),
        ):
            with self.subTest(status=status, rc=rc, payload=payload), TemporaryDirectory() as temp:
                output, summary, probe = (Path(temp, name) for name in ("output", "summary", "python3"))
                probe.write_text(f"#!/bin/sh\n[ \"$*\" = '-I ../../.github/scripts/staging_api_identity.py' ] || exit 2\n"
                                 f"printf '%s\\n' '{payload}'\nexit {rc}\n")
                probe.chmod(0o700)
                env = {"PATH": f"{temp}:{identity.os.defpath}", "RUNNER_TEMP": temp, "GITHUB_OUTPUT": str(output),
                       "GITHUB_STEP_SUMMARY": str(summary), "CONFIGURED": "true", "HYPERDRIVE_OUTCOME": "success",
                       "SCHEMA_OUTCOME": "success", "JOB_STATUS": status, "DEPLOY_OUTCOME": status, "SOURCE_SHA": SHA}
                hook_result = subprocess.run(["bash", "-c", hook], check=True, env=env, capture_output=True, text=True)
                self.assertEqual(hook_result.stdout, "")
                report_result = subprocess.run(["bash", "-c", script], check=True, capture_output=True, text=True,
                                               env=env | {"PATH": identity.os.defpath})
                deployed, outcome = status == "success", {"success": "deployed", "failure": "failed"}.get(status, status)
                self.assertEqual(output.read_text(),
                                 f"deployed={str(deployed).lower()}\noutcome={outcome}\nsource_sha={SHA if deployed else ''}\n")
                receipt = summary.read_text().split("\n### staging-api provider identity\n", 1)[1].strip()
                self.assertEqual(report_result.stdout.splitlines(), [f"staging-api provider identity: {receipt}"])
                self.assertEqual(json.loads(receipt), expected)


if __name__ == "__main__":
    unittest.main()
