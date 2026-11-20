#!/usr/bin/env python3
import runpy
import tempfile
import unittest
from pathlib import Path

runner = runpy.run_path(str(Path(__file__).with_name("subsonic-staging-runner.py")))
StagingModel, matrix, rehearse = (runner[key] for key in ("StagingModel", "MATRIX", "rehearse"))


class RecordingModel(StagingModel):
    def __init__(self):
        super().__init__()
        self.starts = []

    def start(self, slot):
        self.starts.append((slot["slot"], self.now))
        super().start(slot)


class RunnerTests(unittest.TestCase):
    def test_reviewed_schedule(self):
        model = RecordingModel()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["cleanup"]), ("complete", "simulated-off-and-drained"))
        self.assertEqual([number for number, _ in model.starts], list(range(1, 49)))
        self.assertTrue(all(b - a >= 1000 for (_, a), (_, b) in zip(model.starts, model.starts[1:])))
        self.assertEqual(report["starts"], len(report["samples"]))
        self.assertEqual(report["simulated_overlap_starts"], 4)
        self.assertEqual(report["max_simulated_clients"], 2)
        self.assertEqual(report["max_simulated_backends"], 2)
        self.assertLess(model.now, 120000)

    def test_modified_matrix_never_arms(self):
        for raw in (matrix.read_text().replace('"in_flight": 2', '"in_flight": 3'),
                    matrix.read_text().replace('"in_flight": 2', '"in_flight": 99, "in_flight": 2')):
            with tempfile.TemporaryDirectory() as tmp:
                tampered = Path(tmp) / "matrix.json"
                tampered.write_text(raw)
                model = RecordingModel()
                report = rehearse(tampered, model)
                self.assertEqual((report["stop"], report["cleanup"]), ("unreviewed-matrix", "not-armed"))
                self.assertEqual(model.readback(), ("baseline", False))
                self.assertEqual(model.starts, [])

    def test_telemetry_loss_and_pressure_stop_before_next_start(self):
        for observation, reason in (((-1, True, 1), "telemetry-stale-or-unsafe"),
                                    ((50, True, 3), "backend-pressure"),
                                    ((50, False, 1), "telemetry-stale-or-unsafe")):
            class Unsafe(RecordingModel):
                def telemetry(self):
                    return observation if self.lease and self.starts else super().telemetry()

            model = Unsafe()
            report = rehearse(matrix, model)
            self.assertEqual(model.starts, [(1, 0)])
            self.assertEqual(report["stop"], reason)
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_unattributed_503_stops_without_retry(self):
        class Unattributed(RecordingModel):
            def result(self, slot):
                result = super().result(slot)
                if result and slot["slot"] == 38:
                    return {"oracle": "fixed_capacity_json", "http_status": 503, "body_bytes": 112}
                return result

        report = rehearse(matrix, Unattributed())
        self.assertEqual((report["stop"], report["starts"]), ("unattributed-pair-result", 38))

    def test_partial_arm_and_cancellation_restore(self):
        class PartialArm(RecordingModel):
            def arm(self):
                super().arm()
                raise RuntimeError("not part of the report")

        class Cancelled(RecordingModel):
            def tick(self):
                raise KeyboardInterrupt

        for model, reason, starts in ((PartialArm(), "model-failure", 0), (Cancelled(), "cancelled", 1)):
            report = rehearse(matrix, model)
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_client_cancellation_does_not_prove_backend_drain(self):
        class Lingering(RecordingModel):
            def telemetry(self):
                return self.now, True, int(bool(self.starts))

        model = Lingering()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("clock-or-window", 1))
        self.assertEqual(report["cleanup"], "unverified")
        self.assertEqual(model.readback(), ("baseline", False))

    def test_restore_does_not_overwrite_a_newer_version(self):
        class Superseded(RecordingModel):
            def tick(self):
                self.version, self.lease = "new-default-off", False
                raise KeyboardInterrupt

        model = Superseded()
        report = rehearse(matrix, model)
        self.assertEqual(report["cleanup"], "simulated-off-and-drained")
        self.assertEqual(model.readback(), ("new-default-off", False))


if __name__ == "__main__":
    unittest.main()
