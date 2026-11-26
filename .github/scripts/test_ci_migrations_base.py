"""Pin and execute the migration selector -> output -> gate workflow contract."""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from test_ci_pnpm_retry import (
    check_required_step, job_needs, split_jobs, split_steps, step_field, step_level_field,
)


ROOT = Path(__file__).resolve().parents[2]
SELECT = 'python3 -I .github/scripts/migrations_base.py >> "$GITHUB_OUTPUT"'
RATCHET = 'python3 .github/scripts/check_workflow_pr_shas.py --base "$BASE_SHA" --tested "$MERGE_SHA"'
GATE = 'python3 .github/scripts/migrations_gate.py --root . --base-ref "$MIGRATIONS_BASE_REF"'


class TestMigrationWorkflow(unittest.TestCase):
    def setUp(self):
        self.jobs = split_jobs((ROOT / ".github/workflows/ci.yml").read_text())
        self.job = self.jobs["migrations-gate"]
        self.steps = split_steps(self.job)

    def test_selector_output_is_required_gate_input(self):
        job = self.jobs["ci-commits"]
        for command in (SELECT, RATCHET,
                        "python3 -m unittest discover -s .github/scripts -p test_workflow_pr_shas.py"):
            self.assertEqual([], check_required_step("ci-commits", job, command))
        for command in (GATE, "python3 .github/scripts/test_migrations_base.py",
                        "python3 .github/scripts/test_ci_migrations_base.py"):
            self.assertEqual([], check_required_step("migrations-gate", self.job, command))
        self.assertTrue({"migrations-gate", "docs-links", "ci-commits"} <= set(job_needs(self.jobs["ci-ok"])))
        for name in ("docs-links", "migrations-gate"):
            self.assertIn("ci-commits", job_needs(self.jobs[name]))
            self.assertIn("${{ needs.ci-commits.outputs.base_sha }}", self.jobs[name])
        steps = split_steps(job)
        checkout_steps = [
            step for step in steps
            if (step_level_field(step, "uses") or "").startswith("actions/checkout@")
        ]
        self.assertEqual(1, len(checkout_steps), "retain one integration checkout")
        checkout = checkout_steps[0]
        selector = next(step for step in steps if step_level_field(step, "run") == SELECT)
        ratchet = next(step for step in steps if step_level_field(step, "run") == RATCHET)
        self.assertLess(steps.index(checkout), steps.index(selector))
        self.assertLess(steps.index(selector), steps.index(ratchet))
        self.assertEqual("0", step_field(checkout, "fetch-depth"))
        self.assertIsNone(step_field(checkout, "ref"), "retain the default integration checkout")
        self.assertEqual("commits", step_field(selector, "id"))
        self.assertEqual("${{ steps.commits.outputs.base_sha }}", step_field(ratchet, "BASE_SHA"))
        self.assertEqual("${{ steps.commits.outputs.merge_sha }}", step_field(ratchet, "MERGE_SHA"))

    def test_real_workflow_commands_pass_selected_base_and_propagate_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            scripts = root / ".github/scripts"
            scripts.mkdir(parents=True)
            for name in ("migrations_base.py", "migrations_gate.py", "migration_history.py"):
                shutil.copyfile(ROOT / ".github/scripts" / name, scripts / name)

            def run(*args, **kwargs):
                return subprocess.run(args, cwd=root, text=True, capture_output=True,
                                      timeout=30, **kwargs)

            def git(*args):
                return run("git", *args, check=True).stdout.strip()

            def commit(message):
                git("add", ".")
                git("-c", "commit.gpgsign=false", "commit", "-qm", message)
                return git("rev-parse", "HEAD")

            def migration(content):
                path = root / "migrations/0001_base.sql"
                path.parent.mkdir(exist_ok=True)
                path.write_text(content)
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                (path.parent / "manifest.toml").write_text(
                    '[[migration]]\nfile = "0001_base.sql"\ntarget = "app"\n'
                    f'sha256 = "{digest}"\n'
                )

            git("init", "-q", "-b", "main")
            git("config", "user.name", "Migration Test")
            git("config", "user.email", "migration-test@example.invalid")
            migration("SELECT 1;\n")
            base = commit("base")
            git("checkout", "-qb", "change")
            migration("SELECT 2;\n")
            pr_head = commit("edit applied migration")
            git("checkout", "-q", "main")
            git("-c", "commit.gpgsign=false", "merge", "--no-ff", "-qm", "integration", "change")
            merge = git("rev-parse", "HEAD")
            event = root / "event.json"
            event.write_text(json.dumps({"pull_request": {"head": {"sha": pr_head}}}))
            env = {**os.environ, "GITHUB_EVENT_NAME": "pull_request",
                   "GITHUB_REF": "refs/pull/1/merge", "GITHUB_SHA": merge,
                   "GITHUB_EVENT_PATH": str(event), "GITHUB_OUTPUT": str(root / "output")}
            selector = next(step_field(step, "run") for step in split_steps(self.jobs["ci-commits"])
                            if step_field(step, "id") == "commits")
            result = run("bash", "-eo", "pipefail", "-c", selector, env=env)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn(f"checkout={merge} base={base}", result.stderr)
            outputs = (root / "output").read_text()
            value = dict(line.split("=", 1) for line in outputs.splitlines())["base_sha"]
            self.assertEqual(base, value)
            gate = next(step_field(step, "run") for step in self.steps
                        if "migrations_gate.py" in (step_field(step, "run") or ""))
            result = run("bash", "-eo", "pipefail", "-c", gate,
                         env={**env, "MIGRATIONS_BASE_REF": value})
            self.assertEqual(1, result.returncode, result.stdout + result.stderr)
            self.assertIn("applied-edited", result.stdout)
            result = run("bash", "-eo", "pipefail", "-c", selector,
                         env={**env, "GITHUB_SHA": base})
            self.assertEqual(2, result.returncode, result.stderr)
            # A failed selector must never append a fallback output.
            self.assertEqual(outputs, (root / "output").read_text())


if __name__ == "__main__":
    unittest.main()
