"""Offline tests; the provider boundary is always mocked."""

import importlib.util
import io
import json
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location("identity", Path(__file__).with_name("staging_api_identity.py"))
identity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(identity)

ACCOUNT = "a" * 32
SHA = "b" * 40
DEPLOYMENT = "11111111-1111-4111-8111-111111111111"
VERSION = "22222222-2222-4222-8222-222222222222"
OTHER = "33333333-3333-4333-8333-333333333333"
PREFIX = "SUBSONIC_MEASUREMENT_"


def output(**changes):
    deploy = {
        "type": "deploy",
        "version": 1,
        "worker_name": "nntune-api-staging",
        "wrangler_environment": "staging",
        "version_id": VERSION,
    }
    deploy.update(changes)
    return json.dumps(deploy).encode() + b"\n"


def current(deployment_id=DEPLOYMENT, version_id=VERSION, percentage=100):
    versions = [{"version_id": version_id, "percentage": percentage}]
    return {"deployments": [{"id": deployment_id, "strategy": "percentage", "versions": versions}]}


def version(*bindings, sha=SHA):
    build = {"name": "BUILD_SHA", "type": "plain_text", "text": sha}
    return {"id": VERSION, "resources": {"bindings": [build, *bindings]}}


class IdentityTests(unittest.TestCase):
    def test_current_version_build_and_default_off(self):
        deployment = identity.decode(json.dumps(current()).replace("100}", "100.0}").encode(), "bad fixture")
        deployment["deployments"] += current(OTHER, OTHER)["deployments"]
        read = MagicMock(side_effect=[deployment, version(), deployment])
        stream = io.StringIO()
        with TemporaryDirectory() as temp, patch.object(identity, "provider_result", read):
            Path(temp, "wrangler-output-test.json").write_bytes(b'{"type":"wrangler-session","version":1}\n' + output())
            env = {
                "WRANGLER_OUTPUT_FILE_DIRECTORY": temp,
                "GITHUB_SHA": SHA,
                "CLOUDFLARE_ACCOUNT_ID": ACCOUNT,
                "CLOUDFLARE_API_TOKEN": "synthetic-token",
            }
            with patch.dict(identity.os.environ, env, clear=True), redirect_stdout(stream):
                self.assertEqual(identity.main(), 0)
        expected = {
            "identity": "matched",
            "deployment_id": DEPLOYMENT,
            "version_id": VERSION,
            "build_sha": SHA,
            "enable_flag": "off",
            "lease": "absent",
            "measurement_db_binding": "absent",
        }
        self.assertEqual(json.loads(stream.getvalue()), expected)
        path = f"/client/v4/accounts/{ACCOUNT}/workers/scripts/nntune-api-staging"
        self.assertEqual(
            [c.args[0] for c in read.call_args_list],
            [path + "/deployments", path + "/versions/" + VERSION, path + "/deployments"],
        )

    def test_bad_deploy_output_never_reads_provider(self):
        for body in (
            b"",
            b"{" + b"x" * identity.MAX_DEPLOY_OUTPUT,
            output()[:-1],
            output() + output(),
            output(version=True),
            output(version_id=None),
            output(worker_name="wrong"),
            output(wrangler_environment="production"),
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
            with (
                self.subTest(name=name),
                self.assertRaisesRegex(identity.Unavailable, "default-off-not-verified"),
            ):
                identity.verify_version(version(dict(binding, name=PREFIX + name)), VERSION, SHA)

    @patch.object(identity.http.client, "HTTPSConnection")
    def test_provider_bounds_and_no_redirect_or_raw_response(self, connect):
        conn = connect.return_value
        valid = b'{"success":true,"errors":[],"result":{}}'
        chunked = "Transfer-Encoding: chunked"
        chunk = b"".join(f"{len(part):x}\r\n".encode() + part + b"\r\n" for part in (valid[:5], valid[5:]))
        chunk += b"0\r\n"
        for status, body, framing, reason in (
            (403, b"synthetic sensitive response", None, "version-auth-denied"),
            (302, b"not followed", None, "provider-http-status"),
            (200, b"x" * (identity.MAX_BODY + 1), None, "provider-body-limit"),
            (200, b'{"success":true,"success":true}', None, "provider-response-shape"),
            (200, valid, None, None),
            (200, valid, f"Content-Length: {len(valid) + 1}", "provider-transport"),
            (200, valid, "Connection: close", "provider-transport"),
            (200, chunk, chunked, "provider-transport"),
            (200, chunk + b"\r\n", chunked, None),
            (200, chunk + b"\r\n", chunked + "\r\nContent-Length: 1", "provider-transport"),
            (200, f"{identity.MAX_BODY + 1:x}\r\n".encode(), chunked, "provider-body-limit"),
        ):
            framing = framing or f"Content-Length: {len(body)}"
            wire = f"HTTP/1.1 {status} Test\r\n{framing}\r\n\r\n".encode() + body
            response = identity.http.client.HTTPResponse(MagicMock(makefile=MagicMock(return_value=io.BytesIO(wire))))
            response.begin()
            conn.getresponse.return_value = response
            conn.request.reset_mock()
            with self.subTest(reason=reason):
                if reason:
                    with self.assertRaisesRegex(identity.Unavailable, f"^{reason}$"):
                        identity.provider_result("/mock", "synthetic-token")
                else:
                    self.assertEqual(identity.provider_result("/mock", "synthetic-token"), {})
            connect.assert_called_with("api.cloudflare.com", timeout=5)
            conn.request.assert_called_once_with(
                "GET",
                "/mock",
                headers={"Authorization": "Bearer synthetic-token", "Cache-Control": "no-cache, no-store"},
            )

    def test_cli_unavailable_is_explicit_and_nonzero(self):
        stream = io.StringIO()
        with patch.dict(identity.os.environ, {}, clear=True), redirect_stdout(stream):
            self.assertEqual(identity.main(), 1)
        self.assertEqual(json.loads(stream.getvalue()), {"identity": "unavailable", "reason": "credential-config"})


if __name__ == "__main__":
    unittest.main()
