#!/usr/bin/env python3
"""Pin docs-link diff-failure exit 2 in docs_link_lint.

``changed_markdown_files`` (docs_link_lint.py) maps a ``git diff``
``OSError``/``CalledProcessError`` after successful base resolve to
``LintError("cannot diff against base ...")``, and ``main()`` maps that to
exit 2. Only the resolve failure is pinned elsewhere
(``test_unresolvable_base_ref_fails_closed``); this suite pins the diff
failure arm with a throwaway git repo plus a mocked ``subprocess.run``
diff raising ``CalledProcessError``.

Uses temporary synthetic repos and the public scanner entry points only;
the scanner and existing fixtures/assertions are unchanged.
"""

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import docs_link_lint
from docs_link_lint import LintError, changed_markdown_files, main


def _git_env():
    env = dict(
        os.environ,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@t",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@t",
    )
    env.pop("GIT_DIR", None)
    env.pop("GIT_WORK_TREE", None)
    env.pop("GIT_INDEX_FILE", None)
    env.pop("GIT_COMMON_DIR", None)
    return env


def _make_changed_repo(tmp):
    """Init a throwaway repo with one changed markdown file vs HEAD."""
    root = Path(tmp)
    env = _git_env()
    subprocess.run(["git", "init", "-q"], cwd=root, check=True, env=env)
    (root / "docs").mkdir()
    (root / "docs" / "fresh.md").write_text("[ok](#fresh)\n\n# Fresh\n")
    subprocess.run(["git", "add", "."], cwd=root, check=True, env=env)
    subprocess.run(["git", "commit", "-qm", "base"], cwd=root, check=True, env=env)
    (root / "docs" / "fresh.md").write_text(
        "[ok](#fresh)\n\n# Fresh\n\nmore prose\n"
    )
    return root


def _run_main(root, base_ref):
    buf = io.StringIO()
    err = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
        rc = main(["--root", str(root), "--base-ref", base_ref])
    return rc, buf.getvalue() + err.getvalue()


class TestDiffFailure(unittest.TestCase):
    def test_diff_called_process_error_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = _make_changed_repo(tmp)
            real_run = subprocess.run

            def fake_run(argv, *args, **kwargs):
                if "diff" in argv:
                    raise subprocess.CalledProcessError(128, argv, "boom")
                return real_run(argv, *args, **kwargs)

            with mock.patch.object(docs_link_lint.subprocess, "run", side_effect=fake_run):
                with self.assertRaises(LintError) as ctx:
                    changed_markdown_files(root, "HEAD")
                self.assertIn("cannot diff against base", str(ctx.exception))
                self.assertIsInstance(
                    ctx.exception.__cause__, subprocess.CalledProcessError
                )
                rc, out = _run_main(root, "HEAD")
            self.assertEqual(2, rc)
            self.assertIn("cannot diff against base", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_valid_diff_control_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = _make_changed_repo(tmp)
            changed = changed_markdown_files(root, "HEAD")
            self.assertEqual([Path("docs/fresh.md")], changed)
            rc, _ = _run_main(root, "HEAD")
            self.assertEqual(0, rc)


if __name__ == "__main__":
    unittest.main()
