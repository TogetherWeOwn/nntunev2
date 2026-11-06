"""Tests for the migration baseline helper and its use with migrations_gate.

A temporary native git graph models the stale-base failure: an old base
(migration 0030), a main that moved on (0031-0034, grandfathered
without the search_path header) and an unchanged PR branch joined to main
by a native two-parent merge commit.
"""

import hashlib
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
GATE = REPO_ROOT / ".github/scripts/migrations_gate.py"
BASE = REPO_ROOT / ".github/scripts/migrations_base.py"
TIMEOUT = 60
ZERO = "0" * 40
HEADER = "SET search_path = public, pg_catalog;\n"
ALIAS = "0034_admin_ingest_old.sql"


def run(args, cwd, env=None):
    return subprocess.run(
        args, cwd=cwd, env=env, text=True, capture_output=True, check=False, timeout=TIMEOUT
    )


def git(root, *args, input=None):
    result = subprocess.run(
        ["git", "-C", str(root), *args],
        text=True, capture_output=True, check=True, timeout=TIMEOUT, input=input,
    )
    return result.stdout.strip()


def write_manifest(root, targets=None, shas=None, alias=True):
    """Regenerate migrations/manifest.toml from the SQL files present."""
    targets, shas = targets or {}, shas or {}
    chunks = []
    for path in sorted((root / "migrations").glob("*.sql")):
        digest = shas.get(path.name) or hashlib.sha256(path.read_bytes()).hexdigest()
        chunk = f'[[migration]]\nfile = "{path.name}"\ntarget = "{targets.get(path.name, "app")}"\nsha256 = "{digest}"\n'
        if alias and path.name == "0034_admin_ingest.sql":
            chunk += f'\n[[migration.renamed_from]]\nfile = "{ALIAS}"\nsha256 = "{digest}"\n'
        chunks.append(chunk)
    (root / "migrations/manifest.toml").write_text("\n".join(chunks), encoding="utf-8")


class Graph(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.root = root = pathlib.Path(cls._tmp.name)
        git(root, "init", "-q", "-b", "main")
        git(root, "config", "user.email", "base-test@example.invalid")
        git(root, "config", "user.name", "Base Test")
        git(root, "config", "commit.gpgsign", "false")
        (root / "migrations").mkdir()
        (root / "note.txt").write_text("base\n")
        for n in (30,):
            (root / f"migrations/{n:04d}_m{n}.sql").write_text(f"SELECT {n};\n")
        write_manifest(root)
        git(root, "add", "-A")
        git(root, "commit", "-qm", "old base")
        cls.old = git(root, "rev-parse", "HEAD")
        git(root, "checkout", "-qb", "pr")
        (root / "note.txt").write_text("pr change\n")
        git(root, "commit", "-qam", "pr change")
        cls.pr_tip = git(root, "rev-parse", "HEAD")
        git(root, "checkout", "-q", "main")
        for n, name in zip(range(31, 35), ("a", "b", "c", "admin_ingest")):
            (root / f"migrations/{n:04d}_{name}.sql").write_text(f"CREATE TABLE t{n} (id int);\n")
        write_manifest(root)
        git(root, "add", "-A")
        git(root, "commit", "-qm", "main moves on")
        cls.main_tip = git(root, "rev-parse", "HEAD")
        git(root, "checkout", "-q", "--detach")
        git(root, "merge", "-q", "--no-ff", "-m", "merge pr", "pr")
        cls.merge = git(root, "rev-parse", "HEAD")
        assert git(root, "rev-parse", f"{cls.merge}^1") == cls.main_tip
        assert git(root, "rev-parse", f"{cls.merge}^2") == cls.pr_tip

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def setUp(self):
        git(self.root, "checkout", "-qf", "--detach", self.merge)
        git(self.root, "clean", "-fdq")
        git(self.root, "update-ref", "refs/heads/main", self.main_tip)
        git(self.root, "update-ref", "-d", "refs/remotes/origin/main")
        (self.root / ".git/shallow").unlink(missing_ok=True)

    def gate(self, base_ref=None):
        if base_ref is None:
            selected = self.helper(**self.pr_env(MIGRATIONS_PR_HEAD_SHA=self.pr_tip))
            self.assert_base(selected, self.main_tip)
            base_ref = selected.stdout.strip().split("=", 1)[1]
        return run(["python3", str(GATE), "--root", str(self.root), "--base-ref", base_ref], REPO_ROOT)

    def helper(self, **env):
        clean = {k: v for k, v in os.environ.items() if not k.startswith(("GITHUB_", "MIGRATIONS_"))}
        clean.update({k: v for k, v in env.items() if v is not None})
        return run(["python3", str(BASE), "--root", str(self.root)], REPO_ROOT, clean)

    def pr_env(self, **over):
        env = {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": "refs/pull/7/merge", "GITHUB_SHA": self.merge}
        env.update(over)
        return env

    def assert_base(self, result, sha):
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual(f"base_ref={sha}\n", result.stdout)
        self.assertIn(sha, result.stderr)

    def assert_closed(self, result):
        self.assertEqual(2, result.returncode, result.stdout + result.stderr)
        self.assertEqual("", result.stdout)
        self.assertIn("No comparison base was selected", result.stderr)
        self.assertIn("fetch-depth: 0", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def checkout_commit(self, text):
        sha = git(self.root, "hash-object", "-t", "commit", "-w", "--literally", "--stdin", input=text)
        git(self.root, "checkout", "-qf", "--detach", sha)
        return sha

    def commit_text(self, *parents):
        tree = git(self.root, "rev-parse", f"{self.merge}^{{tree}}")
        lines = [f"tree {tree}"] + [f"parent {p}" for p in parents]
        lines += ["author A <a@example.invalid> 1 +0000", "committer A <a@example.invalid> 1 +0000", "", "x", ""]
        return "\n".join(lines)

    def write(self, name, text):
        (self.root / "migrations" / name).write_text(text, encoding="utf-8")


class GateWithBaselineTests(Graph):
    def test_correct_base_passes_and_stale_base_fails(self):
        result = self.helper(**self.pr_env())
        self.assert_base(result, self.main_tip)
        good = self.gate()
        self.assertEqual(0, good.returncode, good.stdout + good.stderr)
        stale = self.gate(self.old)
        self.assertEqual(1, stale.returncode, stale.stdout)
        for name in ("0031_a", "0032_b", "0033_c", "0034_admin_ingest"):
            self.assertIn(f"missing-search-path: {name}.sql", stale.stdout)

    def test_new_migration_header(self):
        self.write("0035_new.sql", "SELECT 35;\n")
        write_manifest(self.root)
        bad = self.gate()
        self.assertEqual(1, bad.returncode, bad.stdout)
        self.assertIn("missing-search-path: 0035_new.sql", bad.stdout)
        self.write("0035_new.sql", HEADER + "SELECT 35;\n")
        write_manifest(self.root)
        ok = self.gate()
        self.assertEqual(0, ok.returncode, ok.stdout + ok.stderr)

    def test_applied_edit_and_remove(self):
        self.write("0031_a.sql", "CREATE TABLE t31 (id bigint);\n")
        write_manifest(self.root)
        edited = self.gate()
        self.assertEqual(1, edited.returncode, edited.stdout)
        self.assertIn("applied-edited: 0031_a.sql", edited.stdout)
        git(self.root, "checkout", "-qf", ".")
        (self.root / "migrations/0031_a.sql").unlink()
        write_manifest(self.root)
        removed = self.gate()
        self.assertEqual(1, removed.returncode, removed.stdout)
        self.assertIn("applied-removed: 0031_a.sql", removed.stdout)

    def test_manifest_target_and_checksum_drift(self):
        write_manifest(self.root, targets={"0032_b.sql": "mirror"})
        target = self.gate()
        self.assertEqual(1, target.returncode, target.stdout)
        self.assertIn("applied-edited: 0032_b.sql: manifest target or checksum", target.stdout)
        write_manifest(self.root, shas={"0032_b.sql": "f" * 64})
        checksum = self.gate()
        self.assertEqual(1, checksum.returncode, checksum.stdout)
        self.assertIn("checksum: 0032_b.sql", checksum.stdout)
        self.assertIn("applied-edited: 0032_b.sql: manifest target or checksum", checksum.stdout)

    def test_rename_history_removal(self):
        write_manifest(self.root, alias=False)
        result = self.gate()
        self.assertEqual(1, result.returncode, result.stdout)
        self.assertIn("rename-history-removed: 0034_admin_ingest.sql", result.stdout)

    def test_checksum_unlisted_and_bad_numbering(self):
        self.write("0035_new.sql", HEADER + "SELECT 35;\n")
        write_manifest(self.root, shas={"0035_new.sql": "a" * 64})
        bad_checksum = self.gate()
        self.assertEqual(1, bad_checksum.returncode, bad_checksum.stderr)
        self.assertIn("checksum: 0035_new.sql", bad_checksum.stdout)
        (self.root / "migrations/manifest.toml").write_text(
            (self.root / "migrations/manifest.toml").read_text().split('[[migration]]\nfile = "0035')[0]
        )
        unlisted = self.gate()
        self.assertEqual(1, unlisted.returncode, unlisted.stderr)
        self.assertIn("unlisted: 0035_new.sql", unlisted.stdout)
        (self.root / "migrations/0035_new.sql").rename(self.root / "migrations/0036_new.sql")
        write_manifest(self.root)
        skipped = self.gate()
        self.assertEqual(1, skipped.returncode, skipped.stdout)
        self.assertIn("out-of-order: 0036_new.sql", skipped.stdout)

    def test_linear_push_before_detects_applied_drift(self):
        self.write("0030_m30.sql", "SELECT 300;\n")
        write_manifest(self.root)
        git(self.root, "commit", "-qam", "edit applied migration")
        tip = git(self.root, "rev-parse", "HEAD")
        result = self.helper(GITHUB_EVENT_NAME="push", GITHUB_SHA=tip,
                             MIGRATIONS_PUSH_BEFORE=self.merge)
        self.assert_base(result, self.merge)
        drift = self.gate(result.stdout.strip().split("=", 1)[1])
        self.assertEqual(1, drift.returncode, drift.stderr)
        self.assertIn("applied-edited: 0030_m30.sql", drift.stdout)

    def test_push_before_detects_drift_when_first_parent_differs(self):
        result = self.helper(GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                             GITHUB_SHA=self.merge, MIGRATIONS_PUSH_BEFORE=self.old)
        self.assert_base(result, self.old)
        self.assertNotEqual(self.main_tip, self.old)  # HEAD^1 would have hidden the drift
        drift = self.gate(self.old)
        self.assertEqual(1, drift.returncode, drift.stdout)
        self.assertIn("missing-search-path: 0031_a.sql", drift.stdout)


class PullRequestBaselineTests(Graph):
    def test_head_sha_matches_second_parent(self):
        self.assert_base(self.helper(**self.pr_env(MIGRATIONS_PR_HEAD_SHA=self.pr_tip)), self.main_tip)

    def test_pr_fails_closed(self):
        other = "1" * 40
        cases = {
            "head mismatch": self.pr_env(MIGRATIONS_PR_HEAD_SHA=other),
            "sha missing": {k: v for k, v in self.pr_env().items() if k != "GITHUB_SHA"},
            "sha short": self.pr_env(GITHUB_SHA=self.merge[:12]),
            "sha nonhex": self.pr_env(GITHUB_SHA="z" * 40),
            "sha not head": self.pr_env(GITHUB_SHA=self.main_tip),
            "ref missing": {k: v for k, v in self.pr_env().items() if k != "GITHUB_REF"},
            "ref branch": self.pr_env(GITHUB_REF="refs/heads/main"),
            "ref head": self.pr_env(GITHUB_REF="refs/pull/7/head"),
            "ref zero": self.pr_env(GITHUB_REF="refs/pull/0/merge"),
            "ref negative": self.pr_env(GITHUB_REF="refs/pull/-1/merge"),
            "ref nonnumeric": self.pr_env(GITHUB_REF="refs/pull/x/merge"),
        }
        for label, env in cases.items():
            with self.subTest(label):
                self.assert_closed(self.helper(**env))

    def test_wrong_parent_count(self):
        for label, parents in (("root", []), ("one", [self.main_tip]),
                               ("octopus", [self.main_tip, self.pr_tip, self.old])):
            with self.subTest(label):
                sha = self.checkout_commit(self.commit_text(*parents))
                self.assert_closed(self.helper(**self.pr_env(GITHUB_SHA=sha)))

    def test_missing_parent_objects(self):
        for label, parents in (("second", [self.main_tip, "2" * 40]), ("first", ["2" * 40, self.pr_tip])):
            with self.subTest(label):
                sha = self.checkout_commit(self.commit_text(*parents))
                self.assert_closed(self.helper(**self.pr_env(GITHUB_SHA=sha)))

    def test_shallow_fails(self):
        (self.root / ".git/shallow").write_text(self.old + "\n")
        self.assert_closed(self.helper(**self.pr_env()))


class OtherEventBaselineTests(Graph):
    def test_missing_event_fails_closed(self):
        self.assert_closed(self.helper())

    def push_env(self, before, sha=None):
        env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_SHA": sha or self.merge}
        if before is not None:
            env["MIGRATIONS_PUSH_BEFORE"] = before
        return env

    def test_push_nonzero_before_is_used(self):
        self.assert_base(self.helper(**self.push_env(self.old)), self.old)
        self.assert_base(self.helper(**self.push_env(self.pr_tip)), self.pr_tip)

    def test_push_before_fails_closed(self):
        tree = git(self.root, "rev-parse", f"{self.merge}^{{tree}}")
        for label, before in (("missing object", "3" * 40), ("invalid", "not-a-sha"), ("short", self.old[:10]),
                              ("tree", tree)):
            with self.subTest(label):
                self.assert_closed(self.helper(**self.push_env(before)))

    def test_fallback_to_main_when_before_zero_or_empty(self):
        for label, before in (("zero", ZERO), ("empty", ""), ("unset", None)):
            with self.subTest(label):
                self.assert_base(self.helper(**self.push_env(before)), self.main_tip)

    def test_other_events_use_main(self):
        for event in ("workflow_dispatch", "merge_group", "schedule"):
            with self.subTest(event):
                env = {"GITHUB_EVENT_NAME": event, "GITHUB_REF": "refs/heads/x", "GITHUB_SHA": self.merge}
                self.assert_base(self.helper(**env), self.main_tip)

    def test_local_main_preferred_then_origin_main(self):
        env = {"GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": "refs/heads/main", "GITHUB_SHA": self.merge}
        git(self.root, "update-ref", "refs/remotes/origin/main", self.old)
        self.assert_base(self.helper(**env), self.main_tip)
        git(self.root, "update-ref", "-d", "refs/heads/main")
        self.assert_base(self.helper(**env), self.old)
        git(self.root, "update-ref", "-d", "refs/remotes/origin/main")
        self.assert_closed(self.helper(**env))

    def test_shallow_fails_for_every_event(self):
        (self.root / ".git/shallow").write_text(self.old + "\n")
        for label, env in (("push", self.push_env(self.old)), ("zero", self.push_env(ZERO)),
                           ("dispatch", {"GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_SHA": self.merge}),
                           ("merge_group", {"GITHUB_EVENT_NAME": "merge_group", "GITHUB_SHA": self.merge})):
            with self.subTest(label):
                self.assert_closed(self.helper(**env))


if __name__ == "__main__":
    unittest.main()
