import json
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest

from production_observation import Refused, validate_observation


def fixture():
    def utc(minute):
        return f"2026-10-01T12:{minute:02}:00Z"

    candidate = {"sha": "a" * 40, "api_sha": "b" * 40, "run_id": "123", "run_attempt": "2",
                 "promoted_version_id": "12345678-1234-1234-1234-123456789abc",
                 "deployment_id": "12345678-1234-1234-1234-123456789def"}
    smoke = {key: value for key, value in candidate.items() if key != "api_sha"}
    smoke.update(outcome="success", completed_at=utc(0), allocation_percent=100,
                 evidence="https://github.com/TogetherWeOwn/nntunev2/actions/runs/123/job/456")
    policy = {"recorded_at": "2026-10-01T11:55:00Z", "evidence": "#evidence-error-policy"}
    pins = {"schema": 1, "candidate": candidate, "final_smoke": smoke, "error_policy": policy}
    assignment = {"from": utc(0), "through": utc(35), "recovery_access": "confirmed"}
    watch = {"schema": 1, "run_id": "123", "run_attempt": "2", "error_policy_evidence": policy["evidence"],
             "watcher": {**assignment, "name": "watcher", "evidence": "#evidence-primary"},
             "backup": {**assignment, "name": "backup", "evidence": "#evidence-backup"},
             "closed_at": utc(30), "result": "satisfied", "samples": []}
    for minute in range(0, 31, 5):
        watch["samples"].append({
            "observed_at": utc(minute), "web_sha": "a" * 40, "api_sha": "b" * 40,
            "promoted_version_id": candidate["promoted_version_id"], "deployment_id": candidate["deployment_id"],
            "allocation_percent": 100, "evidence": f"#evidence-readback-{minute}",
            "readbacks": {"health": "passed", "readiness": "passed", "authorized_playback": "passed",
                          "expected_denials": "passed", "web_errors": "within-threshold",
                          "api_errors": "within-threshold", "stream_errors": "within-threshold"},
        })
    return pins, watch


class ProductionObservationTests(unittest.TestCase):
    def test_complete_healthy_window_and_later_close_with_coverage(self):
        pins, watch = fixture()
        validate_observation(pins, watch)
        watch["samples"][0]["observed_at"] = "2026-10-01T12:00:03Z"
        validate_observation(pins, watch)
        watch["closed_at"] = "2026-10-01T12:35:00+00:00"
        watch["samples"].append({**watch["samples"][-1], "observed_at": watch["closed_at"],
                                 "evidence": "#evidence-readback-35"})
        validate_observation(pins, watch)

    def test_healthy_samples_stop_short_of_declared_close(self):
        pins, watch = fixture()
        # Moving only the final readback earlier keeps the count and valid cadence.
        closing_sample = watch["samples"][-1]
        closing_sample["observed_at"] = "2026-10-01T12:29:59.999999Z"
        with self.assertRaisesRegex(Refused, "^closing readback is missing$"):
            validate_observation(pins, watch)

        closing_sample["observed_at"] = watch["closed_at"]
        validate_observation(pins, watch)

    def test_start_requires_successful_pinned_final_smoke(self):
        for field, value in (("outcome", "failure"), ("sha", "c" * 40), ("run_attempt", "1"),
                             ("evidence", "https://github.com/TogetherWeOwn/nntunev2/actions/runs/999/job/456"),
                             ("allocation_percent", 50), ("deployment_id", "missing"),
                             ("completed_at", "2026-10-01T11:59:00Z"),
                             ("completed_at", "2026-10-01T12:01:00Z"),
                             ("completed_at", "2026-10-01T12:00:00")):
            with self.subTest(field=field, value=value):
                pins, watch = fixture()
                pins["final_smoke"][field] = value
                with self.assertRaises(Refused):
                    validate_observation(pins, watch)
        pins, watch = fixture()
        del pins["final_smoke"]
        with self.assertRaises(Refused):
            validate_observation(pins, watch)

    def test_opening_readback_must_follow_smoke_before_first_cadence_deadline(self):
        pins, watch = fixture()
        watch["samples"].insert(0, {**watch["samples"][0], "observed_at": "2026-10-01T11:59:59Z"})
        with self.assertRaisesRegex(Refused, "opening readback missed its deadline"):
            validate_observation(pins, watch)
        pins, watch = fixture()
        watch["samples"].pop(0)
        with self.assertRaisesRegex(Refused, "opening readback missed its deadline"):
            validate_observation(pins, watch)

    def test_missing_or_invalid_coverage_fails_closed(self):
        cases = (
            lambda pins, watch: watch.pop("backup"),
            lambda pins, watch: watch["backup"].update(name="WATCHER"),
            lambda pins, watch: watch["backup"].update(recovery_access="unknown"),
            lambda pins, watch: watch["watcher"].update(**{"from": "2026-10-01T12:00:01Z"}),
            lambda pins, watch: watch["backup"].update(through="2026-10-01T12:29:59Z"),
            lambda pins, watch: watch.update(run_attempt="1"),
            lambda pins, watch: watch.update(error_policy_evidence="#evidence-other-policy"),
            lambda pins, watch: pins["error_policy"].update(recorded_at="2026-10-01T12:05:00Z"),
            lambda pins, watch: watch.update(samples=[]),
            lambda pins, watch: watch["samples"].pop(3),
            lambda pins, watch: watch["samples"].insert(3, watch["samples"][2]),
            lambda pins, watch: watch["samples"][3].update(observed_at="2026-10-01T12:15:00.000001Z"),
            lambda pins, watch: watch["samples"].pop(),
            lambda pins, watch: watch.update(result="failed"),
        )
        for index, change in enumerate(cases):
            with self.subTest(case=index):
                pins, watch = fixture()
                change(pins, watch)
                with self.assertRaises(Refused):
                    validate_observation(pins, watch)
        pins, watch = fixture()
        watch["closed_at"] = watch["samples"][-1]["observed_at"] = "2026-10-01T12:29:59Z"
        with self.assertRaises(Refused):
            validate_observation(pins, watch)

    def test_identity_and_every_readback_can_fail_at_start_during_and_close(self):
        for index in (0, 3, 6):
            for field, value in (("web_sha", "c" * 40), ("api_sha", "c" * 40),
                                 ("promoted_version_id", "other"), ("deployment_id", "other"),
                                 ("allocation_percent", 50), ("evidence", "")):
                with self.subTest(sample=index, field=field):
                    pins, watch = fixture()
                    watch["samples"][index][field] = value
                    with self.assertRaises(Refused):
                        validate_observation(pins, watch)
            for outcome in ("failed", "unknown", "missing", "breached"):
                _, template = fixture()
                for check in template["samples"][index]["readbacks"]:
                    with self.subTest(sample=index, check=check, outcome=outcome):
                        pins, watch = fixture()
                        readbacks = watch["samples"][index]["readbacks"]
                        if outcome == "missing":
                            del readbacks[check]
                        else:
                            readbacks[check] = outcome
                        with self.assertRaises(Refused):
                            validate_observation(pins, watch)

    def test_cli_verdicts_keep_evidence_out_of_output(self):
        pins, watch = fixture()
        script = Path(__file__).with_name("production_observation.py")
        with TemporaryDirectory(prefix="nntune-observation-") as directory:
            pin_file = Path(directory) / "pins.json"
            watch_file = Path(directory) / "watch.json"
            pin_file.write_text(json.dumps(pins))

            def check(payload, code):
                watch_file.write_text(payload)
                result = subprocess.run([sys.executable, script, pin_file, watch_file],
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, code)
                self.assertNotIn("synthetic-private-text", result.stdout + result.stderr)
                if code:
                    self.assertEqual(result.stdout, "")
                    self.assertIn("Production observation refused:", result.stderr)
                else:
                    self.assertEqual(result.stdout, "Production observation evidence satisfied\n")
                    self.assertEqual(result.stderr, "")

            check(json.dumps(watch), 0)
            watch["samples"][-1]["evidence"] = "https://example.invalid/?token=synthetic-private-text"
            check(json.dumps(watch), 1)
            check('{"schema": 1, "schema": 1}', 1)
            check("{ malformed synthetic-private-text", 1)
            check(" " * (256 * 1024 + 1), 1)

    def test_cli_refuses_explicitly_pending_readback(self):
        pins, watch = fixture()
        watch["samples"][3]["readbacks"]["stream_errors"] = "pending"
        script = Path(__file__).with_name("production_observation.py")
        with TemporaryDirectory(prefix="nntune-observation-") as directory:
            pin_file = Path(directory) / "pins.json"
            watch_file = Path(directory) / "watch.json"
            pin_file.write_text(json.dumps(pins))
            watch_file.write_text(json.dumps(watch))
            result = subprocess.run([sys.executable, "-B", script, pin_file, watch_file],
                                    capture_output=True, text=True, check=False)

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "Production observation refused: readback failed, missing or unusable\n")


if __name__ == "__main__":
    unittest.main()
