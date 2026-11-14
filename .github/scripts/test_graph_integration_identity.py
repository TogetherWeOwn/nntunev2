"""Regression tests for the inert local integration identity primitive."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import graph_integration_identity as identity
import graph_object_guard


SUPPORTED = subprocess.run(["git", "--no-lazy-fetch", "--version"],
                           capture_output=True, check=False).returncode == 0


class IdentityTests(unittest.TestCase):
    def test_bad_input_and_guard_failure_stop_construction(self):
        with self.assertRaises(ValueError):
            identity.construct(".", "main", "b" * 40, ["control"])
        with mock.patch.object(identity, "guard", side_effect=ValueError("missing")):
            with mock.patch.object(identity, "_git", return_value="core.bare") as git:
                with self.assertRaisesRegex(ValueError, "missing"):
                    identity.construct(".", "a" * 40, "b" * 40, ["control"])
                self.assertEqual(git.call_count, 1)

    def test_unsafe_git_configuration_is_rejected_before_guard(self):
        for key in ("merge.custom.driver", "merge.default", "filter.foo.smudge",
                    "filter.foo.clean", "filter.foo.process"):
            with self.subTest(key=key), mock.patch.object(identity, "_git", return_value=key):
                with mock.patch.object(identity, "guard") as guard:
                    with self.assertRaisesRegex(ValueError, "unsupported"):
                        identity.construct(".", "a" * 40, "b" * 40, ["control"])
                    guard.assert_not_called()

    def test_readback_rejects_tree_parent_and_checkout_mismatches(self):
        base, head, tree, merge = (c * 40 for c in "abcm")
        # Use valid hexadecimal IDs for all fields.
        merge = "d" * 40
        good = f"tree {tree}\nparent {base}\nparent {head}\nauthor Test\n\nmessage"
        for headers in (f"tree {tree}\nparent {base}\n\nmessage",
                        f"tree {tree}\nparent {head}\nparent {base}\n\nmessage",
                        good.replace("author", f"parent {base}\nauthor"),
                        good.replace(tree, "e" * 40)):
            with self.subTest(headers=headers), mock.patch.object(
                    identity, "_git", side_effect=["commit", headers]):
                with self.assertRaises(ValueError):
                    identity.verify(".", base, head, tree, merge)
        for actual_head, branch in (("e" * 40, "HEAD"), (merge, "main")):
            with self.subTest(actual_head=actual_head, branch=branch), mock.patch.object(
                    identity, "_git", side_effect=["commit", good, actual_head, branch]):
                with self.assertRaises(ValueError):
                    identity.verify(".", base, head, tree, merge)

    def test_git_disables_configured_fsmonitor(self):
        result = subprocess.CompletedProcess([], 0, stdout=b"", stderr=b"")
        with mock.patch.object(identity.subprocess, "run", return_value=result) as run:
            identity._git(".", "checkout", "--detach", "a" * 40)
        args = run.call_args.args[0]
        self.assertIn("core.fsmonitor=false", args)
        self.assertEqual(args[args.index("core.fsmonitor=false") - 1], "-c")

    def test_git_reads_non_ascii_config_keys(self):
        result = subprocess.CompletedProcess([], 0, stdout="branch.révision.remote\0".encode(), stderr=b"")
        with mock.patch.object(identity.subprocess, "run", return_value=result):
            self.assertEqual(identity._git(".", "config", "--list"), "branch.révision.remote\0")

    def test_ci_requires_git_capability(self):
        if os.environ.get("GRAPH_GUARD_REQUIRE_GIT") == "1":
            self.assertTrue(SUPPORTED, "Git must support --no-lazy-fetch")


@unittest.skipUnless(SUPPORTED, "Git lacks --no-lazy-fetch")
class IntegrationTests(unittest.TestCase):
    def setUp(self):
        env = graph_object_guard.git_environment()
        env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        for module in (identity, graph_object_guard):
            patcher = mock.patch.object(module, "git_environment", return_value=env)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.root / "control").write_text("control\n")
        (self.root / "common").write_text("original\n")
        self.git("add", ".")
        self.git("commit", "-qm", "initial")
        self.base = self.git("rev-parse", "HEAD")
        (self.root / "new").write_text("new\n")
        self.git("add", ".")
        self.git("commit", "-qm", "head")
        self.head = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args],
                                       env=graph_object_guard.git_environment(), text=True).strip()

    def test_clean_graph_and_detached_readback(self):
        tree, merge = identity.construct(self.root, self.base, self.head, ["control"])
        self.assertEqual(self.git("rev-parse", "HEAD"), merge)
        self.assertEqual(self.git("rev-parse", "HEAD^{tree}"), tree)
        self.assertEqual(self.git("show", "-s", "--format=%P", merge),
                         self.base + " " + self.head)
        identity.verify(self.root, self.base, self.head, tree, merge)

    def test_dirty_noncontrol_file_is_refused_before_and_after_checkout(self):
        (self.root / "common").write_text("dirty\n")
        with self.assertRaises(ValueError):
            identity.construct(self.root, self.base, self.head, ["control"])
        self.git("checkout", "--", "common")
        tree, merge = identity.construct(self.root, self.base, self.head, ["control"])
        (self.root / "common").write_text("dirty\n")
        with self.assertRaises(ValueError):
            identity.verify(self.root, self.base, self.head, tree, merge)

    def test_configured_worktree_cannot_redirect_checkout(self):
        self.git("checkout", "-q", "--detach", self.base)
        (self.root / "base-only").write_text("base\n")
        self.git("add", "base-only")
        self.git("commit", "-qm", "base change")
        base = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "--detach", self.head)
        with tempfile.TemporaryDirectory() as other:
            self.git("config", "core.worktree", other)
            identity.construct(self.root, base, self.head, ["control"])
            self.assertEqual((self.root / "base-only").read_text(), "base\n")
            self.assertFalse((Path(other) / "base-only").exists())

    def test_configured_fsmonitor_is_not_executed(self):
        with tempfile.TemporaryDirectory() as other:
            marker = Path(other) / "called"
            hook = self.root / ".git" / "fsmonitor-test"
            hook.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 1\n")
            hook.chmod(0o755)
            self.git("config", "core.fsmonitor", str(hook))
            identity.construct(self.root, self.base, self.head, ["control"])
            self.assertFalse(marker.exists())

    def test_included_external_commands_are_refused(self):
        for section in ("merge.custom.driver", "filter.custom.smudge"):
            with self.subTest(section=section), tempfile.TemporaryDirectory() as other:
                marker = Path(other) / "called"
                config = Path(other) / "included-config"
                self.git("config", "--file", str(config), section, f"touch {marker}")
                self.git("config", "include.path", str(config))
                with self.assertRaisesRegex(ValueError, "unsupported"):
                    identity.construct(self.root, self.base, self.head, ["control"])
                self.assertFalse(marker.exists())
                self.assertEqual(self.git("rev-parse", "HEAD"), self.head)
                self.git("config", "--unset", "include.path")

    def test_conflict_stops_without_checkout(self):
        self.git("checkout", "-q", "--detach", self.base)
        (self.root / "common").write_text("conflicting\n")
        self.git("commit", "-qam", "conflicting")
        conflicting = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "--detach", self.base)
        (self.root / "common").write_text("different\n")
        self.git("commit", "-qam", "different")
        different = self.git("rev-parse", "HEAD")
        with self.assertRaises(ValueError):
            identity.construct(self.root, conflicting, different, ["control"])
        self.assertEqual(self.git("rev-parse", "HEAD"), different)


if __name__ == "__main__":
    unittest.main()
