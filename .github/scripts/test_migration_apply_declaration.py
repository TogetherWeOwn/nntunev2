"""Acceptance tests for the staging migration apply declaration check."""

import json
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ".github/scripts/migration_apply_declaration.py"

COMPLETE_APP = """## What changed

- adds a column

## Staging migration apply

- Migration files: `0002_add_column.sql`
- Target (`app`/`mirror`, per `migrations/manifest.toml`): app
- Role: staging migrator
- Who applies: Operator
- Rollback: forward-only; follow-up migration drops the column
- [x] Applied to nntunev2_staging and verified (preflight/verify output linked)

## Secrets affirmation

- [x] No secrets
"""

# The PR template as committed: every field empty, checkbox unticked.
TEMPLATE = (REPO_ROOT / ".github/pull_request_template.md").read_text(encoding="utf-8")


def git(root, *args):
    subprocess.run(
        ["git", "-C", str(root), *args], check=True, capture_output=True, text=True
    )


class MigrationApplyDeclarationTestCase(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp_dir.name) / "repo"
        (self.root / "migrations").mkdir(parents=True)
        git(self.root, "init", "-q", "-b", "main")
        git(self.root, "config", "user.email", "test@example.invalid")
        git(self.root, "config", "user.name", "test")
        self.write("migrations/0001_init.sql", "-- init\n")
        self.write_manifest({"0001_init.sql": "app"})
        self.commit("base")
        git(self.root, "checkout", "-q", "-b", "pr")

    def tearDown(self):
        self.temp_dir.cleanup()

    def write(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def write_manifest(self, targets):
        lines = []
        for filename, target in targets.items():
            lines += ["[[migration]]", f'file = "{filename}"', f'target = "{target}"', 'sha256 = "x"', ""]
        self.write("migrations/manifest.toml", "\n".join(lines))

    def commit(self, message):
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", message)

    def add_migration(self, name="0002_add_column.sql", target="app"):
        self.write(f"migrations/{name}", "-- change\n")
        self.write_manifest({"0001_init.sql": "app", name: target})
        self.commit("migration")

    def run_check(self, body):
        event = pathlib.Path(self.temp_dir.name) / "event.json"
        event.write_text(json.dumps({"pull_request": {"body": body}}), encoding="utf-8")
        return subprocess.run(
            ["python3", SCRIPT, "--root", str(self.root), "--base-ref", "main",
             "--head-ref", "HEAD", "--event", str(event)],
            cwd=REPO_ROOT, text=True, capture_output=True, check=False,
        )

    def assert_fails(self, result, *fragments):
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        for fragment in fragments:
            self.assertIn(fragment, result.stdout)
        for line in result.stdout.splitlines():
            self.assertTrue(line.startswith("::error "), line)

    # --- Required acceptance cases -------------------------------------

    def test_no_migration_touched_passes_silently(self):
        self.write("README.md", "docs only\n")
        self.commit("docs")
        result = self.run_check("")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual("", result.stdout + result.stderr)

    def test_migration_without_section_fails(self):
        self.add_migration()
        result = self.run_check("## What changed\n\n- stuff\n")
        self.assert_fails(result, "no '## Staging migration apply' section", "0002_add_column.sql")

    def test_section_present_but_unchecked_fails(self):
        self.add_migration()
        body = COMPLETE_APP.replace("- [x] Applied", "- [ ] Applied")
        self.assert_fails(self.run_check(body), "missing or unchecked")

    def test_complete_declaration_passes(self):
        self.add_migration()
        result = self.run_check(COMPLETE_APP)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("declaration complete for 0002_add_column.sql", result.stdout)

    # --- Field and diff details -----------------------------------------

    def test_unfilled_template_names_every_missing_field(self):
        self.add_migration()
        self.assert_fails(
            self.run_check(TEMPLATE),
            "missing or empty fields: migration files, target, role, who applies, rollback",
            "missing or unchecked",
        )

    def test_placeholder_values_count_as_empty(self):
        self.add_migration()
        body = COMPLETE_APP.replace("Who applies: Operator", "Who applies: TBD").replace(
            "Role: staging migrator", "Role: <role>"
        )
        self.assert_fails(self.run_check(body), "missing or empty fields: role, who applies")

    def test_modified_migration_requires_declaration(self):
        self.write("migrations/0001_init.sql", "-- edited\n")
        self.commit("edit")
        self.assert_fails(self.run_check(""), "0001_init.sql")

    def test_deleted_or_unnumbered_sql_is_ignored(self):
        self.write("migrations/notes.sql", "-- scratch\n")
        self.write("migrations/nested/0009_x.sql", "-- nested\n")
        self.write("migrations/manifest.toml", "")
        self.commit("misc")
        result = self.run_check("")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual("", result.stdout)

    def test_files_field_must_list_each_touched_migration(self):
        self.add_migration()
        body = COMPLETE_APP.replace("`0002_add_column.sql`", "`0003_other.sql`")
        self.assert_fails(self.run_check(body), "does not list: 0002_add_column.sql")

    def test_target_must_match_manifest(self):
        self.add_migration()
        body = COMPLETE_APP.replace("manifest.toml`): app", "manifest.toml`): mirror")
        self.assert_fails(self.run_check(body), "'Target' must name app")

    def test_checkbox_inside_html_comment_does_not_count(self):
        self.add_migration()
        body = COMPLETE_APP.replace(
            "- [x] Applied to nntunev2_staging and verified (preflight/verify output linked)",
            "<!-- - [x] Applied to nntunev2_staging and verified -->\n"
            "- [ ] Applied to nntunev2_staging and verified (preflight/verify output linked)",
        )
        self.assert_fails(self.run_check(body), "missing or unchecked")

    def test_checkbox_in_later_section_does_not_count(self):
        self.add_migration()
        body = COMPLETE_APP.replace("- [x] Applied", "- [ ] Applied") + (
            "\n## Notes\n\n- [x] Applied to nntunev2_staging and verified\n"
        )
        self.assert_fails(self.run_check(body), "missing or unchecked")

    def test_crlf_body_and_uppercase_x_pass(self):
        self.add_migration()
        body = COMPLETE_APP.replace("- [x] Applied", "- [X] Applied").replace("\n", "\r\n")
        result = self.run_check(body)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_null_body_fails_closed(self):
        self.add_migration()
        self.assert_fails(self.run_check(None), "no '## Staging migration apply' section")

    def test_body_is_never_echoed(self):
        self.add_migration()
        body = "::set-output name=x::pwned\n$(touch /tmp/should-not-exist)\n"
        result = self.run_check(body)
        self.assertEqual(1, result.returncode)
        self.assertNotIn("pwned", result.stdout + result.stderr)
        self.assertNotIn("should-not-exist", result.stdout + result.stderr)

    # --- Mirror target ------------------------------------------------

    def test_mirror_only_needs_fields_but_not_staging_checkbox(self):
        self.add_migration("0002_mb_extra.sql", target="mirror")
        body = (
            COMPLETE_APP.replace("0002_add_column.sql", "0002_mb_extra.sql")
            .replace("manifest.toml`): app", "manifest.toml`): mirror")
            .replace("- [x] Applied", "- [ ] Applied")
        )
        result = self.run_check(body)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_mirror_only_without_section_fails(self):
        self.add_migration("0002_mb_extra.sql", target="mirror")
        self.assert_fails(self.run_check(""), "no '## Staging migration apply' section")

    def test_unlisted_migration_fails_closed_as_app(self):
        self.write("migrations/0002_add_column.sql", "-- change\n")
        self.commit("unlisted")
        body = COMPLETE_APP.replace("manifest.toml`): app", "manifest.toml`): mirror")
        self.assert_fails(self.run_check(body), "'Target' must name app")


class RepositoryTemplateTestCase(unittest.TestCase):
    def test_template_has_every_field_and_the_checkbox(self):
        import importlib.util

        spec = importlib.util.spec_from_file_location("declaration", REPO_ROOT / SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        section = module.extract_section(TEMPLATE)
        self.assertIsNotNone(section)
        fields, checked = module.parse_section(section)
        self.assertEqual(set(module.FIELDS), set(fields))
        self.assertFalse(checked)
        self.assertRegex("\n".join(section), module.CHECKBOX)


if __name__ == "__main__":
    unittest.main()
