"""Regression tests for the inert checkout and history guard."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import graph_object_guard as guard


SUPPORTED = subprocess.run(
    ["git", "--no-lazy-fetch", "--version"], capture_output=True, check=False
).returncode == 0


class GuardTests(unittest.TestCase):
    def test_unsupported_git_fails_closed(self):
        result = subprocess.CompletedProcess([], 129, b"", b"unsupported option")
        with mock.patch.object(guard.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(ValueError, "unsupported option"):
                guard.guard(".", ["a" * 40], ["controls"])

    def test_connectivity_error_fails_closed(self):
        outputs = [b"false\n", b".git/info/grafts\n", b"H controls/input\0", b"", b"", b"",
                   b"commit\n", b"objects\n"]
        results = [subprocess.CompletedProcess([], 0, output, b"") for output in outputs]
        results.append(subprocess.CompletedProcess([], 1, b"", b"connectivity error"))
        with mock.patch.object(guard.subprocess, "run", side_effect=results):
            with self.assertRaisesRegex(ValueError, "connectivity error"):
                guard.guard(".", ["a" * 40], ["controls"])

    def test_git_environment_discards_overrides_but_keeps_trace(self):
        with mock.patch.dict(os.environ, {"GIT_DIR": "/donor", "GIT_INDEX_FILE": "/index",
                                          "GIT_TRACE2_EVENT": "/trace"}):
            env = guard.git_environment()
        self.assertNotIn("GIT_DIR", env)
        self.assertNotIn("GIT_INDEX_FILE", env)
        self.assertEqual(env["GIT_TRACE2_EVENT"], "/trace")

    def test_capability_required_in_ci(self):
        if os.environ.get("GRAPH_GUARD_REQUIRE_GIT") == "1":
            self.assertTrue(SUPPORTED, "Git must support --no-lazy-fetch")


@unittest.skipUnless(SUPPORTED, "Git lacks --no-lazy-fetch")
class RealRepositoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.root / "controls").mkdir()
        (self.root / "controls" / "input").write_text("trusted\n")
        self.git("add", ".")
        self.git("commit", "-qm", "initial")
        self.commit = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], text=True,
                                       env=guard.git_environment())

    def test_complete_and_dirty_checkout(self):
        guard.guard(self.root, [self.commit], ["controls"])
        (self.root / "controls" / "input").write_text("changed\n")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [self.commit], ["controls"])

    def test_casefold_config_cannot_hide_untracked_input(self):
        self.git("config", "core.ignoreCase", "true")
        guard.guard(self.root, [self.commit], ["controls"])
        (self.root / "controls" / "INPUT").write_text("untracked\n")
        self.assertEqual((self.root / "controls" / "input").read_text(), "trusted\n")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [self.commit], ["controls"])

    def test_configured_worktree_cannot_hide_untracked_input(self):
        with tempfile.TemporaryDirectory() as directory:
            donor = Path(directory) / "donor"
            shutil.copytree(self.root / "controls", donor / "controls")
            guard.guard(self.root, [self.commit], ["controls"])
            self.git("config", "core.worktree", str(donor))
            (self.root / "controls" / "extra").write_text("untracked\n")
            with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
                guard.guard(self.root, [self.commit], ["controls"])

    def test_inspection_does_not_run_clean_filter(self):
        (self.root / ".gitattributes").write_text("controls/input filter=marker\n")
        self.git("add", ".gitattributes")
        self.git("commit", "-qm", "attributes")
        commit = self.git("rev-parse", "HEAD").strip()
        marker = self.root / "marker"
        self.git("config", "filter.marker.clean", f"touch '{marker}'; cat")
        guard.guard(self.root, [commit], ["controls"])
        (self.root / "controls" / "input").write_text("changed contents\n")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [commit], ["controls"])
        self.assertFalse(marker.exists())

    def test_executable_identity_ignores_filemode_config(self):
        executable = self.root / "controls" / "script"
        executable.write_text("script\n")
        executable.chmod(0o755)
        self.git("add", "controls/script")
        self.git("commit", "-qm", "executable")
        commit = self.git("rev-parse", "HEAD").strip()
        self.git("config", "core.filemode", "false")
        guard.guard(self.root, [commit], ["controls"])
        executable.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [commit], ["controls"])

    def test_trusted_file_under_symlinked_directory_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            donor = Path(directory) / "controls"
            shutil.copytree(self.root / "controls", donor)
            guard.guard(self.root, [self.commit], ["controls/input"])
            shutil.rmtree(self.root / "controls")
            (self.root / "controls").symlink_to(donor, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "unsupported trusted input type"):
                guard.guard(self.root, [self.commit], ["controls/input"])

    def test_unmatched_trusted_path_is_refused(self):
        with self.assertRaisesRegex(ValueError, "no tracked files"):
            guard.guard(self.root, [self.commit], ["controls", "absent"])

    def test_clean_filter_cannot_hide_dirty_trusted_bytes(self):
        self.git("config", "filter.normalize.clean", "sed s/evil/good/g")
        (self.root / ".gitattributes").write_text("controls/input filter=normalize\n")
        (self.root / "controls" / "input").write_text("good\n")
        self.git("add", ".gitattributes", "controls/input")
        self.git("commit", "-qm", "configure filter")
        commit = self.git("rev-parse", "HEAD").strip()
        (self.root / "controls" / "input").write_text("evil\n")
        self.assertEqual(self.git("status", "--porcelain", "--", "controls"), "")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [commit], ["controls"])

    def test_fsmonitor_cannot_hide_dirty_trusted_input(self):
        hook = self.root / ".git" / "fake-fsmonitor"
        hook.write_text('#!/bin/sh\nprintf "token\\0"\n')
        hook.chmod(0o755)
        self.git("config", "core.fsmonitor", str(hook))
        self.git("status", "--porcelain")
        self.git("status", "--porcelain")
        (self.root / "controls" / "input").write_text("changed contents\n")
        self.assertEqual(self.git("status", "--porcelain", "--", "controls"), "")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [self.commit], ["controls"])

    def test_suppressed_dirty_trusted_inputs_refused(self):
        for flag, undo in [("--assume-unchanged", "--no-assume-unchanged"),
                           ("--skip-worktree", "--no-skip-worktree")]:
            with self.subTest(flag=flag):
                self.git("update-index", flag, "controls/input")
                (self.root / "controls" / "input").write_text("changed\n")
                self.assertEqual(self.git("status", "--porcelain", "--", "controls"), "")
                with self.assertRaisesRegex(ValueError, "suppressed or unmerged"):
                    guard.guard(self.root, [self.commit], ["controls"])
                self.git("update-index", undo, "controls/input")
                (self.root / "controls" / "input").write_text("trusted\n")

    def test_grafts_cannot_hide_missing_history(self):
        (self.root / "controls" / "input").write_text("second\n")
        self.git("commit", "-qam", "second")
        child = self.git("rev-parse", "HEAD").strip()
        (self.root / ".git" / "info" / "grafts").write_text(child + "\n")
        (self.root / ".git" / "objects" / self.commit[:2] / self.commit[2:]).unlink()
        with self.assertRaisesRegex(ValueError, "grafts can hide history"):
            guard.guard(self.root, [child], ["controls"])

    def test_replacement_cannot_hide_missing_history(self):
        blob = self.git("rev-parse", "HEAD:controls/input").strip()
        empty_tree = subprocess.check_output(
            ["git", "-C", str(self.root), "mktree"], input="", text=True,
            env=guard.git_environment(),
        ).strip()
        replacement = subprocess.check_output(
            ["git", "-C", str(self.root), "commit-tree", empty_tree],
            input="replacement\n", text=True, env=guard.git_environment(),
        ).strip()
        self.git("replace", self.commit, replacement)
        self.git("config", "remote.origin.url", str(self.root / "fetch-must-not-run"))
        self.git("config", "remote.origin.promisor", "true")
        (self.root / ".git" / "objects" / blob[:2] / blob[2:]).unlink()
        trace = self.root / "replacement-trace.json"
        with mock.patch.dict(os.environ, {"GIT_TRACE2_EVENT": str(trace)}):
            with self.assertRaisesRegex(ValueError, "missing reachable object"):
                guard.guard(self.root, [self.commit], ["controls"])
        events = [json.loads(line) for line in trace.read_text().splitlines()]
        self.assertFalse(any("fetch" in event.get("argv", []) for event in events
                             if event.get("event") == "child_start"))

    def test_missing_promisor_blob_refused_without_fetch(self):
        blob = self.git("rev-parse", "HEAD:controls/input").strip()
        self.git("config", "remote.origin.url", str(self.root / "fetch-must-not-run"))
        self.git("config", "remote.origin.promisor", "true")
        (self.root / ".git" / "objects" / blob[:2] / blob[2:]).unlink()
        trace = self.root / "trace.json"
        with mock.patch.dict(os.environ, {"GIT_TRACE2_EVENT": str(trace)}):
            with self.assertRaisesRegex(ValueError, "missing reachable object"):
                guard.guard(self.root, [self.commit], ["controls"])
        events = [json.loads(line) for line in trace.read_text().splitlines()]
        children = [event.get("argv", []) for event in events
                    if event.get("event") == "child_start"]
        self.assertFalse(any("fetch" in argv for argv in children), children)
        self.assertFalse((self.root / ".git" / "objects" / blob[:2] / blob[2:]).exists())

    def test_inherited_repository_overrides_cannot_select_donor(self):
        with tempfile.TemporaryDirectory() as directory:
            donor = Path(directory) / "donor"
            shutil.copytree(self.root, donor)
            (self.root / ".git" / "shallow").write_text(self.commit + "\n")
            with mock.patch.dict(os.environ, {"GIT_DIR": str(donor / ".git"),
                                              "GIT_WORK_TREE": str(donor)}):
                guard.guard(donor, [self.commit], ["controls"])
                with self.assertRaisesRegex(ValueError, "shallow"):
                    guard.guard(self.root, [self.commit], ["controls"])

    def test_shallow_repository_refused(self):
        (self.root / ".git" / "shallow").write_text(self.commit + "\n")
        with self.assertRaisesRegex(ValueError, "shallow"):
            guard.guard(self.root, [self.commit], ["controls"])


if __name__ == "__main__":
    unittest.main()
