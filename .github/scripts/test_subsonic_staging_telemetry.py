"""Deterministic telemetry admission tests; no database, HTTP or provider access."""

import runpy
import unittest
from pathlib import Path

policy = runpy.run_path(str(Path(__file__).with_name("subsonic_staging_telemetry.py")))
Gate = policy["TelemetryGate"]
Stop = policy["TelemetryStop"]
Pressure = policy["Pressure"]
Health = policy["Health"]
Coverage = policy["ResourceCoverage"]
OPEN = 1000
NOW = 5000
VERSION = "experiment"
DB = Pressure(NOW, 0, 2)
HEALTH = Health(NOW, VERSION, True)
COVERED = Coverage(VERSION, OPEN, NOW, 0)


class TelemetryTests(unittest.TestCase):
    def test_missing_or_stale_live_signals_latch_first_stop(self):
        for expected, db, health, resource in (
            ("db-telemetry-missing-or-stale", None, HEALTH, COVERED),
            ("db-telemetry-missing-or-stale", Pressure(1999, 0, 2), HEALTH, COVERED),
            ("db-telemetry-missing-or-stale", Pressure(NOW + 1, 0, 2), HEALTH, COVERED),
            ("health-telemetry-missing-or-stale", DB, None, COVERED),
            (
                "health-telemetry-missing-or-stale",
                DB,
                Health(1999, VERSION, True),
                COVERED,
            ),
            ("resource-coverage-missing", DB, HEALTH, None),
            ("resource-coverage-missing", DB, HEALTH, Coverage(VERSION, OPEN, 1999, 0)),
        ):
            with self.subTest(expected=expected):
                gate = Gate(OPEN, VERSION)
                with self.assertRaisesRegex(Stop, f"^{expected}$"):
                    gate.observe(NOW, db, health, resource)
                with self.assertRaisesRegex(Stop, f"^{expected}$"):
                    gate.observe(NOW, DB, HEALTH, COVERED)

    def test_pressure_and_version_fail_closed(self):
        for db in (Pressure(NOW, 3, 2), Pressure(NOW, 0, 1), Pressure(NOW, True, 2)):
            with self.subTest(db=db), self.assertRaisesRegex(Stop, "^db-pressure$"):
                Gate(OPEN, VERSION).observe(NOW, db, HEALTH, COVERED)
        for health in (Health(NOW, "different", True), Health(NOW, VERSION, False)):
            with (
                self.subTest(health=health),
                self.assertRaisesRegex(Stop, "^health-unsafe$"),
            ):
                Gate(OPEN, VERSION).observe(NOW, DB, health, COVERED)
        for resource in (
            Coverage("different", OPEN, NOW, 0),
            Coverage(VERSION, OPEN + 1, NOW, 0),
            Coverage(VERSION, OPEN, NOW + 1, 0),
        ):
            with (
                self.subTest(resource=resource),
                self.assertRaisesRegex(Stop, "^resource-coverage-invalid$"),
            ):
                Gate(OPEN, VERSION).observe(NOW, DB, HEALTH, resource)

    def test_db_slots_require_drain_except_for_an_admissible_pair(self):
        gate = Gate(OPEN, VERSION)
        self.assertTrue(gate.observe(NOW, DB, HEALTH, COVERED))
        one = Pressure(NOW, 1, 1)
        self.assertFalse(gate.observe(NOW, one, HEALTH, COVERED))
        self.assertTrue(gate.observe(NOW, one, HEALTH, COVERED, overlapping_pair=True))
        full = Pressure(NOW, 2, 0)
        self.assertFalse(
            gate.observe(NOW, full, HEALTH, COVERED, overlapping_pair=True)
        )

    def test_new_group_requires_zero_observed_after_previous_group_ends(self):
        gate = Gate(OPEN, VERSION)
        self.assertTrue(gate.observe(NOW, DB, HEALTH, COVERED))
        ended = NOW + 1
        gate.record_group_completion(ended)
        for stamp in (NOW, ended):
            with self.subTest(stamp=stamp):
                self.assertFalse(
                    gate.observe(ended + 1, Pressure(stamp, 0, 2), HEALTH, COVERED)
                )
        self.assertFalse(
            gate.observe(ended + 1, Pressure(ended + 1, 1, 1), HEALTH, COVERED)
        )
        self.assertTrue(
            gate.observe(ended + 2, Pressure(ended + 2, 0, 2), HEALTH, COVERED)
        )

    def test_only_platform_resources_can_be_delayed(self):
        gate = Gate(OPEN, VERSION, delayed_resources=True)
        self.assertTrue(gate.observe(NOW, DB, HEALTH, None))
        with self.assertRaisesRegex(Stop, "^health-telemetry-missing-or-stale$"):
            gate.close_load(NOW, DB, None, None)
        gate = Gate(OPEN, VERSION, delayed_resources=True)
        with self.assertRaisesRegex(Stop, "^resource-event$"):
            gate.observe(NOW, DB, HEALTH, Coverage(VERSION, OPEN, NOW, 1))

    def test_reconciliation_requires_complete_clean_coverage_by_deadline(self):
        gate = Gate(OPEN, VERSION, delayed_resources=True)
        gate.close_load(NOW, DB, HEALTH, None)
        self.assertFalse(gate.reconcile(NOW, None))
        self.assertFalse(gate.reconcile(NOW + 1, Coverage(VERSION, OPEN, NOW - 1, 0)))
        self.assertTrue(gate.reconcile(NOW + 120_000, COVERED))
        with self.assertRaisesRegex(Stop, "^resource-reconcile-deadline$"):
            gate.reconcile(NOW + 120_001, COVERED)
        gate = Gate(OPEN, VERSION, delayed_resources=True)
        gate.close_load(NOW, DB, HEALTH, None)
        with self.assertRaisesRegex(Stop, "^resource-coverage-missing$"):
            gate.reconcile(NOW + 120_000, None)
        gate = Gate(OPEN, VERSION, delayed_resources=True)
        gate.close_load(NOW, DB, HEALTH, None)
        with self.assertRaisesRegex(Stop, "^resource-event$"):
            gate.reconcile(NOW + 1, Coverage(VERSION, OPEN, NOW, 1))

    def test_load_edges_refuse_prior_samples_clock_regression_and_late_starts(self):
        with self.assertRaisesRegex(Stop, "^db-telemetry-missing-or-stale$"):
            Gate(OPEN, VERSION).observe(OPEN, Pressure(OPEN - 1, 0, 2), HEALTH, COVERED)
        gate = Gate(OPEN, VERSION)
        gate.observe(NOW, DB, HEALTH, COVERED)
        with self.assertRaisesRegex(Stop, "^telemetry-clock$"):
            gate.observe(NOW - 1, DB, HEALTH, COVERED)
        with self.assertRaisesRegex(Stop, "^load-window$"):
            Gate(OPEN, VERSION).observe(OPEN + 120_000, DB, HEALTH, COVERED)
        gate = Gate(OPEN, VERSION)
        gate.close_load(NOW, DB, HEALTH, COVERED)
        with self.assertRaisesRegex(Stop, "^load-window$"):
            gate.observe(NOW, DB, HEALTH, COVERED)


if __name__ == "__main__":
    unittest.main()
