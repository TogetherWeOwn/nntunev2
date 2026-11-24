"""Exercise observer decisions and the credential boundary without a database."""

import re
import runpy
import subprocess
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest.mock import Mock, patch

OBSERVER = runpy.run_path(str(Path(__file__).with_name("subsonic_staging_observer.py")))
sample = OBSERVER["sample"]
parse_sample = OBSERVER["parse_sample"]
require_preflight = OBSERVER["require_preflight"]
UnsafeSample = OBSERVER["UnsafeSample"]

SECRET = "PLANETSCALE_OBSERVER_URL_STAGING"
URL = "postgresql://nntune_staging_observer.examplebranch:synthetic%21@db.example.test/postgres?sslmode=verify-full&sslrootcert=system"
ENV = {
    SECRET: URL,
    "PGSERVICE": "wrong",
    "PGHOSTADDR": "192.0.2.1",
    "OTHER_SECRET": "not-forwarded",
}
VALID = b"100\n0|20|1|25|3\n"


class ObserverTests(unittest.TestCase):
    def test_fixed_sql_and_connection_boundary(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID))
        row = sample(ENV, run=run, wall=lambda: 100, monotonic=lambda: 0)
        require_preflight(row)
        self.assertEqual(row.ordinary_free, 2)
        args, kwargs = run.call_args
        sql = kwargs["input"]
        self.assertIn(b"BEGIN READ ONLY;", sql)
        self.assertIn(
            b"current_user = 'nntune_staging_observer' AND current_database() = 'postgres'",
            sql,
        )
        self.assertIn(b"\\if :observer_identity", sql)
        self.assertEqual(sql.count(b"SELECT * FROM observer.connection_counts();"), 1)
        self.assertIn(b"clock_timestamp()", sql)
        self.assertNotIn(b"synthetic", sql)
        self.assertNotIn("synthetic", repr(args))
        self.assertIn("--no-psqlrc", args[0])
        self.assertIn("--no-password", args[0])
        self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
        self.assertEqual(kwargs["timeout"], 5)
        child = kwargs["env"]
        for name in ENV:
            self.assertNotIn(name, child)
        self.assertEqual(child["PGSSLMODE"], "verify-full")
        self.assertEqual(child["PGSSLROOTCERT"], "/etc/ssl/certs/ca-certificates.crt")
        self.assertEqual(child["PGGSSENCMODE"], "disable")
        self.assertEqual(child["PGCONNECT_TIMEOUT"], "2")
        self.assertIn("statement_timeout=1000", child["PGOPTIONS"])
        self.assertIn("lock_timeout=1000", child["PGOPTIONS"])
        self.assertEqual(child["PGUSER"], "nntune_staging_observer.examplebranch")
        self.assertEqual(child["PGPASSWORD"], "synthetic!")

    def test_invalid_config_never_connects(self):
        run = Mock()
        for url in (
            "",
            URL.replace("verify-full", "require"),
            URL + "&sslmode=verify-full",
            URL.replace("/postgres?", "/other?"),
            URL.replace("staging_observer", "other"),
            URL.replace("db.example.test", "a.example.test,b.example.test"),
            URL + "&options=-csearch_path=other",
            URL + "&sslrootcert=",
            URL + "#fragment",
        ):
            with (
                self.subTest(url=url),
                self.assertRaisesRegex(UnsafeSample, "^config$"),
            ):
                sample({SECRET: url}, run=run)
        run.assert_not_called()

    def test_nonzero_timeout_and_late_result_stop(self):
        for result, category in (
            (subprocess.CompletedProcess([], 2, stdout=VALID), "query"),
            (subprocess.TimeoutExpired("psql", 5), "deadline"),
            (FileNotFoundError(), "client"),
        ):
            run = (
                Mock(side_effect=result)
                if isinstance(result, Exception)
                else Mock(return_value=result)
            )
            with (
                self.subTest(category=category),
                self.assertRaisesRegex(UnsafeSample, f"^{category}$"),
            ):
                sample(ENV, run=run)
        with self.assertRaisesRegex(UnsafeSample, "^deadline$"):
            sample(
                ENV,
                run=Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID)),
                wall=lambda: 100,
                monotonic=iter((0, 6)).__next__,
            )

    def test_pressure_rejects_invalid_deadlines_before_calling_client(self):
        for deadline in (None, "1500", True, -1):
            run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID))
            with self.subTest(deadline=deadline):
                with self.assertRaisesRegex(UnsafeSample, "^deadline$"):
                    OBSERVER["sample_pressure"](
                        ENV,
                        deadline_ms=deadline,
                        run=run,
                        wall=lambda: 100,
                        monotonic=lambda: 0,
                    )
                run.assert_not_called()

    def test_pressure_uses_query_start_and_remaining_budget(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID))
        reading = OBSERVER["sample_pressure"](
            ENV,
            deadline_ms=1625,
            run=run,
            wall=iter((99, 100)).__next__,
            monotonic=iter((1.125, 1.25, 1.5, 1.6)).__next__,
        )
        # A zero arriving at 1500 cannot by itself clear a fence at 1125.
        self.assertEqual(reading.observed_ms, 1125)
        self.assertEqual(reading.measurement, 0)
        self.assertEqual(reading.ordinary_free, 2)
        run.assert_called_once()
        self.assertEqual(run.call_args.kwargs["timeout"], 0.375)

    def test_pressure_drops_expired_late_and_stale_samples(self):
        for deadline, clocks, raw, calls, category in (
            (1000, (1,), VALID, 0, "deadline"),
            (1500, (1, 1, 1.5), VALID, 1, "deadline"),
            (9000, (1, 1, 4), VALID, 1, "deadline"),
            (9000, (1, 1, 2), b"96\n0|20|1|25|3\n", 1, "stale"),
        ):
            run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=raw))
            with (
                self.subTest(deadline=deadline, clocks=clocks, category=category),
                self.assertRaisesRegex(UnsafeSample, f"^{category}$"),
            ):
                OBSERVER["sample_pressure"](
                    ENV,
                    deadline_ms=deadline,
                    run=run,
                    wall=lambda: 100,
                    monotonic=iter(clocks).__next__,
                )
            self.assertEqual(run.call_count, calls)
            if calls:
                self.assertEqual(
                    run.call_args.kwargs["timeout"], min(3, deadline / 1000 - 1)
                )

    def test_pressure_does_not_dispatch_after_wall_clock_setup_crosses_deadline(self):
        now = 1
        run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID))

        def wall():
            nonlocal now
            now = 1.501
            return 100

        with self.assertRaisesRegex(UnsafeSample, "^deadline$"):
            OBSERVER["sample_pressure"](
                ENV, deadline_ms=1500, run=run, wall=wall, monotonic=lambda: now
            )
        run.assert_not_called()

    def test_pressure_rechecks_both_cutoffs_after_validation(self):
        def check(deadline, returned, validated, slow_step):
            now = 1

            def query(*args, **kwargs):
                nonlocal now
                now = returned
                return subprocess.CompletedProcess([], 0, stdout=VALID)

            def wall():
                nonlocal now
                if slow_step == "wall" and now == returned:
                    now = validated
                return 100

            def parse(*args):
                nonlocal now
                if slow_step == "parse":
                    now = validated
                return parse_sample(*args)

            run = Mock(side_effect=query)
            parser = Mock(side_effect=parse)
            with (
                patch.dict(sample.__globals__, {"parse_sample": parser}),
                self.assertRaisesRegex(UnsafeSample, "^deadline$"),
            ):
                OBSERVER["sample_pressure"](
                    ENV, deadline_ms=deadline, run=run, wall=wall, monotonic=lambda: now
                )
            run.assert_called_once()
            if slow_step == "parse":
                parser.assert_called_once_with(VALID, 100, 100)

        for deadline, returned, validated in ((1500, 1.499, 1.501), (9000, 3.999, 4)):
            for slow_step in ("wall", "parse"):
                with self.subTest(deadline=deadline, slow_step=slow_step):
                    check(deadline, returned, validated, slow_step)

    def test_bad_shape_identity_and_stale_telemetry_stop(self):
        for raw, category in (
            (b"wrong-observer-identity\n", "identity"),
            (b"", "shape"),
            (VALID + VALID, "shape"),
            (b"100\n0|20||25|3\n", "shape"),
            (b"100\n0|20|0|25|3\n", "counts"),
            (b"100\n0|26|1|25|3\n", "counts"),
            (b"100\n0|20|1|25|25\n", "counts"),
            (b"100\n2|1|1|25|3\n", "counts"),
            (b"96\n0|20|1|25|3\n", "stale"),
            (b"102\n0|20|1|25|3\n", "stale"),
        ):
            with (
                self.subTest(raw=raw),
                self.assertRaisesRegex(UnsafeSample, f"^{category}$"),
            ):
                parse_sample(raw, 98, 100)

    def test_preflight_excludes_reserved_slots_and_requires_idle_measurement(self):
        for raw in (b"100\n0|21|1|25|3\n", b"100\n1|20|1|25|3\n"):
            with (
                self.subTest(raw=raw),
                self.assertRaisesRegex(UnsafeSample, "^pressure$"),
            ):
                require_preflight(parse_sample(raw, 100, 100))

    def test_cli_accepts_result_at_exact_five_second_limit(self):
        output = StringIO()
        run = Mock(return_value=subprocess.CompletedProcess([], 0, stdout=VALID))

        def smoke_sample(env):
            return sample(
                env, run=run, wall=lambda: 100, monotonic=iter((0, 5)).__next__
            )

        with (
            patch.dict("os.environ", ENV, clear=True),
            patch.dict(sample.__globals__, {"sample": smoke_sample}),
            redirect_stdout(output),
        ):
            self.assertEqual(OBSERVER["main"](), 0)
        self.assertIn("staging-observer: PASS", output.getvalue())
        run.assert_called_once()
        self.assertEqual(run.call_args.kwargs["timeout"], 5)

    def test_cli_never_emits_malformed_telemetry(self):
        output = StringIO()
        leaked_row = subprocess.CompletedProcess([], 0, stdout=URL.encode())
        with (
            patch.dict("os.environ", ENV, clear=True),
            patch.dict(
                sample.__globals__,
                {"sample": lambda env: sample(env, run=Mock(return_value=leaked_row))},
            ),
            redirect_stdout(output),
        ):
            self.assertEqual(OBSERVER["main"](), 1)
        self.assertEqual(output.getvalue(), "staging-observer: UNSAFE category=shape\n")

    def test_secret_referenced_only_by_manual_protected_main_observer(self):
        workflows = Path(__file__).resolve().parents[1] / "workflows"
        path = workflows / "subsonic-staging-observer.yml"
        text = path.read_text()
        self.assertRegex(
            text, r"\non:\n  workflow_dispatch:\n\npermissions:\n  contents: read\n"
        )
        for guard in (
            "github.repository == 'TogetherWeOwn/nntunev2'",
            "github.ref == 'refs/heads/main'",
            "github.ref_protected",
            "github.workflow_sha == github.sha",
            "github.run_attempt == 1",
        ):
            self.assertIn(guard, text)
        self.assertIn(
            "environment:\n      name: staging\n      deployment: false", text
        )
        self.assertIn(
            "ref: ${{ github.sha }}\n          persist-credentials: false", text
        )
        self.assertRegex(text, r"checkout@[0-9a-f]{40}\b")
        self.assertEqual(re.findall(r"secrets\.([A-Z_]+)", text), [SECRET])
        self.assertNotIn("/rest", text)
        self.assertNotIn("upload-artifact", text)


if __name__ == "__main__":
    unittest.main()
