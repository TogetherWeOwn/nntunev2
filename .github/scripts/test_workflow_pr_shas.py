"""Exercise the blocking ratchet on pinned workflow trees."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

CHECK = Path(__file__).with_name("check_workflow_pr_shas.py")
FIXTURES = CHECK.parent / "fixtures/workflow-pr-shas"


class WorkflowReadRatchet(unittest.TestCase):
    def test_compares_added_reads_to_the_selected_base(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def run(*args, check=True):
                return subprocess.run(args, cwd=root, check=check, capture_output=True,
                                      text=True, timeout=20)

            def git(*args):
                return run("git", *args).stdout.strip()

            def commit(case):
                workflow.write_bytes((FIXTURES / f"{case}.yaml").read_bytes())
                git("add", ".github/workflows")
                git("-c", "commit.gpgsign=false", "commit", "-qm", case)
                return git("rev-parse", "HEAD")

            git("init", "-q")
            git("config", "user.name", "Workflow Fixture")
            git("config", "user.email", "workflow@example.invalid")
            workflow = root / ".github/workflows/sample.yaml"
            workflow.parent.mkdir(parents=True)
            stale, base, renamed, tested, moved, bad = map(commit, ("stale", "base", "renamed", "resolved", "moved", "bad"))

            def compare(against, candidate):
                return run(sys.executable, str(CHECK), "--base", against, "--tested", candidate, check=False)

            self.assertEqual(0, compare(base, renamed).returncode)
            self.assertEqual(0, compare(base, tested).returncode)
            self.assertEqual(1, compare(stale, tested).returncode)
            self.assertEqual(1, compare(base, moved).returncode)
            rejected = compare(base, bad)
            self.assertEqual(1, rejected.returncode, rejected.stderr)
            self.assertIn("['HEAD'].sha", rejected.stderr)
            self.assertIn(".base.sha", rejected.stderr)
            self.assertNotIn("github.event.pull_request.base.sha", rejected.stderr)
            self.assertEqual(2, compare("1" * 40, tested).returncode)
