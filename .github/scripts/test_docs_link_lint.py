"""Unit tests for docs_link_lint.

Fixture repos under fixtures/docs-links/ are shared with the CI job:
``match`` must pass, ``broken-file`` and ``broken-anchor`` must fail. The
remaining cases build throwaway mini-repos in a temp directory.
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

from docs_link_lint import (
    LintError,
    anchors_in_markdown,
    changed_markdown_files,
    check_source,
    extract_link_targets,
    github_slug,
    is_external,
    main,
    markdown_files,
    resolve_target,
)

SCRIPTS = Path(__file__).resolve().parent
FIXTURES = SCRIPTS / "fixtures" / "docs-links"
REPO_ROOT = SCRIPTS.parents[1]


def make_root(tmp, files):
    root = Path(tmp)
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def check(root, source):
    return check_source(Path(source), root, {})


class TestGitHubSlug(unittest.TestCase):
    def test_plain_heading(self):
        self.assertEqual("environments-matrix", github_slug("Environments matrix"))

    def test_punctuation_is_dropped(self):
        self.assertEqual(
            "blank-readout-copy-from-here",
            github_slug("Blank readout (copy from here)"),
        )

    def test_inline_formatting_reduces_to_text(self):
        # "/" is punctuation to the slugger, so "/api/health" folds together.
        self.assertEqual(
            "the-apihealth-check", github_slug("The `/api/health` check")
        )
        self.assertEqual("a-bold-title", github_slug("A **bold** title"))
        self.assertEqual("a-dunder-word", github_slug("A __dunder__ word"))

    def test_underscores_survive(self):
        self.assertEqual("some_name", github_slug("some_name"))


class TestLinkTargets(unittest.TestCase):
    def test_external_shapes_are_external(self):
        for target in (
            "https://example.com/x",
            "http://example.com/x",
            "mailto:someone@example.com",
            "//example.com/x",
        ):
            self.assertTrue(is_external(target), target)

    def test_internal_shapes_are_not_external(self):
        for target in ("cost-basis.md", "../ops.md#section", "#anchor", "/docs/ops.md"):
            self.assertFalse(is_external(target), target)

    def test_title_after_target_is_dropped(self):
        self.assertEqual(
            ["file.md"], list(extract_link_targets('[t](file.md "title")'))
        )

    def test_angle_target_is_unwrapped(self):
        self.assertEqual(
            ["./file.md"], list(extract_link_targets("[t](<./file.md>)"))
        )

    def test_no_links_yields_nothing(self):
        self.assertEqual([], list(extract_link_targets("just prose here")))


class TestCheckSource(unittest.TestCase):
    def test_valid_relative_file_link_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp, {"docs/a.md": "[b](b.md)\n", "docs/b.md": "# B\n"}
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_broken_file_link_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "[gone](missing.md)\n"})
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not exist", hits[0][2])

    def test_valid_same_file_anchor_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "## Blank readout (copy from here)\n\n"
                              "[go](#blank-readout-copy-from-here)\n"},
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_broken_anchor_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "# Real\n\n[go](#nope)\n"})
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("anchor", hits[0][2])

    def test_valid_cross_file_anchor_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[env](ops.md#environments-matrix)\n",
                    "docs/ops.md": "## Environments matrix\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_broken_cross_file_anchor_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[env](ops.md#no-such-section)\n",
                    "docs/ops.md": "## Environments matrix\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("anchor", hits[0][2])

    def test_repo_root_absolute_link_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "[ops](/docs/ops.md)\n", "docs/ops.md": "# Ops\n"},
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_link_escaping_repo_root_is_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp, {"SECURITY.md": "[report](../../security/advisories/new)\n"}
            )
            self.assertEqual([], check(root, "SECURITY.md"))

    def test_fenced_and_inline_code_links_are_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "```\n[gone](missing.md)\n```\n\n"
                                 "Use `[gone](missing.md)` here.\n"
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_directory_link_passes_without_anchor(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {"docs/a.md": "[adrs](adr/)\n", "docs/adr/b.md": "# B\n"},
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_query_string_is_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[b](b.md?x=1)\n",
                    "docs/b.md": "# B\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_duplicate_headings_get_numbered_slugs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Dup\n\n# Dup\n\n"
                                 "[one](#dup) [two](#dup-1)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_explicit_named_anchor_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[go](#custom-target)\n\n"
                                 '<a name="custom-target"></a>\n',
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_violation_line_numbers_survive_preceding_fences(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "# Title\n\n```md\n[x](y.md)\n```\n\n"
                                 "[gone](missing.md)\n"
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertEqual(7, hits[0][0])

    def test_image_links_are_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "![pic](missing.png)\n"})
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))

    def test_anchor_on_non_markdown_target_is_skipped(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[cfg](app.toml#section)\n",
                    "docs/app.toml": 'x = 1\n',
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))


class TestAnchorMissingDiagnosticPin(unittest.TestCase):
    """Pin the cross-file anchor-missing diagnostic (docs_link_lint.py:369-372).

    Contract under test: a link ``b.md#no-such-heading`` into a target file
    that only defines ``# Real Heading`` yields exactly one violation with
    reason exactly ``anchor does not exist in target file``, rendered by
    main() as
    ``::error file=<rel>,line=<n>::docs-links: <reason>: <target>``; a link
    to the existing ``#real-heading`` stays clean.

    Sensitivity (verified with temporary mutants, restored after): changing
    the reason string fails the exact-reason test, and dropping ``line=``
    from the annotation fails the annotation test.
    """

    BAD_FILES = {
        "docs/a.md": "[bad](b.md#no-such-heading)\n",
        "docs/b.md": "# Real Heading\n",
    }
    GOOD_FILES = {
        "docs/a.md": "[good](b.md#real-heading)\n",
        "docs/b.md": "# Real Heading\n",
    }

    def _run(self, *argv):
        buf = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
            rc = main(list(argv))
        return rc, buf.getvalue() + err.getvalue()

    def test_cross_file_anchor_missing_reason_is_exact(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, self.BAD_FILES)
            hits = check(root, "docs/a.md")
        self.assertEqual(
            [(1, "b.md#no-such-heading", "anchor does not exist in target file")],
            hits,
        )

    def test_cross_file_anchor_missing_annotation_is_exact(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, self.BAD_FILES)
            rc, out = self._run("--root", str(root), "--files", "docs/a.md")
        self.assertEqual(1, rc)
        self.assertIn(
            "::error file=docs/a.md,line=1::docs-links: "
            "anchor does not exist in target file: b.md#no-such-heading",
            out,
        )
        self.assertNotIn("Traceback", out)

    def test_valid_cross_file_anchor_stays_clean(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, self.GOOD_FILES)
            self.assertEqual([], check(root, "docs/a.md"))
            rc, out = self._run("--root", str(root), "--files", "docs/a.md")
        self.assertEqual(0, rc)
        self.assertIn("0 violations", out)


class TestAnchors(unittest.TestCase):
    def test_heading_slugs_and_duplicates(self):
        anchors = anchors_in_markdown("# Dup\n\n## Dup\n\n### `code` head\n")
        self.assertIn("dup", anchors)
        self.assertIn("dup-1", anchors)
        self.assertIn("code-head", anchors)

    def test_resolve_target_parent_navigation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertEqual(
                Path("docs/ops.md"),
                resolve_target(Path("docs/adr/a.md"), "../ops.md", root),
            )
            self.assertIsNone(
                resolve_target(Path("a.md"), "../../outside.md", root)
            )


class TestFenceHandling(unittest.TestCase):
    def test_longer_opener_ignores_inner_fence(self):
        # A ``` line inside a ```` block is content, not a closer: the
        # sample link must stay ignored and the real link must pass.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "````md\n```\n[gone](missing.md)\n```\n````\n\n"
                                 "# Title\n\n[ok](#title)\n",
                    "docs/b.md": "# B\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_closing_fence_with_info_string_is_content(self):
        # ```python inside a ``` block is sample text, not a closer.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "```\n```python\n[gone](missing.md)\n```\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_mismatched_fence_character_is_content(self):
        # ~~~ inside a ``` block never closes it.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "```\n~~~\n[gone](missing.md)\n~~~\n```\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_shorter_closing_fence_is_content(self):
        # A 3-tick line cannot close a 4-tick opener.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "````\n```\n[gone](missing.md)\n````\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_indented_fence_in_list_item_is_ignored(self):
        # A fenced sample nested in a list item (4+ leading spaces) is
        # still code: its sample link must never fail the lint.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "- step\n    - sub\n"
                                 "        ```md\n"
                                 "        [gone](gone.md)\n"
                                 "        ```\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_unclosed_indented_fence_does_not_swallow_rest(self):
        # An indented code block holding a lone fence line is not a fence:
        # without a matching closer it must not swallow the real link below.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Intro\n\n    ```\n    sample\n\n"
                                 "[gone](missing.md)\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not exist", hits[0][2])

    def test_indented_fence_line_does_not_close_column_zero_fence(self):
        # A 4-space-indented fence line cannot close a column-0 fence, so
        # the sample link stays inside the outer block and passes.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "```md\n- item\n\n"
                                 "    ```\n"
                                 "    [gone](gone.md)\n"
                                 "    ```\n"
                                 "```\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_indented_fence_line_with_later_block_does_not_swallow_link(self):
        # An indented code block holding a lone fence line is not a fence
        # even when a fenced block appears further down: the first later
        # fence line belongs to that block, so the broken link between
        # them must still fail.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "Intro\n\n    ```\n    sample\n\n"
                                 "[gone](missing.md)\n\n"
                                 "More prose.\n\n"
                                 "```bash\necho hi\n```\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("does not exist", hits[0][2])

    def test_indented_fence_ignores_info_string_content(self):
        # An info-string line at the same indent inside a real list-item
        # fence is content: the block still closes at its own bare fence.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "- step\n"
                                 "        ```md\n"
                                 "        ```python\n"
                                 "        [gone](gone.md)\n"
                                 "        ```\n\n"
                                 "# Title\n\n[ok](#title)\n",
                },
            )
            self.assertEqual([], check(root, "docs/a.md"))

    def test_inline_triple_backtick_line_is_not_a_fence(self):
        # A backtick fence info string cannot contain a backtick: a line
        # like ```text``` starts inline code, so the real link below is
        # still checked.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "```text```\n\n[gone](missing.md)\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))

    def test_heading_inside_nested_example_is_not_an_anchor(self):
        # strip_fences and code_free_lines must agree: the sample heading
        # is not linkable, so the anchor link fails.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[x](b.md#not-a-heading)\n",
                    "docs/b.md": "````md\n```\n# Not a heading\n```\n````\n\n"
                                 "# Real\n",
                },
            )
            hits = check(root, "docs/a.md")
            self.assertEqual(1, len(hits))
            self.assertIn("anchor", hits[0][2])


class TestMarkdownFileDiscovery(unittest.TestCase):
    def _git_env(self):
        # Drop repository-location variables so git uses the explicit cwd
        # instead of an inherited repository.
        env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1",
                   GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        env.pop("GIT_DIR", None)
        env.pop("GIT_WORK_TREE", None)
        env.pop("GIT_INDEX_FILE", None)
        env.pop("GIT_COMMON_DIR", None)
        return env

    def test_untracked_node_modules_are_excluded(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = self._git_env()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True, env=env)
            make_root(
                tmp,
                {
                    "docs/a.md": "[b](b.md)\n",
                    "docs/b.md": "# B\n",
                    "node_modules/somepkg/README.md": "[gone](missing.md)\n",
                },
            )
            subprocess.run(["git", "add", "docs"], cwd=root, check=True, env=env)
            found = markdown_files(root)
            self.assertEqual([Path("docs/a.md"), Path("docs/b.md")], found)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(root)]))

    def test_uppercase_suffix_tracked_file_is_scanned(self):
        # git pathspecs are case-sensitive: a tracked Guide.MD must still
        # reach the suffix.lower() filter instead of being dropped early.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = self._git_env()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True, env=env)
            make_root(
                tmp,
                {
                    "docs/Guide.MD": "[gone](missing.md)\n",
                    "docs/b.md": "# B\n",
                },
            )
            subprocess.run(["git", "add", "docs"], cwd=root, check=True, env=env)
            found = markdown_files(root)
            self.assertIn(Path("docs/Guide.MD"), found)
            hits = check(root, "docs/Guide.MD")
            self.assertEqual(1, len(hits))

    def test_walk_fallback_skips_vendor_trees_outside_git(self):
        # Throwaway roots outside git (no `git` repo) still skip vendored
        # trees instead of flagging files nobody owns.
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[b](b.md)\n",
                    "docs/b.md": "# B\n",
                    "node_modules/somepkg/README.md": "[gone](missing.md)\n",
                    "target/doc/README.md": "[gone](missing.md)\n",
                },
            )
            found = markdown_files(root)
            self.assertEqual([Path("docs/a.md"), Path("docs/b.md")], found)


class TestChangedScope(unittest.TestCase):
    def test_only_changed_files_are_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1",
                       GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                       GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
            env.pop("GIT_DIR", None)
            env.pop("GIT_WORK_TREE", None)
            env.pop("GIT_INDEX_FILE", None)
            env.pop("GIT_COMMON_DIR", None)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True, env=env)
            # A broken file that stays untouched must never fail the run.
            (root / "docs").mkdir()
            (root / "docs" / "stale.md").write_text("[gone](missing.md)\n")
            (root / "docs" / "fresh.md").write_text("[ok](#fresh)\n\n# Fresh\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True, env=env)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=root,
                           check=True, env=env)
            (root / "docs" / "fresh.md").write_text(
                "[ok](#fresh)\n\n# Fresh\n\nmore prose\n"
            )
            changed = changed_markdown_files(root, "HEAD")
            self.assertEqual([Path("docs/fresh.md")], changed)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(root), "--base-ref", "HEAD"]))

    def test_unresolvable_base_ref_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir()
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    2, main(["--root", str(root), "--base-ref", "no-such-ref"])
                )


class TestMainExitCodes(unittest.TestCase):
    def test_clean_root_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "[b](b.md)\n", "docs/b.md": "# B\n"})
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, main(["--root", str(root)]))

    def test_broken_root_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(tmp, {"docs/a.md": "[gone](missing.md)\n"})
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(1, main(["--root", str(root)]))

    def test_missing_root_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    2, main(["--root", str(Path(tmp) / "nope")])
                )


class TestReadFailures(unittest.TestCase):
    """Pin read-failure exits: unreadable inputs fail closed, never clean.

    Covers check_source source reads and anchor-target reads
    (docs_link_lint.py check_source): OSError and invalid-UTF8 bytes both
    surface as LintError, and main() maps them to exit 1 naming the file
    with no traceback and no clean-scan summary.
    """

    def _run_files(self, root, *files):
        buf = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(err):
            rc = main(["--root", str(root), "--files", *files])
        return rc, buf.getvalue() + err.getvalue()

    def test_source_directory_raises_oserror(self):
        # Directory-as-input where the OS permits: reading a directory as
        # UTF-8 text raises OSError (IsADirectoryError on Linux).
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs" / "a.md").mkdir(parents=True)
            with self.assertRaises(LintError) as ctx:
                check(root, "docs/a.md")
            self.assertIn("docs/a.md", str(ctx.exception))
            self.assertIsInstance(ctx.exception.__cause__, OSError)

    def test_source_oserror_exit_names_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp, {"docs/a.md": "[b](b.md)\n", "docs/b.md": "# B\n"}
            )

            def boom(self, *args, **kwargs):
                raise OSError("boom")

            with mock.patch.object(Path, "read_text", new=boom):
                with self.assertRaises(LintError) as ctx:
                    check(root, "docs/a.md")
                self.assertIn("docs/a.md", str(ctx.exception))
                rc, out = self._run_files(root, "docs/a.md")
            self.assertEqual(1, rc)
            self.assertIn("docs/a.md", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_source_invalid_utf8_exit_names_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir(parents=True)
            (root / "docs" / "a.md").write_bytes(b"# Title\n\xff\xfe bad\n")
            with self.assertRaises(LintError) as ctx:
                check(root, "docs/a.md")
            self.assertIn("docs/a.md", str(ctx.exception))
            rc, out = self._run_files(root, "docs/a.md")
            self.assertEqual(1, rc)
            self.assertIn("docs/a.md", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_anchor_target_invalid_utf8_names_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "docs").mkdir(parents=True)
            (root / "docs" / "a.md").write_text(
                "[x](b.md#sec)\n", encoding="utf-8"
            )
            (root / "docs" / "b.md").write_bytes(b"# Sec\n\xff bad\n")
            with self.assertRaises(LintError) as ctx:
                check(root, "docs/a.md")
            self.assertIn("docs/b.md", str(ctx.exception))
            rc, out = self._run_files(root, "docs/a.md")
            self.assertEqual(1, rc)
            self.assertIn("docs/b.md", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)

    def test_anchor_target_oserror_names_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = make_root(
                tmp,
                {
                    "docs/a.md": "[x](b.md#sec)\n",
                    "docs/b.md": "# Sec\n",
                },
            )
            real_read = Path.read_text

            def selective(self, *args, **kwargs):
                if self.name == "b.md":
                    raise OSError("disk gone")
                return real_read(self, *args, **kwargs)

            with mock.patch.object(Path, "read_text", new=selective):
                with self.assertRaises(LintError) as ctx:
                    check(root, "docs/a.md")
                self.assertIn("docs/b.md", str(ctx.exception))
                rc, out = self._run_files(root, "docs/a.md")
            self.assertEqual(1, rc)
            self.assertIn("docs/b.md", out)
            self.assertNotIn("Traceback", out)
            self.assertNotIn("scanned", out)


class TestFixtureRepos(unittest.TestCase):
    def test_match_fixture_passes(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(0, main(["--root", str(FIXTURES / "match")]))

    def test_broken_file_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                1, main(["--root", str(FIXTURES / "broken-file")])
            )

    def test_broken_anchor_fixture_fails(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(
                1, main(["--root", str(FIXTURES / "broken-anchor")])
            )

    def test_real_repo_docs_are_clean(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = main(["--root", str(REPO_ROOT)])
        self.assertEqual(0, rc, buf.getvalue())

    def test_lint_fixtures_are_excluded_from_full_scan(self):
        self.assertEqual(
            [],
            [
                p
                for p in markdown_files(REPO_ROOT)
                if p.parts[:3] == (".github", "scripts", "fixtures")
            ],
        )


if __name__ == "__main__":
    unittest.main()
