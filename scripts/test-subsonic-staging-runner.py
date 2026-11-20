#!/usr/bin/env python3
import runpy
import tempfile
import unittest
from pathlib import Path

runner = runpy.run_path(str(Path(__file__).with_name("subsonic-staging-runner.py")))
StagingModel, matrix, rehearse = (runner[key] for key in ("StagingModel", "MATRIX", "rehearse"))


class RecordingModel(StagingModel):
    delays = {}

    def __init__(self):
        super().__init__()
        self.starts = []

    def start(self, slot):
        number = slot["slot"]
        self.starts.append((number, self.now))
        super().start(slot)
        if number in self.delays:
            self.pending[number] = self.now + self.delays[number]


class RunnerTests(unittest.TestCase):
    def test_reviewed_schedule(self):
        model = RecordingModel()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["cleanup"]), ("complete", "simulated-off-and-drained"))
        self.assertEqual([number for number, _ in model.starts], list(range(1, 49)))
        self.assertTrue(all(b - a >= 1000 for (_, a), (_, b) in zip(model.starts, model.starts[1:])))
        self.assertEqual([row["state"] for row in report["slots"].values()], ["accepted"] * 48)
        self.assertEqual(report["simulated_pair_overlap"],
                         {1: False, 2: False, 3: True, 4: True, 5: True, 6: True})
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
        for starts, observation, reason in ((0, (-1, True, 0), "telemetry-stale-or-unsafe"),
                                            (1, (50, True, 3), "backend-pressure"),
                                            (1, (50, False, 1), "telemetry-stale-or-unsafe")):
            class Unsafe(RecordingModel):
                def telemetry(self):
                    return observation if self.lease and len(self.starts) >= starts else super().telemetry()

            report = rehearse(matrix, Unsafe())
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["max_simulated_backends"], 0 if starts else None)
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_pair_busy_requires_attribution_and_is_bounded(self):
        for fmt, size, attributed, delay, stop in (
            ("json", 112, False, 100, "unattributed-pair-result"),
            ("xml", 120, False, 250, "unattributed-pair-result"),
            ("xml", 120, True, 250, "complete"), ("xml", 120, True, 300, "busy-latency"),
        ):
            class Busy(RecordingModel):
                delays = {38: delay}

                def result(self, slot):
                    result = super().result(slot)
                    return {"oracle": f"fixed_capacity_{fmt}", "http_status": 503,
                            "body_bytes": size} if result and slot["slot"] == 38 else result

                def busy_observed(self, slot):
                    return attributed and slot["slot"] == 38

            report = rehearse(matrix, Busy())
            accepted = stop == "complete"
            self.assertEqual((report["stop"], report["starts"]), (stop, 48 if accepted else 38))
            self.assertEqual(report["slots"][38]["state"], "accepted" if accepted else "rejected")
            self.assertEqual(report["slots"][39]["state"], "accepted" if accepted else "unstarted")

    def test_reader_unfinished_at_five_seconds_stops_and_cleans_up(self):
        model = RecordingModel()
        model.delays = {41: 6000, 42: 6000}
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("request-or-reader-timeout", 42))
        self.assertEqual(model.now - dict(model.starts)[41], 5000)
        self.assertEqual(report["simulated_pair_overlap"], {1: False, 2: False, 3: True})
        self.assertEqual([report["slots"][i]["state"] for i in (41, 42, 43)],
                         ["unfinished", "unfinished", "unstarted"])
        self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_partial_arm_and_cancellation_restore(self):
        class PartialArm(RecordingModel):
            def arm(self):
                super().arm()
                raise RuntimeError("not part of the report")

        class Cancelled(RecordingModel):
            def tick(self):
                raise KeyboardInterrupt

        class Superseded(Cancelled):
            def tick(self):
                self.version, self.lease = "new-default-off", False
                super().tick()

        for model, reason, starts, version in (
            (PartialArm(), "model-failure", 0, "baseline"), (Cancelled(), "cancelled", 1, "baseline"),
            (Superseded(), "cancelled", 1, "new-default-off"),
        ):
            report = rehearse(matrix, model)
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")
            self.assertEqual(model.readback(), (version, False))

    def test_client_cancellation_does_not_prove_backend_drain(self):
        class Lingering(RecordingModel):
            def telemetry(self):
                return self.now, True, int(bool(self.starts))

        model = Lingering()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("clock-or-window", 1))
        self.assertEqual(report["cleanup"], "unverified")
        self.assertEqual(model.readback(), ("baseline", False))


if __name__ == "__main__":
    unittest.main()
