"""Rehearse refused release-watch closeout with synthetic observations only."""

from contextlib import redirect_stdout
from io import StringIO
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

from production_observation import Refused, validate_observation
from test_production_observation import fixture

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / ".github" / "scripts"))
import production_backend_readback as backend_readback
from test_production_backend_readback import ACCOUNT, API, STREAM, STREAM_VERSION, provider_results


def backend_report(pins, *, incomplete=False):
    results = provider_results()
    if incomplete:
        del results[STREAM_VERSION]

    def read(path, *, deadline):
        if path not in results:
            raise OSError("synthetic response interrupted")
        return results[path]

    output = StringIO()
    with redirect_stdout(output), patch.object(backend_readback, "monotonic", return_value=20):
        status = backend_readback.report(candidate_sha=pins["candidate"]["api_sha"], account=ACCOUNT,
                                         api=API, stream=STREAM, read=read)
    return status, json.loads(output.getvalue())


class ProductionObservationRecoveryTests(unittest.TestCase):
    def test_concurrent_health_conflict_refuses_close_in_either_order(self):
        pins, watch = fixture()
        self.assertEqual(backend_report(pins), (0, {"identity": "matched"}))
        validate_observation(pins, watch)

        healthy = watch["samples"][-1]
        failed = {**healthy, "evidence": "#evidence-failed-health",
                  "readbacks": {**healthy["readbacks"], "health": "failed"}}
        for first, second, reason in (
            (healthy, failed, "invalid observation sample cadence"),
            (failed, healthy, "readback failed, missing or unusable"),
        ):
            with self.subTest(first=first["readbacks"]["health"]):
                conflicting_watch = {**watch, "samples": [*watch["samples"][:-1], first, second]}
                with self.assertRaisesRegex(Refused, reason):
                    validate_observation(pins, conflicting_watch)

    def test_incomplete_health_read_refuses_close_with_matching_identity(self):
        pins, watch = fixture()
        self.assertEqual(backend_report(pins), (0, {"identity": "matched"}))
        validate_observation(pins, watch)

        del watch["samples"][-1]["readbacks"]["health"]
        with self.assertRaisesRegex(Refused, "readback failed, missing or unusable"):
            validate_observation(pins, watch)

    def test_incomplete_backend_read_refuses_close_with_healthy_probes(self):
        pins, watch = fixture()
        self.assertEqual(backend_report(pins), (0, {"identity": "matched"}))
        validate_observation(pins, watch)

        self.assertEqual(backend_report(pins, incomplete=True), (1, {
            "identity": "unavailable", "backend": "stream", "reason": "build-unverified",
        }))
        # An unavailable paired read supplies no confirmed API build for close.
        del watch["samples"][-1]["api_sha"]
        with self.assertRaisesRegex(Refused, "incomplete observation sample"):
            validate_observation(pins, watch)
