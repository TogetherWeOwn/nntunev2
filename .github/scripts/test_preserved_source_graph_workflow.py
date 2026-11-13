"""Check the synthetic graph workflow's intended execution boundary."""

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_ci_pnpm_retry import split_jobs, split_steps, step_field


ROOT = Path(__file__).resolve().parents[2]


class TestPreservedSourceGraphWorkflow(unittest.TestCase):
    def test_head_and_graph_control_are_wired(self):
        text = (ROOT / ".github/workflows/preserved-source-graph.yml").read_text()
        self.assertIn(
            "  pull_request:\n"
            "    types: [opened, synchronize, reopened, ready_for_review, labeled, unlabeled, edited]\n",
            text,
        )
        self.assertNotIn("pull_request_target:", text)
        self.assertIn("permissions:\n  contents: read\n", text)
        jobs = split_jobs(text)
        self.assertEqual({"synthetic-graph"}, set(jobs))
        job = jobs["synthetic-graph"]
        self.assertIn("    timeout-minutes: 5", job)
        steps = split_steps(job)
        self.assertEqual(4, len(steps))
        checkout, identity, wiring, graph = steps
        self.assertEqual("python3 .github/scripts/test_preserved_source_graph_workflow.py -v",
                         step_field(wiring, "run"))
        self.assertEqual("${{ github.sha }}", step_field(checkout, "ref"))
        self.assertEqual("2", step_field(checkout, "fetch-depth"))
        self.assertEqual("false", step_field(checkout, "persist-credentials"))
        for name, expression, rev in (
            ("MERGE", "github.sha", "HEAD"),
            ("BASE", "github.event.pull_request.base.sha", "HEAD^1"),
            ("HEAD", "github.event.pull_request.head.sha", "HEAD^2"),
        ):
            self.assertIn(f"EXPECTED_{name}: ${{{{ {expression} }}}}", identity)
            self.assertIn(f'test "$(git rev-parse {rev})" = "$EXPECTED_{name}"', identity)
        self.assertIn("git --version", identity)
        self.assertIn("GitFixture.test_preserved_source_requires_original_graph_and_data_objects", graph)
        self.assertNotIn("continue-on-error:", job)
        self.assertNotIn("if:", job)

    def test_merge_with_old_head_runs_base_controls_against_head_code(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            scripts = root / ".github/scripts"
            shutil.copytree(ROOT / ".github/scripts", scripts, ignore=shutil.ignore_patterns("__pycache__"))
            wiring = scripts / "test_preserved_source_graph_workflow.py"
            wiring.unlink()
            graph = scripts / "test_migrations_rename_check.py"
            full_graph = graph.read_text()
            start = full_graph.index("    def test_preserved_source_requires_original_graph_and_data_objects(")
            end = full_graph.index("    def test_preserved_source_rejects_mismatched_final_sql(", start)
            graph.write_text(full_graph[:start] + full_graph[end:])

            def git(*args):
                return subprocess.run(["git", "-C", str(root), *args], check=True,
                                      capture_output=True, text=True).stdout.strip()

            git("init", "-q", "-b", "main")
            git("config", "user.email", "fixture@example.invalid")
            git("config", "user.name", "Graph Fixture")
            git("add", ".")
            git("commit", "-qm", "old base")
            git("checkout", "-qb", "old-head")
            implementation = scripts / "migration_history.py"
            implementation.write_text(implementation.read_text() + "\n# old head change\n")
            git("commit", "-qam", "head change")
            old_head = git("rev-parse", "HEAD")
            git("checkout", "-q", "main")
            shutil.copy2(ROOT / ".github/scripts/test_preserved_source_graph_workflow.py", wiring)
            graph.write_text(full_graph)
            git("add", ".")
            git("commit", "-qm", "add controls")
            base = git("rev-parse", "HEAD")
            git("merge", "--no-ff", "-qm", "synthetic merge", old_head)
            self.assertEqual(base, git("rev-parse", "HEAD^1"))
            self.assertEqual(old_head, git("rev-parse", "HEAD^2"))
            self.assertIn("# old head change", implementation.read_text())
            command = [sys.executable, str(graph),
                       "GitFixture.test_preserved_source_requires_original_graph_and_data_objects", "-v"]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn("Ran 1 test", result.stderr)
            source = implementation.read_text()
            implementation.write_text(source.replace("def verify_preserved_source(",
                                                     "def broken_verify_preserved_source(", 1))
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertNotEqual(0, result.returncode)
            self.assertIn("verify_preserved_source", result.stderr)


if __name__ == "__main__":
    unittest.main()
