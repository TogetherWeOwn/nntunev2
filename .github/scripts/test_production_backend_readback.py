"""Exercise release input, transport and reporting together without networking."""

import io
import json
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import Mock, patch

import production_backend_readback as caller
from production_backend_identity import WorkerPin
from test_staging_api_identity import ACCOUNT, DEPLOYMENT, OTHER, SHA, VERSION, current, version

API = WorkerPin("nntune-api", SHA, VERSION, DEPLOYMENT)
STREAM = WorkerPin("nntune-stream", SHA, OTHER, "44444444-4444-4444-8444-444444444444")
ROOT = f"/client/v4/accounts/{ACCOUNT}/workers/scripts"
API_DEPLOYMENTS = f"{ROOT}/nntune-api/deployments"
STREAM_DEPLOYMENTS = f"{ROOT}/nntune-stream/deployments"
API_VERSION = f"{ROOT}/nntune-api/versions/{VERSION}"
STREAM_VERSION = f"{ROOT}/nntune-stream/versions/{OTHER}"


def provider_results():
    binding = {"name": "fixture-only", "type": "plain_text", "text": "private fixture value"}
    return {
        API_DEPLOYMENTS: current(),
        STREAM_DEPLOYMENTS: current(STREAM.deployment_id, STREAM.version_id),
        API_VERSION: version(binding),
        STREAM_VERSION: dict(version(binding), id=STREAM.version_id),
    }


class ReleaseReadbackTests(unittest.TestCase):
    def report(self, read, expected_status, expected_result, *, candidate_sha=SHA, api=API):
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            status = caller.report(candidate_sha=candidate_sha, account=ACCOUNT, api=api, stream=STREAM, read=read)
        self.assertEqual(status, expected_status)
        self.assertEqual(json.loads(stdout.getvalue()), expected_result)
        self.assertEqual(stderr.getvalue(), "")

    def test_matches_both_published_allocations_and_builds(self):
        results = provider_results()
        reader = Mock(side_effect=lambda path, *, deadline: results[path])
        with patch.object(caller, "monotonic", return_value=20):
            self.report(reader, 0, {"identity": "matched"})
        self.assertEqual([call.args[0] for call in reader.call_args_list], [
            API_DEPLOYMENTS, STREAM_DEPLOYMENTS, API_VERSION, STREAM_VERSION, STREAM_DEPLOYMENTS, API_DEPLOYMENTS,
        ])
        self.assertEqual([call.kwargs for call in reader.call_args_list], [{"deadline": 35}] * 6)

    def test_refuses_when_stream_drifts_during_readback(self):
        results = provider_results()
        reader = Mock(side_effect=[results[API_DEPLOYMENTS], results[STREAM_DEPLOYMENTS],
                                   results[API_VERSION], results[STREAM_VERSION],
                                   current(DEPLOYMENT, STREAM.version_id)])
        self.report(reader, 1, {"identity": "unavailable", "backend": "stream", "reason": "deployment-unverified"})

    def test_refuses_incomplete_read_without_leaking_diagnostics(self):
        results = provider_results()
        reader = Mock(side_effect=[results[API_DEPLOYMENTS], results[STREAM_DEPLOYMENTS],
                                   results[API_VERSION], OSError("private fixture diagnostic")])
        self.report(reader, 1, {"identity": "unavailable", "backend": "stream", "reason": "build-unverified"})

    def test_refuses_paired_pins_for_a_different_candidate_before_reading(self):
        reader = Mock()
        self.report(reader, 1, {"identity": "unavailable", "reason": "invalid-pins"}, candidate_sha="c" * 40)
        reader.assert_not_called()

    def test_refuses_malformed_pin_before_comparing_candidate(self):
        class BadSHA:
            def __eq__(self, other):
                raise AssertionError("malformed SHA reached equality")

        reader = Mock()
        self.report(reader, 1, {"identity": "unavailable", "reason": "invalid-pins"},
                    api=API._replace(build_sha=BadSHA()))
        reader.assert_not_called()

    def test_refuses_result_at_deadline_and_stops_further_reads(self):
        for late_read in (1, 6):
            with self.subTest(late_read=late_read), patch.object(caller, "monotonic", return_value=20) as clock:
                results = provider_results()

                def respond(path, *, deadline):
                    if reader.call_count == late_read:
                        clock.return_value = deadline
                    return results[path]

                reader = Mock(side_effect=respond)
                self.report(reader, 1, {"identity": "unavailable", "reason": "deadline-expired"})
                self.assertEqual(reader.call_count, late_read)
