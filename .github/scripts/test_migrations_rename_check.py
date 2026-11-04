#!/usr/bin/env python3
"""Tests for the per-PR migration rename-history check."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migrations_rename_check as check_module

MANIFEST = f"""
[[migration]]
file = "0003_beta.sql"
target = "app"
sha256 = "{'b' * 64}"
renamed_from = [{{ file = "0001_beta_old.sql", sha256 = "{'c' * 64}" }}]
"""


class RenameCheckTests(unittest.TestCase):
    def check(self, lines, manifest=MANIFEST):
        return check_module.check(lines, manifest)

    def test_no_renames_pass(self):
        for lines in ([], ["workers/api/src/lib.rs\t\n"],
                      ["migrations/0003_beta.sql\t\n"]):
            ok, _ = self.check(lines)
            self.assertTrue(ok)

    def test_recorded_rename_passes(self):
        ok, message = self.check(
            ["migrations/0003_beta.sql\tmigrations/0001_beta_old.sql"])
        self.assertTrue(ok, message)

    def test_unrecorded_rename_fails(self):
        ok, message = self.check(
            ["migrations/0003_beta.sql\tmigrations/0002_beta.sql"])
        self.assertFalse(ok)
        self.assertIn("0002_beta.sql -> 0003_beta.sql", message)

    def test_rename_of_unlisted_file_fails(self):
        ok, _ = self.check(["migrations/0009_new.sql\tmigrations/0001_beta_old.sql"])
        self.assertFalse(ok)

    def test_non_migration_renames_are_ignored(self):
        ok, _ = self.check(["docs/new.md\tdocs/old.md",
                            "migrations/README.md\tmigrations/GUIDE.md"])
        self.assertTrue(ok)

    def test_unparseable_manifest_fails_closed(self):
        ok, _ = self.check(["migrations/0003_beta.sql\tmigrations/0001_beta_old.sql"],
                           manifest="[[[")
        self.assertFalse(ok)


if __name__ == "__main__":
    unittest.main()
