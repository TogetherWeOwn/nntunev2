"""Check the synthetic graph workflow's intended execution boundary."""

import json
import os
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
        self.assertEqual("0", step_field(checkout, "fetch-depth"))
        self.assertEqual("false", step_field(checkout, "persist-credentials"))
        self.assertIn('python3 -I .github/scripts/migrations_base.py >> "$GITHUB_OUTPUT"', identity)
        self.assertIn("git --version", identity)
        self.assertIn("GitFixture.test_preserved_source_requires_original_graph_and_data_objects", graph)
        self.assertNotIn("continue-on-error:", job)
        self.assertNotIn("if:", job)

    def test_identity_uses_actual_merge_parents(self):
        text = (ROOT / ".github/workflows/preserved-source-graph.yml").read_text()
        identity = split_steps(split_jobs(text)["synthetic-graph"])[1]
        script = identity.split("        run: |\n", 1)[1]
        script = "\n".join(line.removeprefix("          ") for line in script.splitlines())
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            def git(*args):
                return subprocess.run(["git", "-C", str(root), *args], check=True,
                                      capture_output=True, text=True).stdout.strip()

            git("init", "-q", "-b", "main")
            git("config", "user.email", "fixture@example.invalid")
            git("config", "user.name", "Graph Fixture")
            git("commit", "--allow-empty", "-qm", "event base")
            stale = git("rev-parse", "HEAD")
            git("checkout", "-qb", "feature")
            git("commit", "--allow-empty", "-qm", "head")
            head = git("rev-parse", "HEAD")
            git("checkout", "-q", "main")
            git("commit", "--allow-empty", "-qm", "actual base")
            base = git("rev-parse", "HEAD")
            git("merge", "--no-ff", "-qm", "merge", head)
            merge = git("rev-parse", "HEAD")

            scripts = root / ".github/scripts"
            scripts.mkdir(parents=True)
            shutil.copy2(ROOT / ".github/scripts/migrations_base.py", scripts)
            event, output = root / "event.json", root / "output"

            def run(expected_merge=merge, expected_head=head, event_base=stale):
                event.write_text(json.dumps({"pull_request": {
                    "base": {"sha": event_base}, "head": {"sha": expected_head},
                }}))
                output.write_text("")
                env = {**os.environ, "GITHUB_SHA": expected_merge, "GITHUB_REF": "refs/pull/1/merge",
                       "GITHUB_EVENT_NAME": "pull_request", "GITHUB_EVENT_PATH": str(event),
                       "GITHUB_OUTPUT": str(output)}
                return subprocess.run(["bash", "-eo", "pipefail", "-c", script], cwd=root,
                                      env=env, capture_output=True, text=True)

            for event_base in (stale, base):
                result = run(event_base=event_base)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertEqual({"merge_sha": merge, "base_sha": base, "head_sha": head},
                                 dict(line.split("=") for line in output.read_text().splitlines()))
                self.assertIn("git version", result.stdout)
            self.assertNotEqual(0, run(expected_merge=base).returncode)
            self.assertNotEqual(0, run(expected_head=base).returncode)
            git("checkout", "-q", "main^1")
            self.assertNotEqual(0, run(expected_merge=base).returncode)
            git("checkout", "-q", "main")
            # Remove only the fixture's loose first-parent object, not its reference.
            (root / ".git/objects" / base[:2] / base[2:]).unlink()
            self.assertNotEqual(0, run().returncode)

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
