"""Offline staging cleanup contract tests: no network, credentials or subprocesses."""

import io
import json
import subprocess
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from staging_api_identity import matched_receipt
from subsonic_staging_cleanup import CleanupResult as Result
from subsonic_staging_cleanup import DeploymentPin, verify_cleanup
from subsonic_staging_observer import SQL, PressureObservation, sample_pressure

PIN = DeploymentPin("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "c" * 40)
OTHER = "dddddddd-dddd-dddd-dddd-dddddddddddd"
MARKER = "private-synthetic-body"
ORDER = ["before", "version", "fence", "sample", "after"]


def deployment(deployment_id=PIN.deployment_id, version_id=PIN.version_id, percentage=100):
    return {
        "deployments": [
            {
                "id": deployment_id,
                "strategy": "percentage",
                "versions": [
                    {"version_id": version_id, "percentage": percentage},
                ],
            }
        ],
        "ignored": MARKER,
    }


def version(*bindings, sha=PIN.build_sha):
    return {
        "id": PIN.version_id,
        "resources": {
            "bindings": [
                {"name": "BUILD_SHA", "type": "plain_text", "text": sha},
                *bindings,
            ]
        },
    }


class Reads:
    def __init__(self, *, slow_step=None, **overrides):
        # The fence completes at 1300, query starts at 1400 and returns at 1500.
        self.now_ms = 1000
        self.slow_step = slow_step
        self.calls = []
        self.deadlines = []
        self.deploy_reads = iter(("before", "fence", "after"))
        self.responses = {
            "before": deployment(),
            "version": version(),
            "fence": deployment(),
            "sample": PressureObservation(1400, 0, -1),
            "after": deployment(),
        }
        self.responses.update(overrides)

    def clock(self):
        return self.now_ms / 1000

    def take(self, step, deadline_ms):
        self.calls.append(step)
        self.deadlines.append(deadline_ms)
        self.now_ms += 200 if step == "sample" else 100
        if step == self.slow_step:
            self.now_ms = 4400  # Exactly three seconds after the query start.
        value = self.responses[step]
        if isinstance(value, Exception):
            raise value
        return value

    def deployments(self, *, deadline_ms):
        return self.take(next(self.deploy_reads), deadline_ms)

    def version(self, version_id, *, deadline_ms):
        if version_id != PIN.version_id:
            raise AssertionError("wrong version requested")
        return self.take("version", deadline_ms)

    def sample(self, *, deadline_ms):
        return self.take("sample", deadline_ms)

    def verify(self, pin=PIN, deadline_ms=5000, **overrides):
        kwargs = {
            "read_deployments": self.deployments,
            "read_version": self.version,
            "sample_pressure": self.sample,
            "monotonic": self.clock,
        }
        return verify_cleanup(pin, deadline_ms=deadline_ms, **(kwargs | overrides))


class CleanupTest(unittest.TestCase):
    def test_verified_with_fresh_reads_and_actual_observer_contract(self):
        for bindings in ((), ({"name": "SUBSONIC_MEASUREMENT_ENABLE", "type": "plain_text", "text": "0"},)):
            with self.subTest(bindings=bindings):
                reads = Reads(version=version(*bindings))
                self.assertIs(reads.verify(), Result.VERIFIED)
                self.assertEqual(reads.calls, ORDER)
                self.assertEqual(reads.deadlines, [5000] * len(ORDER))

        reads = Reads()
        reads.now_ms += 0.412  # Off readback ends at 1300.412 ms.
        queries = []

        def run(argv, *, input, **kwargs):
            queries.append((argv[0], input, kwargs["timeout"]))
            reads.now_ms += 100
            return subprocess.CompletedProcess([], 0, stdout=b"100\n0|5|1|5|1\n")

        def sample(*, deadline_ms):
            self.assertEqual(reads.calls, ORDER[:3])
            reads.now_ms += 0.035  # Start strictly later in the same millisecond.
            return sample_pressure(
                {
                    "PLANETSCALE_OBSERVER_URL_STAGING": "postgresql://nntune_staging_observer.fake:synthetic@database.invalid/postgres"
                },
                deadline_ms=deadline_ms,
                run=run,
                wall=lambda: 100,
                monotonic=reads.clock,
            )

        self.assertIs(reads.verify(sample_pressure=sample), Result.VERIFIED)
        self.assertEqual(len(queries), 1)
        self.assertEqual(queries[0][:2], ("psql", SQL.encode("ascii")))
        self.assertLessEqual(queries[0][2], 3)
        self.assertEqual(reads.calls, ["before", "version", "fence", "after"])

    def test_rejects_deployment_replacement_even_with_same_version(self):
        split = deployment(percentage=99)
        split["deployments"][0]["versions"].append({"version_id": OTHER, "percentage": 1})
        for step in ("before", "fence", "after"):
            for wrong in (deployment(deployment_id=OTHER), deployment(version_id=OTHER), split):
                with self.subTest(step=step, wrong=wrong):
                    reads = Reads(**{step: wrong})
                    self.assertIs(reads.verify(), Result.DEPLOYMENT_UNVERIFIED)
                    self.assertEqual(reads.calls, ORDER[: ORDER.index(step) + 1])

    def test_rejects_unproven_off_state(self):
        for wrong in (
            version({"name": "SUBSONIC_MEASUREMENT_LEASE", "type": "secret_text"}),
            version({"name": "SUBSONIC_MEASUREMENT_DB", "type": "hyperdrive"}),
            version({"name": "SUBSONIC_MEASUREMENT_ENABLE", "type": "plain_text", "text": "1"}),
            version({"name": "SUBSONIC_MEASUREMENT_ENABLE", "type": "secret_text"}),
            version(sha="f" * 40),
            {"id": PIN.version_id, "resources": {}},
        ):
            with self.subTest(wrong=wrong):
                reads = Reads(version=wrong)
                self.assertIs(reads.verify(), Result.OFF_UNVERIFIED)
                self.assertEqual(reads.calls, ORDER[:2])

    def test_rejects_nonzero_unknown_and_out_of_bracket_samples(self):
        for sample, expected in (
            (PressureObservation(1400, 1, 1), Result.MEASUREMENT_SESSIONS_PRESENT),
            (PressureObservation(1299, 0, 1), Result.DRAIN_UNVERIFIED),
            (PressureObservation(1300, 0, 1), Result.DRAIN_UNVERIFIED),
            (PressureObservation(1501, 0, 1), Result.DRAIN_UNVERIFIED),
            (PressureObservation(1400, False, 1), Result.DRAIN_UNVERIFIED),
            ((1400, 0, 1), Result.DRAIN_UNVERIFIED),
        ):
            with self.subTest(sample=sample):
                reads = Reads(sample=sample)
                self.assertIs(reads.verify(), expected)
                self.assertEqual(reads.calls, ORDER[:4])

    def test_sample_must_still_be_fresh_at_return_and_after_deployment_recheck(self):
        for late, expected_calls in (("sample", ORDER[:4]), ("after", ORDER)):
            with self.subTest(late=late):
                reads = Reads(slow_step=late)
                self.assertIs(reads.verify(), Result.DRAIN_UNVERIFIED)
                self.assertEqual(reads.calls, expected_calls)

    def test_deadline_prevents_extra_reads_and_rejects_late_evidence(self):
        for deadline, reads_made in (
            (True, 0),
            (1000, 0),
            (1100, 1),
            (1200, 2),
            (1300, 3),
            (1499, 4),
            (1500, 4),
            (1600, 5),
        ):
            with self.subTest(deadline=deadline):
                reads = Reads()
                self.assertIs(reads.verify(deadline_ms=deadline), Result.DEADLINE)
                self.assertEqual(reads.calls, ORDER[:reads_made])

        for invalid in (0.5, float("nan"), float("inf"), None):
            with self.subTest(invalid=invalid):
                reads = Reads()
                clock = iter((1, invalid)).__next__
                self.assertIs(reads.verify(monotonic=clock), Result.DEADLINE)
                self.assertEqual(reads.calls, ORDER[:1])

    def test_missing_cached_and_exception_evidence_is_redacted(self):
        expected = (
            Result.DEPLOYMENT_UNVERIFIED,
            Result.OFF_UNVERIFIED,
            Result.DEPLOYMENT_UNVERIFIED,
            Result.DRAIN_UNVERIFIED,
            Result.DEPLOYMENT_UNVERIFIED,
        )
        receipt = matched_receipt(*PIN)
        output = io.StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            for step, category in zip(ORDER, expected):
                for payload in (None, receipt, RuntimeError(MARKER)):
                    with self.subTest(step=step, payload=type(payload)):
                        reads = Reads(**{step: payload})
                        actual = reads.verify()
                        self.assertIs(actual, category)
                        self.assertEqual(reads.calls, ORDER[: ORDER.index(step) + 1])
                        self.assertEqual(json.dumps(actual), json.dumps(category.value))
        self.assertEqual(output.getvalue(), "")

    def test_invalid_pin_stops_without_reads(self):
        for pin in (
            None,
            tuple(PIN),
            PIN._replace(deployment_id=""),
            PIN._replace(version_id="bad"),
            PIN._replace(build_sha=MARKER),
        ):
            with self.subTest(pin=pin):
                reads = Reads()
                self.assertIs(reads.verify(pin), Result.INVALID_PIN)
                self.assertFalse(reads.calls)


if __name__ == "__main__":
    unittest.main()
