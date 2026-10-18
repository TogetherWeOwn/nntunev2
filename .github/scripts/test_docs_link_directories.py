#!/usr/bin/env python3
"""Pin directory-anchor rejection in docs_link_lint.

A link that targets an existing directory must be fragment-free: any
``#fragment`` (including an empty ``#``) on a directory link is the exact
``anchor on a directory link`` violation (docs_link_lint.py check_source).
The same directory paths without fragments pass, and a valid markdown-file
anchor still passes as the compatibility control.

Uses temporary synthetic Markdown trees and the public scanner functions
only; the scanner and existing fixtures/assertions are unchanged.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from docs_link_lint import check_source, main


def make_root(tmp, files):
    root = Path(tmp)
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def check(root, source):
    return check_source(Path(source), root, {})


class TestDirectoryAnchors(unittest.TestCase):
    def test_subdirectory_fragment_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [adrs](adr/#frag).\n",
                    "docs/adr/b.md": "# B\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            lineno, target, reason = hits[0]
            self.assertEqual(3, lineno)
            self.assertEqual("adr/#frag", target)
            self.assertEqual("anchor on a directory link", reason)

    def test_repo_root_fragment_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "# Title\n\nSee [root](/#frag).\n"},
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            lineno, target, reason = hits[0]
            self.assertEqual(3, lineno)
            self.assertEqual("/#frag", target)
            self.assertEqual("anchor on a directory link", reason)

    def test_empty_fragment_is_present_and_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [adrs](adr/#).\n",
                    "docs/adr/b.md": "# B\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            lineno, target, reason = hits[0]
            self.assertEqual(3, lineno)
            self.assertEqual("adr/#", target)
            self.assertEqual("anchor on a directory link", reason)

    def test_same_directory_paths_without_fragments_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [adrs](adr/) and [root](/).\n",
                    "docs/adr/b.md": "# B\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_valid_markdown_file_anchor_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [ok](b.md#title).\n",
                    "docs/b.md": "# Title\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))


class TestDirectoryFragmentCLI(unittest.TestCase):
    def _run_root(self, root):
        buf = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
            rc = main(["--root", str(root)])
        return rc, buf.getvalue() + err.getvalue()

    def test_cli_exit_1_names_file_and_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [adrs](adr/#frag).\n",
                    "docs/adr/b.md": "# B\n",
                },
            )
            rc, out = self._run_root(root)
            self.assertEqual(1, rc)
            self.assertIn("docs/a.md", out)
            self.assertIn("line=3", out)
            self.assertIn("anchor on a directory link", out)
            self.assertIn("adr/#frag", out)
            # The verdict is the directory-anchor rejection, not a
            # missing file or an unreadable input.
            self.assertNotIn("does not exist", out)
            self.assertNotIn("cannot read", out)

    def test_removing_fragment_yields_exit_0(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\nSee [adrs](adr/#frag).\n",
                    "docs/adr/b.md": "# B\n",
                },
            )
            rc, _ = self._run_root(root)
            self.assertEqual(1, rc)
            (root / "docs" / "a.md").write_text(
                "# Title\n\nSee [adrs](adr/).\n", encoding="utf-8"
            )
            rc, out = self._run_root(root)
            self.assertEqual(0, rc, out)


if __name__ == "__main__":
    unittest.main()
