"""Synthetic production cutover readback through the staging identity helpers."""

import io
import json
import unittest
from functools import partial
from unittest.mock import MagicMock, call, patch

import staging_api_identity as identity
from production_backend_identity import WorkerPin, readback
from test_staging_api_identity import ACCOUNT, DEPLOYMENT, OTHER, SHA, VERSION, current, version

API = WorkerPin("nntune-api", SHA, VERSION, DEPLOYMENT)
STREAM = WorkerPin("nntune-stream", SHA, OTHER, "44444444-4444-4444-8444-444444444444")
ROOT = f"/client/v4/accounts/{ACCOUNT}/workers/scripts"
API_DEPLOYMENTS = f"{ROOT}/nntune-api/deployments"
STREAM_DEPLOYMENTS = f"{ROOT}/nntune-stream/deployments"
PATHS = [
    API_DEPLOYMENTS,
    STREAM_DEPLOYMENTS,
    f"{ROOT}/nntune-api/versions/{API.version_id}",
    f"{ROOT}/nntune-stream/versions/{STREAM.version_id}",
    STREAM_DEPLOYMENTS,
    API_DEPLOYMENTS,
]


def matched_responses():
    api_deployment = current()
    stream_deployment = current(STREAM.deployment_id, STREAM.version_id)
    private_binding = {"name": "fixture-only-binding", "type": "plain_text", "text": "synthetic-value"}
    return [
        api_deployment,
        stream_deployment,
        version(private_binding),
        dict(version(private_binding), id=STREAM.version_id),
        stream_deployment,
        api_deployment,
    ]


class BackendReadbackTests(unittest.TestCase):
    @patch.object(identity.http.client, "HTTPSConnection")
    def test_exact_cutover_uses_only_pinned_gets_and_returns_safe_evidence(self, connect):
        def wire(result):
            body = json.dumps({"success": True, "errors": [], "result": result}).encode()
            return io.BytesIO(f"HTTP/1.1 200 OK\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body)

        conn = connect.return_value
        conn.sock.makefile.side_effect = [wire(result) for result in matched_responses()]
        result = readback(account=ACCOUNT, api=API, stream=STREAM,
                          read=partial(identity.provider_result, token="synthetic-read-token"))
        self.assertEqual(result, {"identity": "matched"})
        self.assertEqual(connect.call_args_list, [call("api.cloudflare.com", timeout=5)] * len(PATHS))
        self.assertEqual(conn.request.call_args_list, [call("GET", path, headers={
            "Authorization": "Bearer synthetic-read-token", "Cache-Control": "no-cache, no-store",
        }) for path in PATHS])

    def test_bad_or_mixed_pins_never_dispatch(self):
        reader = MagicMock()
        for account, api, stream in (
            ("../other-account", API, STREAM),
            (ACCOUNT, None, STREAM),
            (ACCOUNT, API, None),
            (ACCOUNT, STREAM, API),
            (ACCOUNT, API._replace(worker="nntune-api-staging"), STREAM),
            (ACCOUNT, API, STREAM._replace(worker="nntune-stream?unexpected")),
            (ACCOUNT, API._replace(build_sha="unknown"), STREAM),
            (ACCOUNT, API._replace(build_sha="c" * 40), STREAM),
            (ACCOUNT, API, STREAM._replace(version_id=None)),
            (ACCOUNT, API._replace(deployment_id=""), STREAM),
        ):
            with self.subTest(account=account, api=api, stream=stream):
                self.assertEqual(readback(account=account, api=api, stream=stream, read=reader),
                                 {"identity": "unavailable", "reason": "invalid-pins"})
        reader.assert_not_called()

    def test_absent_mixed_mismatched_or_changed_identity_is_unavailable(self):
        mixed = current(percentage=50)
        mixed["deployments"][0]["versions"] += [{"version_id": OTHER, "percentage": 50}]
        prior_match = current(deployment_id=OTHER)
        prior_match["deployments"] += current()["deployments"]
        for step, response, backend, reason in (
            (0, {"deployments": []}, "api", "deployment-unverified"),
            (0, mixed, "api", "deployment-unverified"),
            (0, prior_match, "api", "deployment-unverified"),
            (1, current(STREAM.deployment_id, API.version_id), "stream", "deployment-unverified"),
            (1, current(STREAM.deployment_id, STREAM.version_id, 99), "stream", "deployment-unverified"),
            (2, {"id": VERSION}, "api", "build-unverified"),
            (2, version(sha="c" * 40), "api", "build-unverified"),
            (3, version(), "stream", "build-unverified"),
            (3, RuntimeError("synthetic private diagnostic"), "stream", "build-unverified"),
            (4, current(DEPLOYMENT, STREAM.version_id), "stream", "deployment-unverified"),
            (5, current(version_id=OTHER), "api", "deployment-unverified"),
        ):
            with self.subTest(step=step, response=response):
                responses = matched_responses()
                responses[step] = response
                reader = MagicMock(side_effect=responses)
                self.assertEqual(readback(account=ACCOUNT, api=API, stream=STREAM, read=reader),
                                 {"identity": "unavailable", "backend": backend, "reason": reason})
                self.assertEqual(reader.call_args_list, [call(path) for path in PATHS[:step + 1]])
