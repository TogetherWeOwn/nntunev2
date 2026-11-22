"""Exercise the blocking ratchet on pinned workflow trees."""
from pathlib import Path
import subprocess
import tempfile
import unittest

CHECK = Path(__file__).with_name("check_workflow_pr_shas.py")


class WorkflowReadRatchet(unittest.TestCase):
    def test_compares_added_reads_to_the_selected_base(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def run(*args, check=True):
                return subprocess.run(args, cwd=root, check=check, capture_output=True,
                                      text=True, timeout=20)

            def git(*args):
                return run("git", *args).stdout.strip()

            def commit(message, contents):
                workflow.write_text(contents)
                git("add", ".github/workflows")
                git("-c", "commit.gpgsign=false", "commit", "-qm", message)
                return git("rev-parse", "HEAD")

            git("init", "-q")
            git("config", "user.name", "Workflow Fixture")
            git("config", "user.email", "workflow@example.invalid")
            workflow = root / ".github/workflows/sample.yaml"
            workflow.parent.mkdir(parents=True)
            good = "      SHA: ${{ needs.ci-commits.outputs.base_sha }}\n"
            old = "      OLD: ${{ github.event.pull_request.base.sha }}\n"
            prefix = "name: sample\non: pull_request\njobs:\n  sample:\n    env:\n"
            stale = commit("earlier base", prefix + good)
            base = commit("actual first parent", prefix + old)
            tested = commit("resolved consumer with old read", prefix + old + good)
            bad = commit("added raw reads", prefix + old + good
                         + "      BAD: ${{ GitHub['event'].pull_request['HEAD'].sha }}\n"
                         + "    if: >-\n      github.event.pull_request\n        .base.sha != ''\n")

            def compare(against, candidate):
                return run("python3", str(CHECK), "--base", against, "--tested", candidate, check=False)

            self.assertEqual(0, compare(base, tested).returncode)
            self.assertEqual(1, compare(stale, tested).returncode)
            rejected = compare(base, bad)
            self.assertEqual(1, rejected.returncode, rejected.stderr)
            self.assertIn("BAD:", rejected.stderr)
            self.assertIn(".base.sha", rejected.stderr)
            self.assertNotIn("OLD:", rejected.stderr)
            self.assertEqual(2, compare("1" * 40, tested).returncode)
