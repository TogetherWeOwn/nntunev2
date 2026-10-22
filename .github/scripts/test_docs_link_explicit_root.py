"""Pin docs-link explicit-file root confinement.

Contract under test (docs_link_lint.py:549-555,571-573): an absolute
--files path outside --root fails closed with exit 2 and
``file is outside --root``; the external file is never scanned and no
clean scanned-files summary is emitted. An existing absolute markdown
file inside root reaches ordinary linting and returns its ordinary
result, so blanket absolute-path rejection cannot pass.

Duplicate boundary: source/target read failures and directory-anchor
rejection are owned by their own pins. Neither owns explicit CLI file
confinement. No checker changes here; this module is tests-only.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from docs_link_lint import main


def make_root(tmp, files):
    root = Path(tmp)
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


class TestExplicitRootConfinement(unittest.TestCase):
    def _run(self, *argv):
        buf = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
            rc = main(list(argv))
        return rc, buf.getvalue() + err.getvalue()

    def test_absolute_path_outside_root_fails_closed(self):
        with tempfile.TemporaryDirectory() as root_tmp:
            with tempfile.TemporaryDirectory() as outside_tmp:
                root = Path(root_tmp)
                (root / "docs").mkdir(parents=True, exist_ok=True)
                outside = Path(outside_tmp) / "outside.md"
                # Broken on purpose: if the external file were scanned the
                # run would report exit 1 with "target file does not exist".
                # Confinement must win with exit 2 instead.
                outside.write_text("[gone](missing.md)\n", encoding="utf-8")
                rc, out = self._run(
                    "--root", str(root), "--files", str(outside)
                )
        self.assertEqual(2, rc)
        self.assertIn("file is outside --root", out)
        self.assertIn(str(outside), out)

    def test_outside_file_is_not_scanned(self):
        with tempfile.TemporaryDirectory() as root_tmp:
            with tempfile.TemporaryDirectory() as outside_tmp:
                root = Path(root_tmp)
                (root / "docs").mkdir(parents=True, exist_ok=True)
                outside = Path(outside_tmp) / "outside.md"
                outside.write_text("[gone](missing.md)\n", encoding="utf-8")
                rc, out = self._run(
                    "--root", str(root), "--files", str(outside)
                )
        self.assertEqual(2, rc)
        # No lint verdict for the external file: neither a violation
        # report nor a clean scanned-files summary may be emitted.
        self.assertNotIn("target file does not exist", out)
        self.assertNotIn("scanned", out)
        self.assertNotIn("Traceback", out)

    def test_absolute_path_inside_root_clean_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp, {"docs/a.md": "[b](b.md)\n", "docs/b.md": "# B\n"}
            )
            inside = root / "docs" / "a.md"
            rc, out = self._run(
                "--root", str(root), "--files", str(inside)
            )
        # Blanket absolute-path rejection would exit 2 here; ordinary
        # linting passes clean with the usual summary.
        self.assertEqual(0, rc)
        self.assertIn("scanned 1 files, 0 violations", out)

    def test_absolute_path_inside_root_broken_fails_ordinary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "[gone](missing.md)\n"})
            inside = root / "docs" / "a.md"
            rc, out = self._run(
                "--root", str(root), "--files", str(inside)
            )
        # Ordinary lint result for a broken file: exit 1 naming the
        # violation, not exit 2 confinement and not exit 0 blanket accept.
        self.assertEqual(1, rc)
        self.assertIn("target file does not exist", out)
        self.assertIn("scanned 1 files, 1 violations", out)


if __name__ == "__main__":
    unittest.main()
