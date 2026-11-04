"""Pin and execute the migration selector -> output -> gate workflow contract."""

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from test_ci_pnpm_retry import (
    check_required_step, job_needs, split_jobs, split_steps, step_field,
)


ROOT = Path(__file__).resolve().parents[2]
SELECT = 'python3 .github/scripts/migrations_base.py --root . >> "$GITHUB_OUTPUT"'
GATE = 'python3 .github/scripts/migrations_gate.py --root . --base-ref "$MIGRATIONS_BASE_REF"'


class TestMigrationWorkflow(unittest.TestCase):
    def setUp(self):
        self.jobs = split_jobs((ROOT / ".github/workflows/ci.yml").read_text())
        self.job = self.jobs["migrations-gate"]
        self.steps = split_steps(self.job)

    def test_selector_output_is_required_gate_input(self):
        for command in (SELECT, GATE,
                        "python3 .github/scripts/test_migrations_base.py",
                        "python3 .github/scripts/test_ci_migrations_base.py"):
            self.assertEqual([], check_required_step("migrations-gate", self.job, command))
        self.assertIn("migrations-gate", job_needs(self.jobs["ci-ok"]))
        checkout = next(step for step in self.steps if "actions/checkout@" in step)
        self.assertEqual("0", step_field(checkout, "fetch-depth"))
        self.assertIsNone(step_field(checkout, "ref"), "retain the default integration checkout")
        selector = next(step for step in self.steps if step_field(step, "run") == SELECT)
        gate = next(step for step in self.steps if step_field(step, "run") == GATE)
        self.assertEqual("migrations-base", step_field(selector, "id"))
        self.assertLess(self.steps.index(checkout), self.steps.index(selector))
        self.assertLess(self.steps.index(selector), self.steps.index(gate))
        self.assertIn("        env:\n          MIGRATIONS_BASE_REF: ${{ steps.migrations-base.outputs.base_ref }}", gate)
        self.assertNotIn("github.event.pull_request.base.sha", self.job)
        self.assertIn("MIGRATIONS_PR_HEAD_SHA: ${{ github.event.pull_request.head.sha }}", self.job)
        self.assertIn("MIGRATIONS_PUSH_BEFORE: ${{ github.event.before }}", self.job)

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
            env = {**os.environ, "GITHUB_EVENT_NAME": "pull_request",
                   "GITHUB_REF": "refs/pull/1/merge", "GITHUB_SHA": merge,
                   "MIGRATIONS_PR_HEAD_SHA": pr_head, "GITHUB_OUTPUT": str(root / "output")}
            selector = next(step_field(step, "run") for step in self.steps
                            if step_field(step, "id") == "migrations-base")
            result = run("bash", "-eo", "pipefail", "-c", selector, env=env)
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn(f"checkout={merge} base={base}", result.stderr)
            key, value = (root / "output").read_text().strip().split("=", 1)
            self.assertEqual("base_ref", key)
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
            self.assertEqual(f"base_ref={base}\n", (root / "output").read_text())


if __name__ == "__main__":
    unittest.main()
