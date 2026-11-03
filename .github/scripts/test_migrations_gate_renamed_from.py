#!/usr/bin/env python3
"""Pin `renamed_from` manifest-history handling in the migrations gate."""
import contextlib
import hashlib
import io
import os
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migrations_gate


class RenamedFromTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.dir.name)
        self.migrations = self.root / "migrations"
        self.migrations.mkdir()
        (self.migrations / "0003_beta.sql").write_text("-- beta\n", encoding="utf-8")
        self.sha = hashlib.sha256(b"-- beta\n").hexdigest()

    def tearDown(self):
        self.dir.cleanup()

    def run_gate(self, manifest):
        (self.migrations / "manifest.toml").write_text(manifest, encoding="utf-8")
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = migrations_gate.main(["--root", str(self.root)])
        return code, out.getvalue() + err.getvalue()

    def manifest(self, renamed_from=""):
        return ('[[migration]]\nfile = "0003_beta.sql"\ntarget = "app"\n'
                f'sha256 = "{self.sha}"\n{renamed_from}')

    def test_valid_rename_history_passes(self):
        line = f'renamed_from = [{{ file = "0001_beta_old.sql", sha256 = "{self.sha}" }}]\n'
        code, output = self.run_gate(self.manifest(line))
        self.assertEqual(code, 0, output)

    def test_alias_sha_must_equal_entry_sha(self):
        # A rename that also edits bytes would let the staging gate
        # certify content that was never applied under either name.
        line = f'renamed_from = [{{ file = "0001_beta_old.sql", sha256 = "{"c" * 64}" }}]\n'
        code, output = self.run_gate(self.manifest(line))
        self.assertEqual(code, 1, output)
        self.assertIn("bad-rename", output)

    def test_malformed_history_is_a_shape_error(self):
        for line in ('renamed_from = "0001_beta_old.sql"\n',
                     'renamed_from = [{ file = "0001_beta_old.sql" }]\n',
                     'renamed_from = [{ file = "../0001_beta_old.sql", sha256 = "abc" }]\n'):
            code, output = self.run_gate(self.manifest(line))
            self.assertEqual(code, 2, output)

    def test_alias_colliding_with_current_file_fails(self):
        line = f'renamed_from = [{{ file = "0003_beta.sql", sha256 = "{self.sha}" }}]\n'
        code, output = self.run_gate(self.manifest(line))
        self.assertEqual(code, 1, output)
        self.assertIn("bad-rename", output)

    def test_bad_alias_sha_fails(self):
        line = 'renamed_from = [{ file = "0001_beta_old.sql", sha256 = "ABC" }]\n'
        code, output = self.run_gate(self.manifest(line))
        self.assertEqual(code, 1, output)
        self.assertIn("bad-rename", output)


if __name__ == "__main__":
    unittest.main()
