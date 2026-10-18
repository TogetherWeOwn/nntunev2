"""Pin changed-path decoding failures in docs_link_lint.

Changed-file mode decodes NUL-delimited ``git diff`` path bytes as UTF-8
(``docs_link_lint.changed_markdown_files``): an undecodable ``.md``
pathname must fail closed (``LintError`` / exit 2), never be skipped
like full-discovery non-UTF8 entries and never be scanned as mojibake.

The diff bytes below are supplied directly (``-z`` NUL-delimited) so the
pin never depends on locale-specific git quoting (``core.quotepath``).
Only the ``subprocess.run`` result is mocked, with ``rev-parse``
(base-ref resolution) and ``diff`` distinguished by argv.
"""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from docs_link_lint import LintError, changed_markdown_files, main

# Invalid-UTF8 .md pathname: 0xFF never appears in valid UTF-8, while the
# trailing ".md" keeps the entry recognisably a markdown changed path.
BAD_DIFF = b"docs/bad-\xff.md\x00"
FAKE_COMMIT = "0123456789abcdef0123456789abcdef01234567\n"


def _fake_git(diff_stdout):
    """Fake subprocess.run distinguishing base resolution from the diff."""

    def _run(cmd, **kwargs):
        argv = list(cmd)
        if "rev-parse" in argv:
            # Base-ref resolution runs in text mode in the checker.
            return subprocess.CompletedProcess(
                cmd, 0, stdout=FAKE_COMMIT, stderr=""
            )
        if "diff" in argv:
            return subprocess.CompletedProcess(cmd, 0, stdout=diff_stdout, stderr="")
        raise AssertionError(f"unexpected git call: {argv}")

    return _run


def _run_main(root, *args):
    buf = io.StringIO()
    err = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
        rc = main(["--root", str(root), *args])
    return rc, buf.getvalue() + err.getvalue()


class TestChangedPathDecoding(unittest.TestCase):
    def test_changed_markdown_files_raises_on_invalid_utf8(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with mock.patch(
                "docs_link_lint.subprocess.run",
                side_effect=_fake_git(BAD_DIFF),
            ):
                with self.assertRaises(LintError) as ctx:
                    changed_markdown_files(root, "HEAD")
            self.assertIn("cannot decode changed path", str(ctx.exception))
            self.assertIsInstance(ctx.exception.__cause__, UnicodeDecodeError)

    def test_main_returns_2_without_traceback_or_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with mock.patch(
                "docs_link_lint.subprocess.run",
                side_effect=_fake_git(BAD_DIFF),
            ):
                rc, out = _run_main(root, "--base-ref", "HEAD")
            self.assertEqual(2, rc)
            self.assertIn("cannot decode changed path", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_valid_utf8_control_reaches_ordinary_linting(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir(parents=True)
            (root / "docs" / "ok.md").write_text("# Ok\n", encoding="utf-8")
            with mock.patch(
                "docs_link_lint.subprocess.run",
                side_effect=_fake_git(b"docs/ok.md\x00"),
            ):
                self.assertEqual(
                    [Path("docs/ok.md")],
                    changed_markdown_files(root, "HEAD"),
                )
                rc, out = _run_main(root, "--base-ref", "HEAD")
            self.assertEqual(0, rc)
            self.assertIn("scanned 1 files", out)


if __name__ == "__main__":
    unittest.main()
