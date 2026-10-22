"""Pin docs-link annotation diagnostic strings.

Contract under test (docs_link_lint.py:527-532): per-violation hits render
``::error file=<escaped>,line=<n>::docs-links: <reason>: <target>`` with the
file property escaped for workflow-command routing (``,`` -> ``%2C``,
``:`` -> ``%3A``). The format is load-bearing for file/line routing, and no
existing docs-link suite asserts an exact ``::error`` line.

Uses temporary synthetic repos and the public ``main()`` entry point only;
the scanner and existing fixtures/assertions are unchanged.
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


class TestAnnotationStrings(unittest.TestCase):
    def _run(self, *argv):
        buf = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
            rc = main(list(argv))
        return rc, buf.getvalue() + err.getvalue()

    def test_broken_file_link_renders_exact_annotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/we,ird:page.md": "[gone](missing.md)\n"})
            rc, out = self._run(
                "--root", str(root), "--files", "docs/we,ird:page.md"
            )
        self.assertEqual(1, rc)
        expected = (
            "::error file=docs/we%2Cird%3Apage.md,line=1"
            "::docs-links: target file does not exist: missing.md"
        )
        self.assertIn(expected, out)
        # The raw property value must never reach the annotation: an
        # unescaped comma/colon would split the file/line routing.
        self.assertNotIn("file=docs/we,ird:page.md", out)
        self.assertNotIn("Traceback", out)

    def test_broken_anchor_renders_exact_annotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/we,ird:anchor.md": (
                        "# Title\n\n[bad](target.md#no-such-anchor)\n"
                    ),
                    "docs/target.md": "# Real Heading\n",
                },
            )
            rc, out = self._run(
                "--root", str(root), "--files", "docs/we,ird:anchor.md"
            )
        self.assertEqual(1, rc)
        expected = (
            "::error file=docs/we%2Cird%3Aanchor.md,line=3"
            "::docs-links: anchor does not exist in target file: "
            "target.md#no-such-anchor"
        )
        self.assertIn(expected, out)
        self.assertNotIn("file=docs/we,ird:anchor.md", out)
        self.assertNotIn("Traceback", out)


if __name__ == "__main__":
    unittest.main()
