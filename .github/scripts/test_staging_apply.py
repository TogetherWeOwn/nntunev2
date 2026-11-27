"""Executed tests for the staging-apply driver (finding 5 coverage)."""
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import staging_apply

APP_SHA = "a" * 64
MIRROR_SHA = "b" * 64
DB = "nntunev2_staging"
URL_REQUIRE = ("postgresql://migrator:secretpw@ep.example.com"
               f"/{DB}?sslmode=require")
URL_FULL = ("postgresql://migrator:secretpw@ep.example.com"
            f"/{DB}?sslmode=verify-full")


def manifest_text():
    return f"""[[migration]]
file = "0001_mb_mirror.sql"
sha256 = "{MIRROR_SHA}"
target = "mirror"

[[migration]]
file = "0054_first.sql"
sha256 = "{APP_SHA}"
target = "app"

[[migration]]
file = "0055_second.sql"
sha256 = "{APP_SHA}"
target = "app"
"""


def renamed_manifest_text():
    return f"""[[migration]]
file = "0054_renamed.sql"
sha256 = "{APP_SHA}"
target = "app"

[[migration.renamed_from]]
file = "0054_original.sql"
sha256 = "{APP_SHA}"
"""


class StagingApplyTests(unittest.TestCase):
    def _roots(self, manifest):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = pathlib.Path(tmp.name)
        data = root / "data"
        trusted = root / "trusted"
        (data / "migrations").mkdir(parents=True)
        (trusted / ".github/scripts").mkdir(parents=True)
        (data / "migrations/manifest.toml").write_text(manifest)
        (trusted / ".github/scripts/apply_migration.py").write_text(
            "import sys\nsys.exit(0)\n")
        return data, trusted, root

    def test_census_binding_and_receipt_fail_closed(self):
        from unittest.mock import patch

        # Use the reviewed candidate as input, not the driver's own pin.
        filename = "0051_purge_legacy_provider_credentials.sql"
        head = "10c9e67b73c59efed95c2c693fd2fd1971dbf2e6"
        sha = "ccd14876ce8ed7379c0a03ad1fb4404f12b32f0d31cb470b6b9f1817c8b0e0ca"
        _, _, root = self._roots(manifest_text())
        receipt = root / "census.json"
        env = {"DATA_REVISION": head, "TRUSTED_REVISION": "a" * 40,
               "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "2", "GITHUB_JOB": "readback",
               "GITHUB_STEP_SUMMARY": str(root / "summary"), staging_apply.SECRET_ENV: URL_REQUIRE}
        with patch.object(staging_apply.subprocess, "run") as query:
            with self.assertRaisesRegex(staging_apply.StagingApplyError, "no approved"):
                staging_apply.run_census(filename, sha, DB, env)
            query.assert_not_called()
        url = URL_REQUIRE.replace("migrator", "fixture_observer")
        binding = ("FIXTURE_OBSERVER", "ep.example.com", 5432, "fixture_observer")
        with patch.object(staging_apply, "CENSUS_BINDING", binding), \
             patch.object(staging_apply.subprocess, "run") as query, \
             patch.object(staging_apply, "Path", return_value=receipt):
            for changes in ({}, {"FIXTURE_OBSERVER": url.replace(DB, "production")},
                            {"FIXTURE_OBSERVER": url.replace("ep.example.com", "other.example.com")},
                            {"FIXTURE_OBSERVER": URL_REQUIRE}, {"FIXTURE_OBSERVER": url, "DATA_REVISION": "b" * 40}):
                with self.assertRaises(staging_apply.StagingApplyError):
                    staging_apply.run_census(filename, sha, DB, env | changes)
            query.assert_not_called()
            active = env | {"FIXTURE_OBSERVER": url, "PGHOSTADDR": "unused", "PGSSLMODE": "disable"}
            row = "3\t2\t2026-11-18T20:00:00Z\n"
            for status, stdout in ((1, row), (0, "row-id")):
                query.return_value = SimpleNamespace(returncode=status, stdout=stdout, stderr="secretpw")
                with self.assertRaisesRegex(staging_apply.StagingApplyError, r"details redacted$"):
                    staging_apply.run_census(filename, sha, DB, active)
                self.assertFalse(receipt.exists())
            query.return_value = SimpleNamespace(returncode=0, stdout=row, stderr="")
            with patch.dict(os.environ, active, clear=True):
                self.assertEqual(staging_apply.main([
                    "purge-census", "--file", filename, "--expected-sha", sha, "--expect-database", DB]), 0)
            self.assertIn("REPEATABLE READ READ ONLY", query.call_args.kwargs["input"])
            self.assertIn("SET LOCAL row_security = off", query.call_args.kwargs["input"])
            child = query.call_args.kwargs["env"]
            self.assertEqual((child["PGSSLMODE"], child["PGGSSENCMODE"]), ("verify-full", "disable"))
            self.assertFalse({"PGHOSTADDR", staging_apply.SECRET_ENV} & child.keys())
            self.assertIn("statement_timeout=30000", child["PGOPTIONS"])
            self.assertEqual(query.call_args.kwargs["timeout"], 60)
        expected = dict(target="app", database=DB, guard_head=head, guard_sha256=sha, trusted_revision="a" * 40,
                        run_id="123", run_attempt="2", job="readback", observed_at_utc="2026-11-18T20:00:00Z",
                        user_cache_grants=3, provider_owners_without_account_evidence=2)
        self.assertEqual(json.loads(receipt.read_text()), expected)
        self.assertIn(receipt.read_text(), (root / "summary").read_text())

    def test_ledger_audit_reads_one_snapshot_and_redacts_failures(self):
        contents = b"SELECT 1;\n"
        sha = hashlib.sha256(contents).hexdigest()
        data, trusted, root = self._roots(manifest_text().replace(APP_SHA, sha))
        (data / "migrations/0054_first.sql").write_bytes(contents)
        (trusted / "migrations").mkdir()
        (trusted / "migrations/manifest.toml").write_text(manifest_text().replace(APP_SHA, sha))
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(root / "summary"), "AUDIT_RECEIPT": str(root / "audit"),
               "TRUSTED_REVISION": "a" * 40, "DATA_REVISION": "b" * 40,
               "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_JOB": "readback"}
        row = ["0054_first.sql", sha, "2026-11-17 04:00:00+00", "release:operator```<tag>\x7fé"]
        from unittest.mock import patch
        response = SimpleNamespace(returncode=0, stdout=json.dumps({"database": DB, "plain_table": True, "rows": [row]}))
        with patch.object(staging_apply.subprocess, "run", return_value=response) as runner:
            staging_apply.run_ledger_audit(trusted, data, row[0], sha, DB, env)
        self.assertEqual(runner.call_count, 1)
        self.assertIn("REPEATABLE READ READ ONLY", runner.call_args.kwargs["input"])
        self.assertIn("SET LOCAL row_security = off", runner.call_args.kwargs["input"])
        self.assertIn("SET LOCAL DateStyle = 'ISO, YMD'", runner.call_args.kwargs["input"])
        for plain_table in (False, None, 1):
            response.stdout = json.dumps({"database": DB, "plain_table": plain_table, "rows": [row]})
            with patch.object(staging_apply.subprocess, "run", return_value=response):
                with self.assertRaises(staging_apply.StagingApplyError):
                    staging_apply.run_ledger_audit(trusted, data, row[0], sha, DB, env)
        self.assertIn("-c search_path=pg_catalog", runner.call_args.kwargs["env"]["PGOPTIONS"])
        self.assertNotIn("public", runner.call_args.kwargs["env"]["PGOPTIONS"])
        receipt_text = (root / "audit").read_text()
        self.assertNotIn("secretpw", receipt_text)
        self.assertEqual(json.loads(receipt_text)["rows"][0]["applied_by"], "[redacted]")
        self.assertNotIn(row[3], receipt_text)
        summary = (root / "summary").read_text()
        self.assertEqual(summary.count("```"), 2)
        self.assertEqual(json.loads(summary.split("```json\n")[1].split("```\n")[0])["rows"][0]["applied_by"], "[redacted]")
        response.stdout = json.dumps({"database": "production", "rows": [row]})
        with patch.object(staging_apply.subprocess, "run", return_value=response):
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.run_ledger_audit(trusted, data, row[0], sha, DB, env)
        response.stdout = '{"plain_table":false,"plain_table":true,' + json.dumps(
            {"database": DB, "rows": [row]})[1:]
        with patch.object(staging_apply.subprocess, "run", return_value=response):
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.run_ledger_audit(trusted, data, row[0], sha, DB, env)
        (trusted / "migrations/manifest.toml").write_text(renamed_manifest_text())
        with patch.object(staging_apply.subprocess, "run") as runner:
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.run_ledger_audit(trusted, data, row[0], sha, DB, env)
            runner.assert_not_called()

    def test_ledger_audit_never_publishes_actor_text(self):
        contents = b"SELECT 1;\n"
        sha = hashlib.sha256(contents).hexdigest()
        manifest = manifest_text().replace(APP_SHA, sha)
        data, trusted, root = self._roots(manifest)
        for name in ("0054_first.sql", "0055_second.sql"):
            (data / "migrations" / name).write_bytes(contents)
        (trusted / "migrations").mkdir()
        (trusted / "migrations/manifest.toml").write_text(manifest)
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(root / "summary"), "AUDIT_RECEIPT": str(root / "audit"),
               "TRUSTED_REVISION": "a" * 40, "DATA_REVISION": "b" * 40,
               "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_JOB": "readback"}
        actors = ("sample-actor-one", "opaqueActor123456789")
        rows = [[name, sha, "2026-11-17 04:00:00+00", actor]
                for name, actor in zip(("0054_first.sql", "0055_second.sql"), actors)]
        response = SimpleNamespace(returncode=0, stdout=json.dumps({"database": DB, "plain_table": True, "rows": rows}))
        from unittest.mock import patch
        with patch.object(staging_apply.subprocess, "run", return_value=response):
            staging_apply.run_ledger_audit(trusted, data, rows[-1][0], sha, DB, env)
        for path in (root / "audit", root / "summary"):
            text = path.read_text()
            for actor in actors:
                self.assertNotIn(actor, text)
            self.assertEqual(text.count('"applied_by": "[redacted]"'), 2)
            for row in rows:
                self.assertIn(row[0], text)
                self.assertIn(row[1], text)

    def test_ledger_audit_requires_complete_unique_app_prefix(self):
        data, _, _ = self._roots(manifest_text())
        entries = staging_apply.load_entries(data)
        first = ["0054_first.sql", APP_SHA, "2026-11-17 04:00:00+00", "migrator"]
        second = ["0055_second.sql", APP_SHA, "2026-11-17 04:01:00+00", "migrator"]
        validate = staging_apply.validate_ledger_audit
        self.assertEqual(len(validate(entries, [first, second], "0055_second.sql")), 2)
        actor = [*first[:3], "release:operator"]
        self.assertEqual(validate(entries, [actor, second], "0055_second.sql")[0]["applied_by"], "[redacted]")
        historical = [*first[:3], "backfill-2026-11-03 (operator-verified)"]
        self.assertEqual(validate(entries, [historical, second], "0055_second.sql")[0]["applied_by"], "[redacted]")
        for rows in ([second], [first, first, second],
                     [first, [second[0], "f" * 64, *second[2:]]],
                     [first, second, ["0056_other.sql", APP_SHA, *first[2:]]],
                     [first, [*second[:3], "bad\nactor"]]):
            with self.subTest(rows=rows), self.assertRaises(staging_apply.StagingApplyError):
                validate(entries, rows, "0055_second.sql")
        with self.assertRaises(staging_apply.StagingApplyError):
            validate(entries, [first], "0001_mb_mirror.sql")

    def test_ledger_audit_accepts_exact_historical_alias_once(self):
        data, _, _ = self._roots(renamed_manifest_text())
        entries = staging_apply.load_entries(data)
        old = ["0054_original.sql", APP_SHA, "2026-11-17 04:00:00+00", "migrator"]
        new = ["0054_renamed.sql", APP_SHA, *old[2:]]
        self.assertEqual(staging_apply.validate_ledger_audit(entries, [old], "0054_renamed.sql")[0]["file"], old[0])
        with self.assertRaises(staging_apply.StagingApplyError):
            staging_apply.validate_ledger_audit(entries, [old, new], "0054_renamed.sql")

    def test_readback_observes_exact_row_without_apply(self):
        contents = b"SELECT 1;\n"
        sha = hashlib.sha256(contents).hexdigest()
        data, _, root = self._roots(manifest_text().replace(APP_SHA, sha))
        (data / "migrations/0054_first.sql").write_bytes(contents)
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(root / "summary"),
               "READBACK_RECEIPT": str(root / "receipt"),
               "TRUSTED_REVISION": "a" * 40, "DATA_REVISION": "b" * 40,
               "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_JOB": "observe"}
        valid = f"0054_first.sql\t{sha}\t2026-11-16 01:02:03+00\tactor"
        calls = []

        def fake_psql(argv, **kwargs):
            calls.append((argv, kwargs))
            return SimpleNamespace(returncode=0, stdout=valid.encode())

        from unittest.mock import patch
        with patch.object(staging_apply.subprocess, "run", side_effect=fake_psql), \
             patch.object(staging_apply, "apply_files", side_effect=AssertionError("apply reached")):
            staging_apply.run_readback(str(data), "0054_first.sql", sha, DB, env)
        receipt = json.loads((root / "receipt").read_text())
        self.assertEqual(receipt["applied_by"], "actor")
        self.assertEqual(receipt["kind"], "staging-ledger-readback")
        self.assertIn("not original apply", (root / "summary").read_text())
        self.assertEqual(len(calls), 1)
        argv, kw = calls[0]
        self.assertEqual(argv[0], "psql")
        self.assertNotIn("secretpw", str(argv))
        self.assertEqual(kw["env"]["PGSSLMODE"], "verify-full")
        self.assertIn("default_transaction_read_only=on", kw["env"]["PGOPTIONS"])
        self.assertTrue(kw["input"].startswith(b"SELECT file, sha256, applied_at, applied_by"))
        self.assertNotIn("secretpw", (root / "receipt").read_text())
        with patch.object(staging_apply.subprocess, "run", return_value=SimpleNamespace(returncode=1, stdout="", stderr=URL_REQUIRE)):
            with self.assertRaises(staging_apply.StagingApplyError) as error:
                staging_apply.run_readback(str(data), "0054_first.sql", sha, DB, env)
            self.assertNotIn("secretpw", str(error.exception))
        with self.assertRaises(staging_apply.StagingApplyError):
            staging_apply.evidence_child_env(URL_REQUIRE + "&sslrootcert=/tmp/untrusted", DB)
        for bad in ("", valid + "\n" + valid, valid.replace(sha, "f" * 64)):
            with self.subTest(row=bad), patch.object(staging_apply.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout=bad.encode())):
                with self.assertRaises(staging_apply.StagingApplyError):
                    staging_apply.run_readback(str(data), "0054_first.sql", sha, DB, env)
        with patch.object(staging_apply.subprocess, "run") as runner:
            for file, digest, db in (("../oops", sha, DB), ("0054_first.sql", "f" * 64, DB),
                                     ("0054_first.sql", sha, "production")):
                with self.assertRaises(staging_apply.StagingApplyError):
                    staging_apply.run_readback(str(data), file, digest, db, env)
            runner.assert_not_called()

    def test_readback_rejects_actor_carriage_return_at_subprocess_boundary(self):
        contents = b"SELECT 1;\n"
        sha = hashlib.sha256(contents).hexdigest()
        data, _, root = self._roots(manifest_text().replace(APP_SHA, sha))
        (data / "migrations/0054_first.sql").write_bytes(contents)
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(root / "summary"),
               "READBACK_RECEIPT": str(root / "receipt"),
               "TRUSTED_REVISION": "a" * 40, "DATA_REVISION": "b" * 40,
               "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_JOB": "observe"}
        output = f"0054_first.sql\t{sha}\t2026-11-16\tactor\r\n".encode()
        real_run = subprocess.run

        def fake_psql(argv, **kwargs):
            return real_run([sys.executable, "-c", "import sys; sys.stdout.buffer.write(%r)" % output],
                            **kwargs)

        from unittest.mock import patch
        with patch.object(staging_apply.subprocess, "run", side_effect=fake_psql):
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.run_readback(str(data), "0054_first.sql", sha, DB, env)
        self.assertFalse((root / "receipt").exists())

    # Finding 2: single mode refuses a mirror file before connecting.
    def test_single_mode_refuses_mirror_before_connecting(self):
        def fail_runner(*args, **kwargs):
            raise AssertionError("subprocess must not run for a mirror file")
        data, trusted, _ = self._roots(manifest_text())
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fail_runner
        try:
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.apply_files(str(trusted), str(data),
                                          "0001_mb_mirror.sql", "tester",
                                          "single", DB, env)
        finally:
            staging_apply.subprocess.run = original

    def test_plan_files_single_unknown_file_refuses(self):
        entries = staging_apply.load_entries(
            str(self._roots(manifest_text())[0]))
        with self.assertRaises(staging_apply.StagingApplyError):
            staging_apply.plan_files(entries, "0099_unknown.sql", "single")

    def test_plan_files_all_pending_walks_app_order_only(self):
        data, _, _ = self._roots(manifest_text())
        entries = staging_apply.load_entries(str(data))
        self.assertEqual(
            staging_apply.plan_files(entries, "0055_second.sql", "all-pending"),
            ["0054_first.sql", "0055_second.sql"])

    # Finding 3: one shared normalization forcing verify-full.
    def test_normalize_forces_verify_full_over_require(self):
        normalized = staging_apply.normalize_url(URL_REQUIRE, DB)
        self.assertIn("sslmode=verify-full", normalized)
        self.assertNotIn("require", normalized)

    def test_normalize_rejects_invalid_port_and_blank_root_cert(self):
        for url in (URL_REQUIRE.replace(".com", ".com:invalid"), URL_REQUIRE + "&sslrootcert="):
            with self.subTest(url=url), self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.normalize_url(url, DB)

    def test_manifest_cannot_use_symlinked_migrations_directory(self):
        data, _, root = self._roots(manifest_text())
        migrations = data / "migrations"
        original = migrations.rename(root / "other-migrations")
        migrations.symlink_to(original, target_is_directory=True)
        with self.assertRaises(staging_apply.StagingApplyError):
            staging_apply.load_entries(data)

    def test_evidence_child_env_forces_verify_full_over_require(self):
        env = staging_apply.evidence_child_env(URL_REQUIRE, DB)
        self.assertEqual(env["PGSSLMODE"], "verify-full")
        apply_url = staging_apply.normalize_url(URL_REQUIRE, DB)
        self.assertIn("verify-full", apply_url)
        # Both phases accept the same secret: no sslmode rejection.
        full_env = staging_apply.evidence_child_env(URL_FULL, DB)
        self.assertEqual(full_env["PGSSLMODE"], "verify-full")

    # Finding 4: evidence finds a renamed (alias-recorded) row.
    def test_evidence_accepts_renamed_alias_row(self):
        data, _, root = self._roots(renamed_manifest_text())
        summary = root / "summary.md"
        summary.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(summary),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}

        def fake_psql(argv, **kwargs):
            # Row exists only under the pre-rename name.
            return SimpleNamespace(
                returncode=0,
                stdout=f"0054_original.sql\t{APP_SHA}\t2026-11-11\tci\n".encode())

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_psql
        try:
            staging_apply.run_evidence(str(data), "0054_renamed.sql",
                                       "0054_renamed.sql", "success", DB, env)
        finally:
            staging_apply.subprocess.run = original
        self.assertIn("0054_original.sql", summary.read_text())

    def test_evidence_query_includes_alias_names(self):
        data, _, _ = self._roots(renamed_manifest_text())
        entries = staging_apply.load_entries(str(data))
        entry = staging_apply.find_entry(entries, "0054_renamed.sql")
        self.assertEqual(sorted(staging_apply.entry_names(entry)),
                         ["0054_original.sql", "0054_renamed.sql"])

    def test_evidence_rejects_unverified_rows(self):
        from unittest.mock import patch

        data, _, root = self._roots(renamed_manifest_text())
        env = {staging_apply.SECRET_ENV: URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(root / "summary"), "APPLY_PROGRESS": str(root / "progress")}
        valid = f"0054_original.sql\t{APP_SHA}\t2026-11-11\tci\n"
        for row in (valid.replace(APP_SHA, MIRROR_SHA), valid + valid):
            with self.subTest(row=row), patch.object(staging_apply.subprocess, "run") as runner:
                runner.return_value = SimpleNamespace(returncode=0, stdout=row.encode())
                with self.assertRaises(staging_apply.StagingApplyError):
                    staging_apply.run_evidence(data, "0054_renamed.sql", "0054_renamed.sql", "success", DB, env)

    # First-failure stop with applied-prefix evidence.
    def test_first_failure_stops_and_reports_applied_prefix(self):
        data, trusted, root = self._roots(manifest_text())
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output), "APPLY_PROGRESS": str(root / "progress"),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        calls = []

        def fake_runner(argv, **kwargs):
            calls.append(argv)
            # First file succeeds, second fails.
            code = 0 if len(calls) == 1 else 1
            return SimpleNamespace(returncode=code, stdout="", stderr="boom")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_runner
        try:
            with self.assertRaises(staging_apply.StagingApplyError):
                staging_apply.apply_files(str(trusted), str(data),
                                          "0055_second.sql", "tester",
                                          "all-pending", DB, env)
        finally:
            staging_apply.subprocess.run = original
        self.assertEqual(len(calls), 2)
        self.assertEqual(output.read_text().strip(),
                         "applied_files=0054_first.sql")

    # In-flight file is named in the evidence summary on failure.
    def test_in_flight_file_reported_on_failure(self):
        data, _, root = self._roots(manifest_text())
        summary = root / "summary.md"
        summary.write_text("")
        progress = root / "progress"
        progress.write_text("0055_second.sql")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_STEP_SUMMARY": str(summary),
               "APPLY_PROGRESS": str(progress),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}

        def fake_psql(argv, **kwargs):
            return SimpleNamespace(returncode=0, stdout=b"")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_psql
        try:
            staging_apply.run_evidence(str(data), "0055_second.sql",
                                       "0054_first.sql", "failure", DB, env)
        finally:
            staging_apply.subprocess.run = original
        text = summary.read_text()
        self.assertIn("In-flight at failure", text)
        self.assertIn("0055_second.sql", text)
        self.assertIn("reconcile", text)

    # Finding 1: only trusted code executes; data code never runs, and
    # the child environment is allowlisted.
    def test_only_trusted_migrator_executes_with_allowlisted_env(self):
        data, trusted, root = self._roots(manifest_text())
        # Malicious scripts in the data checkout: must never execute.
        (data / ".github/scripts").mkdir(parents=True, exist_ok=True)
        (data / ".github/scripts/apply_migration.py").write_text(
            "import pathlib; pathlib.Path('pwned.txt').write_text('x')\n")
        # Trusted stub: records that it ran and dumps its env keys.
        stub = trusted / ".github/scripts/apply_migration.py"
        stub.write_text(
            "import os, pathlib\n"
            "pathlib.Path('ran.txt').write_text('ok')\n"
            "pathlib.Path('env.txt').write_text(','.join(sorted(os.environ)))\n")
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output), "APPLY_PROGRESS": str(root / "progress"),
               "PATH": os.environ["PATH"], "HOME": "/tmp",
               "FAKE_SECRET": "spike-value", "GITHUB_TOKEN": "spike"}
        staging_apply.apply_files(str(trusted), str(data),
                                  "0054_first.sql", "tester",
                                  "single", DB, env)
        self.assertTrue((trusted / "ran.txt").is_file())
        self.assertFalse((trusted / "pwned.txt").exists())
        self.assertFalse((data / "pwned.txt").exists())
        env_keys = (trusted / "env.txt").read_text()
        self.assertIn("DATABASE_URL", env_keys)
        self.assertNotIn("FAKE_SECRET", env_keys)
        self.assertNotIn("GITHUB_TOKEN", env_keys)
        self.assertNotIn("PLANETSCALE_MIGRATOR_URL_STAGING", env_keys)

    def test_apply_argv_points_at_trusted_script_and_data_files(self):
        data, trusted, root = self._roots(manifest_text())
        output = root / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output), "APPLY_PROGRESS": str(root / "progress"),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        seen = []

        def fake_runner(argv, **kwargs):
            seen.append(argv)
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        original = staging_apply.subprocess.run
        staging_apply.subprocess.run = fake_runner
        try:
            staging_apply.apply_files(str(trusted), str(data),
                                      "0054_first.sql", "tester",
                                      "single", DB, env)
        finally:
            staging_apply.subprocess.run = original
        argv = seen[0]
        self.assertIn(str(trusted / ".github/scripts/apply_migration.py"), argv)
        self.assertIn(str(data / "migrations/0054_first.sql"), argv)
        self.assertIn(str(data / "migrations/manifest.toml"), argv)
        # -E -s (not isolated -I): the flag set ships with the driver and
        # keeps the trusted script directory on sys.path for the sibling
        # import (finding 1 in the return-#2 fix).
        self.assertIn("-E", argv)
        self.assertIn("-s", argv)
        self.assertNotIn("-I", argv)

    # Return-#2 finding 4: executed regression in the workflow's exact
    # invocation shape (subprocess, relative roots, sibling import),
    # which the in-process tests above cannot see.
    def test_driver_subprocess_reaches_child_with_relative_roots(self):
        scripts_dir = pathlib.Path(__file__).resolve().parent
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        workspace = pathlib.Path(tmp.name)
        trusted_scripts = workspace / "trusted/.github/scripts"
        data_migrations = workspace / "data/migrations"
        trusted_scripts.mkdir(parents=True)
        data_migrations.mkdir(parents=True)
        # Real driver + real sibling module so the import chain executes.
        (trusted_scripts / "staging_apply.py").write_bytes(
            (scripts_dir / "staging_apply.py").read_bytes())
        (trusted_scripts / "migration_history.py").write_bytes(
            (scripts_dir / "migration_history.py").read_bytes())
        # Trusted migrator stub: imports the sibling module and refuses
        # unless --file/--manifest resolve to existing files.
        (trusted_scripts / "apply_migration.py").write_text(
            "import migration_history, pathlib, sys\n"
            "argv = sys.argv\n"
            "file_arg = argv[argv.index('--file') + 1]\n"
            "manifest_arg = argv[argv.index('--manifest') + 1]\n"
            "assert pathlib.Path(file_arg).is_file(), file_arg\n"
            "assert pathlib.Path(manifest_arg).is_file(), manifest_arg\n"
            "pathlib.Path('child_ran.txt').write_text('ok')\n")
        (data_migrations / "manifest.toml").write_text(manifest_text())
        (data_migrations / "0054_first.sql").write_text("SELECT 1;\n")
        output = workspace / "outputs.txt"
        output.write_text("")
        env = {"PLANETSCALE_MIGRATOR_URL_STAGING": URL_REQUIRE,
               "GITHUB_OUTPUT": str(output),
               "APPLY_PROGRESS": str(workspace / "progress"),
               "PATH": os.environ["PATH"], "HOME": "/tmp"}
        result = subprocess.run(
            [sys.executable, "-E", "-s",
             "trusted/.github/scripts/staging_apply.py", "apply",
             "--trusted-root", "trusted", "--data-root", "data",
             "--file", "0054_first.sql", "--applied-by", "tester",
             "--mode", "single", "--expect-database", DB],
            cwd=str(workspace), env=env, capture_output=True, text=True,
            timeout=120)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(
            (workspace / "trusted/child_ran.txt").is_file()
            or (workspace / "child_ran.txt").is_file())
        self.assertIn("applied_files=0054_first.sql", output.read_text())


if __name__ == "__main__":
    unittest.main()
