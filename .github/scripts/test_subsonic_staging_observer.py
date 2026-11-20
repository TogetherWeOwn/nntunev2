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
URL = "postgresql://nntune_staging_observer:synthetic%21@db.example.test/postgres?sslmode=verify-full&sslrootcert=system"
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
