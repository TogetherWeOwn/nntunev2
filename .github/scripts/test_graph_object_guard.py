"""Regression tests for the inert checkout and history guard."""

import json
import os
from pathlib import Path
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
        outputs = [b"false\n", b".git/info/grafts\n", b"H controls/input\0", b"", b"commit\n", b"objects\n"]
        results = [subprocess.CompletedProcess([], 0, output, b"") for output in outputs]
        results.append(subprocess.CompletedProcess([], 1, b"", b"connectivity error"))
        with mock.patch.object(guard.subprocess, "run", side_effect=results):
            with self.assertRaisesRegex(ValueError, "connectivity error"):
                guard.guard(".", ["a" * 40], ["controls"])

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
        return subprocess.check_output(["git", "-C", str(self.root), *args], text=True)

    def test_complete_and_dirty_checkout(self):
        guard.guard(self.root, [self.commit], ["controls"])
        (self.root / "controls" / "input").write_text("changed\n")
        with self.assertRaisesRegex(ValueError, "dirty trusted inputs"):
            guard.guard(self.root, [self.commit], ["controls"])

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
            ["git", "-C", str(self.root), "mktree"], input="", text=True
        ).strip()
        replacement = subprocess.check_output(
            ["git", "-C", str(self.root), "commit-tree", empty_tree],
            input="replacement\n", text=True,
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

    def test_shallow_repository_refused(self):
        (self.root / ".git" / "shallow").write_text(self.commit + "\n")
        with self.assertRaisesRegex(ValueError, "shallow"):
            guard.guard(self.root, [self.commit], ["controls"])


if __name__ == "__main__":
    unittest.main()
