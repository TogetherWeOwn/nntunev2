"""Pin the migrations-staging PR-body parser: pass, fail and pending cases."""

import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migrations_staging_declaration as msd  # noqa: E402

SCRIPT = pathlib.Path(__file__).with_name("migrations_staging_declaration.py")
FILES = ["migrations/0024_session_refresh_retry.sql", "migrations/0025_session_refresh_claim_clock.sql", "README.md"]


class CheckTestCase(unittest.TestCase):
    def assertPasses(self, body, paths=FILES):
        ok, message = msd.check(body, paths)
        self.assertTrue(ok, message)
        return message

    def assertFails(self, body, paths=FILES):
        ok, message = msd.check(body, paths)
        self.assertFalse(ok, message)
        return message

    def test_no_migrations_needs_no_line(self):
        self.assertPasses("", ["README.md", "workers/api/migrations/x.sql", "migrations/manifest.toml"])

    def test_missing_line_fails_naming_files(self):
        message = self.assertFails("## What changed\n\n- stuff\n")
        self.assertIn("migrations/0024_session_refresh_retry.sql", message)
        self.assertIn("migrations/0025_session_refresh_claim_clock.sql", message)
        self.assertIn("docs/ops.md", message)

    def test_applied_line_passes(self):
        self.assertPasses(
            "Staging migration: 0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql"
            " — applied 2026-10-30 by migrator (#900)\n"
        )

    def test_applied_accepts_paths_backticks_bullets_and_ascii_dash(self):
        self.assertPasses(
            "- [x] staging migration: `migrations/0024_session_refresh_retry.sql` "
            "`migrations/0025_session_refresh_claim_clock.sql` - applied 2026-10-30 by ops\n"
        )

    def test_applied_lines_may_split_the_list(self):
        self.assertPasses(
            "Staging migration: 0024_session_refresh_retry.sql — applied 2026-10-30 by a\n"
            "Staging migration: 0025_session_refresh_claim_clock.sql — applied 2026-10-31 by b\n"
        )

    def test_applied_line_missing_one_file_fails_naming_it(self):
        message = self.assertFails("Staging migration: 0024_session_refresh_retry.sql — applied 2026-10-30 by a\n")
        self.assertIn("0025_session_refresh_claim_clock.sql", message)
        self.assertNotIn("0024", message)

    def test_applied_line_needs_date_and_who(self):
        for value in (
            "0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql — applied by a",
            "0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql — applied 30/10/2026 by a",
            "0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql — applied 2026-10-30",
            "0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql — applied 2026-02-31 by a",
            "0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql — applied 2026-10-30 by <who>",
            "<files> — applied <date> by <who>",
        ):
            with self.subTest(value=value):
                self.assertFails(f"Staging migration: {value}\n")

    def test_pending_line_passes(self):
        message = self.assertPasses("Staging migration: pending (blocked on #900)\n")
        self.assertIn("#900", message)

    def test_pending_needs_ticket(self):
        for value in ("pending", "pending ()", "pending (blocked on )", "pending (#900)", "pending (blocked on <ticket>)"):
            with self.subTest(value=value):
                self.assertFails(f"Staging migration: {value}\n")

    def test_line_must_start_the_line(self):
        self.assertFails("Note: Staging migration: pending (blocked on #900)\n")

    def test_html_comment_example_does_not_count(self):
        self.assertFails(
            "<!--\nStaging migration: pending (blocked on #1)\n"
            "Staging migration: 0024_session_refresh_retry.sql, 0025_session_refresh_claim_clock.sql"
            " — applied 2026-10-30 by x\n-->\n"
        )

    def test_none_body_fails(self):
        self.assertFails(None)


class CliTestCase(unittest.TestCase):
    def run_cli(self, body, paths):
        with tempfile.TemporaryDirectory() as tmp:
            body_file = pathlib.Path(tmp, "body")
            files_file = pathlib.Path(tmp, "files")
            body_file.write_text(body, encoding="utf-8")
            files_file.write_text("\n".join(paths) + "\n", encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(SCRIPT), "--body-file", str(body_file), "--files-file", str(files_file)],
                capture_output=True,
                text=True,
                check=False,
            )

    def test_exit_codes(self):
        self.assertEqual(self.run_cli("", ["README.md"]).returncode, 0)
        failed = self.run_cli("", FILES)
        self.assertEqual(failed.returncode, 1)
        self.assertIn("::error::migrations-staging:", failed.stdout)
        pending = self.run_cli("Staging migration: pending (blocked on #900)", FILES)
        self.assertEqual(pending.returncode, 0)
        self.assertIn("::warning::migrations-staging:", pending.stdout)
        applied = self.run_cli("Staging migration: " + ", ".join(FILES[:2]) + " — applied 2026-10-30 by a", FILES)
        self.assertEqual(applied.returncode, 0)
        self.assertNotIn("::", applied.stdout)

    def test_unreadable_input_exits_2(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--body-file", "/nonexistent/body", "--files-file", "/nonexistent/files"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertNotIn("Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
