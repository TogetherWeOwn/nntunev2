#!/usr/bin/env python3
"""Coverage for the dormant Worker oracle staging validator."""
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_oracle_inputs as inputs


class InputValidation(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.root = self.base / "inputs"
        self.root.mkdir()
        (self.root / "source").mkdir()
        self.file = self.root / "source" / "file.txt"
        self.file.write_bytes(b"data")
        self.root.chmod(0o700)
        self.file.parent.chmod(0o700)
        self.file.chmod(0o600)
        self.output = self.base / "output"

    def validate(self):
        return inputs.validate_inputs(self.root, self.output)

    def test_valid_tree(self):
        self.assertEqual(self.validate(), (3, 4))

    def test_unrelated_and_secret_names(self):
        for name in ("unrelated", ".git", "credentials", "access-token.json", "private.key", ".env"):
            with self.subTest(name=name):
                path = self.root / name if name == "unrelated" else self.file.parent / name
                path.write_text("x")
                with self.assertRaises(ValueError):
                    self.validate()
                path.unlink()

    def test_links_and_special_files(self):
        for create in (lambda p: p.symlink_to(self.file),
                       lambda p: os.link(self.file, p), os.mkfifo):
            with self.subTest(create=create):
                path = self.file.parent / "extra"
                create(path)
                with self.assertRaises(ValueError):
                    self.validate()
                path.unlink()

    def test_permissions_and_extended_attributes(self):
        self.file.chmod(0o666)
        with self.assertRaises(ValueError):
            self.validate()
        self.file.chmod(0o644)
        with patch.object(inputs.os, "listxattr", return_value=["user.extra"]):
            with self.assertRaises(ValueError):
                self.validate()

    def test_limits_and_overlap(self):
        with patch.object(inputs, "MAX_ENTRIES", 2), self.assertRaises(ValueError):
            self.validate()
        with patch.object(inputs, "MAX_BYTES", 3), self.assertRaises(ValueError):
            self.validate()
        for output in (self.root, self.root / "output", self.base):
            with self.subTest(output=output), self.assertRaises(ValueError):
                inputs.validate_inputs(self.root, output)
        (self.base / "alias").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            inputs.validate_inputs(self.base / "alias", self.output)


if __name__ == "__main__":
    unittest.main()
