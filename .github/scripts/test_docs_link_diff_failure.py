#!/usr/bin/env python3
"""Pin docs-link diff-failure exit 2 in docs_link_lint.

``changed_markdown_files`` (docs_link_lint.py) maps a ``git diff``
``OSError``/``CalledProcessError`` after successful base resolve to
``LintError("cannot diff against base ...")``, and ``main()`` maps that to
exit 2. Base-ref resolution is pinned here too: a bogus ref fails closed
with the ref named, and the ``main``-to-``origin/main`` fallback is pinned
(fallback success resolves via ``origin/main``; an exhausted fallback fails
closed). The older ``test_unresolvable_base_ref_fails_closed`` in
``test_docs_link_lint.py`` pins only the exit code; this suite pins the
full failure mapping.

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


FAKE_SHA = "0123456789abcdef0123456789abcdef01234567\n"


def _fake_resolve(main_ok=True, origin_ok=True, diff_stdout=b""):
    """Fake subprocess.run pinning the main->origin/main fallback order.

    Returns the fake and the list of attempted ``rev-parse`` refs, so tests
    can assert ``origin/main`` was tried exactly when ``main`` failed.
    """
    seen = []

    def _run(cmd, **kwargs):
        argv = list(cmd)
        if "rev-parse" in argv:
            ref = argv[-1]
            seen.append(ref)
            if ref == "main^{commit}":
                if not main_ok:
                    raise subprocess.CalledProcessError(128, cmd, "unknown")
                return subprocess.CompletedProcess(cmd, 0, stdout=FAKE_SHA,
                                                   stderr="")
            if ref == "origin/main^{commit}":
                if not origin_ok:
                    raise subprocess.CalledProcessError(128, cmd, "unknown")
                return subprocess.CompletedProcess(cmd, 0, stdout=FAKE_SHA,
                                                   stderr="")
            raise AssertionError(f"unexpected rev-parse ref: {ref}")
        if "diff" in argv:
            return subprocess.CompletedProcess(cmd, 0, stdout=diff_stdout,
                                               stderr="")
        raise AssertionError(f"unexpected git call: {argv}")

    return _run, seen


class TestBaseRefResolution(unittest.TestCase):
    def test_bogus_base_ref_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = _make_changed_repo(tmp)
            bogus = "no-such-ref-xyz"
            with self.assertRaises(LintError) as ctx:
                changed_markdown_files(root, bogus)
            self.assertIn("cannot resolve base ref", str(ctx.exception))
            self.assertIn(bogus, str(ctx.exception))
            rc, out = _run_main(root, bogus)
            self.assertEqual(2, rc)
            self.assertIn("::error::docs-links:", out)
            self.assertIn("cannot resolve base ref", out)
            self.assertIn(bogus, out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_main_falls_back_to_origin_main(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir()
            (root / "docs" / "ok.md").write_text("# Ok\n", encoding="utf-8")
            fake, seen = _fake_resolve(main_ok=False, origin_ok=True,
                                       diff_stdout=b"docs/ok.md\x00")
            with mock.patch.object(docs_link_lint.subprocess, "run",
                                   side_effect=fake):
                self.assertEqual(
                    [Path("docs/ok.md")],
                    changed_markdown_files(root, "main"),
                )
                self.assertEqual(["main^{commit}", "origin/main^{commit}"],
                                 seen)
                rc, out = _run_main(root, "main")
            self.assertEqual(0, rc)
            self.assertIn("scanned 1 files", out)

    def test_main_fallback_exhausted_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake, seen = _fake_resolve(main_ok=False, origin_ok=False)
            with mock.patch.object(docs_link_lint.subprocess, "run",
                                   side_effect=fake):
                with self.assertRaises(LintError) as ctx:
                    changed_markdown_files(root, "main")
                self.assertIn("cannot resolve base ref", str(ctx.exception))
                self.assertIn("'main'", str(ctx.exception))
                self.assertIsInstance(
                    ctx.exception.__cause__, subprocess.CalledProcessError
                )
                self.assertEqual(["main^{commit}", "origin/main^{commit}"],
                                 seen)
                rc, out = _run_main(root, "main")
            self.assertEqual(2, rc)
            self.assertIn("::error::docs-links:", out)
            self.assertIn("cannot resolve base ref", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)


if __name__ == "__main__":
    unittest.main()
