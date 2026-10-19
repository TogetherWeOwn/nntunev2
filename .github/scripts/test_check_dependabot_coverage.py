#!/usr/bin/env python3
"""Self-tests for check_dependabot_coverage.py.

The shipped repo must stay green; deleting any expected entry (repair
cargo/docker, npm, github-actions) must turn the check red, as must an
unlisted new standalone crate in either the `exclude` form or the
own-`[workspace]` form. Structural breaks (`version`/`updates`), a missing
`.gitignore`, stale cargo entries, and uncovered tracked lockfiles are
input errors or drift. Run from the repo root:

  python3 .github/scripts/test_check_dependabot_coverage.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_dependabot_coverage import (
    check_coverage,
    normalize_directory,
    parse_dependabot_entries,
)

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "check_dependabot_coverage.py"

DEPENDABOT = """\
# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.
version: 2
updates:
  - package-ecosystem: "cargo"
    directory: "/"
  - package-ecosystem: "cargo"
    directory: "/workers/repair"
  - package-ecosystem: "docker"
    directory: "/containers/repair"
  - package-ecosystem: "npm"
    directory: "/e2e"
  - package-ecosystem: "github-actions"
    directory: "/"
"""

ROOT_CARGO = """\
[workspace]
members = []
exclude = ["workers/repair"]
"""


def write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def make_fixture(extra_updates=""):
    tmp = Path(tempfile.mkdtemp(prefix="dependabot-coverage-"))
    write(tmp / "Cargo.toml", ROOT_CARGO)
    write(tmp / "Cargo.lock", "lock")
    write(tmp / "workers" / "repair" / "Cargo.toml", "[package]\nname = \"x\"\n")
    write(tmp / "workers" / "repair" / "Cargo.lock", "lock")
    write(tmp / "containers" / "repair" / "Dockerfile", "FROM debian:bookworm-slim\n")
    write(tmp / "e2e" / "package.json", "{}\n")
    (tmp / ".github" / "workflows").mkdir(parents=True)
    write(tmp / ".github" / "dependabot.yml", DEPENDABOT + extra_updates)
    write(tmp / ".gitignore", "Cargo.lock\n!/Cargo.lock\n!/workers/repair/Cargo.lock\n")
    return tmp


def run_check(root):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(root)],
        capture_output=True,
        text=True,
        cwd=str(ROOT),
    )
    return proc.returncode, proc.stdout, proc.stderr


class DependabotCoverageTests(unittest.TestCase):
    def test_shipped_repo_is_green(self):
        drift, err = check_coverage(ROOT)
        self.assertIsNone(err, err)
        self.assertEqual(drift, [], drift)

    def test_shipped_repo_green_via_subprocess(self):
        code, out, _ = run_check(ROOT)
        self.assertEqual(code, 0, out)
        self.assertIn("OK", out)

    def test_fixture_is_green(self):
        tmp = make_fixture()
        try:
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertEqual(drift, [], drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_repair_cargo_entry_fails(self):
        # The exact omission this card fixes: drop the repair cargo entry.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/workers/repair"', 'directory: "/workers/mb"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("/workers/repair" in d for d in drift),
                drift,
            )
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_repair_docker_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/containers/repair"', 'directory: "/containers/mb-import"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("/containers/repair" in d for d in drift),
                drift,
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_new_standalone_crate_without_entry_fails(self):
        # A new excluded crate with no entry must turn the check red.
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace('exclude = ["workers/repair"]',
                                  'exclude = ["workers/repair", "workers/new"]')
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            write(tmp / "workers" / "new" / "Cargo.toml", "[package]\nname = \"y\"\n")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/new" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_own_workspace_crate_without_entry_fails(self):
        # Own-[workspace] form: builds fine, not in exclude, no lockfile.
        # The exclude list alone would miss it; the repo scan must catch it.
        tmp = make_fixture()
        try:
            write(
                tmp / "workers" / "solo" / "Cargo.toml",
                "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n\n[workspace]\n",
            )
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/solo" in d for d in drift), drift)
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_workspace_member_needs_no_entry(self):
        # Members resolve to the root lockfile: no cargo entry required.
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace(
                'members = []', 'members = ["workers/stream"]'
            )
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            write(tmp / "workers" / "stream" / "Cargo.toml", "[package]\nname = \"s\"\n")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertEqual(drift, [], drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_members_glob_is_input_error(self):
        tmp = make_fixture()
        try:
            cargo = (tmp / "Cargo.toml").read_text(encoding="utf-8")
            cargo = cargo.replace('members = []', 'members = ["crates/*"]')
            (tmp / "Cargo.toml").write_text(cargo, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("glob", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_npm_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('directory: "/e2e"', 'directory: "/web"')
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/e2e" in d for d in drift), drift)
            code, out, _ = run_check(tmp)
            self.assertEqual(code, 1, out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_github_actions_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace('- package-ecosystem: "github-actions"\n    directory: "/"', "")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("github-actions" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_stale_cargo_entry_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text += '  - package-ecosystem: "cargo"\n    directory: "/workers/ghost"\n'
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("stale" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_tracked_lockfile_without_entry_fails(self):
        # Lockfile loop: a tracked lock whose dir has no cargo entry fails
        # even when no Cargo.toml advertises the directory.
        tmp = make_fixture()
        try:
            write(tmp / "workers" / "ghost" / "Cargo.lock", "lock")
            gitignore = (tmp / ".gitignore").read_text(encoding="utf-8")
            gitignore += "!/workers/ghost/Cargo.lock\n"
            (tmp / ".gitignore").write_text(gitignore, encoding="utf-8")
            dependabot = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            dependabot = dependabot.replace(
                "# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.",
                "# Lockfiles: Cargo.lock, workers/repair/Cargo.lock and workers/ghost/Cargo.lock.",
            )
            (tmp / ".github" / "dependabot.yml").write_text(dependabot, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("/workers/ghost" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_lockfile_without_negation_fails(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".gitignore").read_text(encoding="utf-8")
            text = text.replace("!/workers/repair/Cargo.lock\n", "")
            (tmp / ".gitignore").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("negat" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_gitignore_is_input_error(self):
        tmp = make_fixture()
        try:
            (tmp / ".gitignore").unlink()
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn(".gitignore", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_malformed_version_is_input_error(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("version: 2", "version: [")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("version", err)
            code, _, err_out = run_check(tmp)
            self.assertEqual(code, 2, err_out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_updates_is_input_error(self):
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("updates:\n", "")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(drift, err)
            self.assertIn("updates", err)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_root_lock_header_needs_word_match(self):
        # Nested lock names all contain `Cargo.lock`: a header naming only
        # the nested lock must not satisfy the root lock.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace(
                "# Lockfiles: Cargo.lock and workers/repair/Cargo.lock.",
                "# Lockfiles: workers/repair/Cargo.lock.",
            )
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(
                any("Cargo.lock" in d and "header" in d for d in drift), drift
            )
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_stale_header_fails(self):
        # A lockfile the header does not name must fail.
        tmp = make_fixture()
        try:
            text = (tmp / ".github" / "dependabot.yml").read_text(encoding="utf-8")
            text = text.replace("workers/repair/Cargo.lock", "workers/api/Cargo.lock")
            (tmp / ".github" / "dependabot.yml").write_text(text, encoding="utf-8")
            drift, err = check_coverage(tmp)
            self.assertIsNone(err, err)
            self.assertTrue(any("header" in d for d in drift), drift)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_entry_parsing_ignores_comments(self):
        text = (
            "# directory: \"/ignored\"\n"
            '- package-ecosystem: "cargo"\n'
            '  directory: "/workers/repair" # trailing\n'
        )
        self.assertEqual(
            parse_dependabot_entries(text), {("cargo", "/workers/repair")}
        )

    def test_normalize_directory(self):
        self.assertEqual(normalize_directory("/"), "/")
        self.assertEqual(normalize_directory("workers/repair/"), "/workers/repair")
        self.assertEqual(normalize_directory("/e2e"), "/e2e")


if __name__ == "__main__":
    unittest.main(verbosity=2)
