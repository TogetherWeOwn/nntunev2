"""Public-only PR caller wiring and executable ci-ok verdict tests.

Keep the caller in the existing graph. Only this job may skip on non-PR
events. PR code suites may skip only with a complete, explicit coverage plan.
Stdlib plus CI's existing jq.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import textwrap
import unittest

from test_ci_pnpm_retry import (
    check_required_step,
    job_level_field,
    non_plain_key_lines,
    split_jobs,
    split_steps,
    step_level_field,
)

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
COMMAND = "python3 .github/scripts/test_ci_public_pr_security.py"
PIN = "c05610872587137c3c492b7025f02585a1b8ba7d"
GUARD = "github.event_name == 'pull_request' && github.event.repository.private == false"


class PublicPrSecurityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = WORKFLOW.read_text(encoding="utf-8")
        cls.jobs = split_jobs(cls.text)
        cls.aggregate = cls.jobs["ci-ok"]
        cls.script = textwrap.dedent(re.search(
            r"^      - run: \|\n(.*?)^        env:",
            cls.aggregate, re.MULTILINE | re.DOTALL,
        ).group(1))
        cls.needs = [item.strip() for item in
                     job_level_field(cls.aggregate, "needs").strip("[]").split(",")]

    def verdict(self, event, overrides=None, raw=None, skip="[]", complete="true"):
        needs = {job: {"result": "success"} for job in self.needs}
        needs["public-pr-security"]["result"] = (
            "success" if event == "pull_request" else "skipped"
        )
        for job, result in (overrides or {}).items():
            needs[job]["result"] = result
        env = {"PATH": os.environ["PATH"], "CI_EVENT_NAME": event,
               "CI_SKIP": skip, "CI_COVERAGE_COMPLETE": complete,
               "NEEDS_JSON": json.dumps(needs) if raw is None else raw}
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c", self.script], env=env,
            capture_output=True, text=True, timeout=10,
        )

    def test_pr_only_public_caller_and_nested_permissions(self):
        caller = self.jobs["public-pr-security"]
        self.assertEqual(job_level_field(caller, "if"), GUARD)
        self.assertEqual(job_level_field(caller, "uses"),
                         "TogetherWeOwn/.github/.github/workflows/public-pr-security.yml@" + PIN)
        self.assertRegex(caller, rf"(?m)^    uses: .+@{PIN} # v0\.11\.0$")
        # An exact key set prevents inherited secrets, inputs, skip/soft-fail
        # overrides or a second caller configuration from hiding in this job.
        self.assertEqual(re.findall(r"^    ([\w-]+):", caller, re.MULTILINE),
                         ["if", "permissions", "uses"])
        self.assertEqual(re.findall(r"^      ([\w-]+): (\w+)(?: +#.*)?$", caller, re.MULTILINE),
                         [("actions", "read"), ("contents", "read"),
                          ("security-events", "write")])

    def test_nontrivial_permission_reasons_are_attached_to_each_key(self):
        caller = self.jobs["public-pr-security"]
        self.assertRegex(caller, r"(?m)^      actions: read # .*OSV.*artifacts.*$")
        self.assertRegex(caller, r"(?m)^      security-events: write # .*OSV.*SARIF.*$")

    def test_existing_ci_events_and_concurrency_stay_reportable(self):
        events = re.search(r"^on:\n(.*?)^permissions:", self.text,
                           re.MULTILINE | re.DOTALL).group(1)
        self.assertEqual(re.findall(r"^  ([\w_]+):", events, re.MULTILINE),
                         ["pull_request", "merge_group", "push", "schedule", "workflow_dispatch"])
        self.assertIn("branches: [main]", events)
        for text in (self.text, WORKFLOW.with_name("codeql.yml").read_text(encoding="utf-8")):
            self.assertIn("group: ${{ github.workflow }}-${{ github.event_name }}-${{ github.event.pull_request.number || github.run_id }}", text)
            self.assertIn("cancel-in-progress: ${{ github.event_name == 'pull_request' }}", text)
        self.assertRegex(self.text, r"(?m)^permissions:\n  contents: read\n")

    def test_required_repo_workflows_do_not_repeat_on_readiness(self):
        for workflow, types in {
            "ci": "opened, synchronize, reopened",
            "codeql": "opened, synchronize, reopened",
            "gitleaks": "opened, synchronize, reopened",
            "migrations-staging": "opened, synchronize, reopened",
            "pr-lint": "opened, synchronize, reopened",
            "pr-size": "opened, synchronize, reopened, labeled, unlabeled",
        }.items():
            with self.subTest(workflow=workflow):
                text = WORKFLOW.with_name(f"{workflow}.yml").read_text(encoding="utf-8")
                self.assertIn(f"\n  pull_request:\n    types: [{types}]\n", text)

    def test_ci_ok_and_regression_suite_are_required(self):
        self.assertIn("public-pr-security", self.needs)
        self.assertEqual(job_level_field(self.aggregate, "name"), "ci-ok")
        self.assertEqual(job_level_field(self.aggregate, "if"), "${{ always() }}")
        self.assertIsNone(job_level_field(self.aggregate, "continue-on-error"))
        self.assertIn("CI_EVENT_NAME: ${{ github.event_name }}", self.aggregate)
        self.assertIn("NEEDS_JSON: ${{ toJSON(needs) }}", self.aggregate)
        self.assertEqual(check_required_step("public-text", self.jobs["public-text"], COMMAND), [])
        self.assertIn("public-text", self.needs)

    def test_every_enforcing_job_remains_in_aggregate(self):
        self.assertEqual(set(self.needs), set(self.jobs) - {"ci-ok"})
        self.assertEqual(len(self.needs), len(set(self.needs)))

    def test_worker_wasm_report_uses_the_enforcing_build(self):
        budget = "worker-wasm-budget"
        command = "python3 .github/scripts/check_worker_wasm_budget.py"
        self.assertNotIn("worker-wasm-sizes", self.jobs.keys())
        self.assertEqual(self.text.count("report_worker_wasm_sizes.py"), 0)
        self.assertIn(budget, self.needs)
        job = self.jobs[budget]
        self.assertEqual(job_level_field(job, "name"), "worker wasm budget (enforcing)")
        self.assertEqual(non_plain_key_lines(job), [])
        builds = [
            step
            for step in split_steps(job)
            if command in (step_level_field(step, "run") or "")
        ]
        self.assertEqual(len(builds), 1)
        self.assertEqual(self.text.count(command), 1)
        self.assertEqual(step_level_field(builds[0], "run"), command)
        self.assertIn("'worker-wasm-budget'", job_level_field(job, "if"))
        self.assertIsNone(job_level_field(job, "continue-on-error"))
        for field in ("if", "continue-on-error"):
            self.assertIsNone(step_level_field(builds[0], field))

    def test_successful_public_pr(self):
        result = self.verdict("pull_request")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_only_non_pr_events_allow_security_skip(self):
        for event in ("push", "merge_group", "schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                result = self.verdict(event)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotEqual(self.verdict(event, {"public-pr-security": "success"}).returncode, 0)

    def test_private_or_unexpectedly_skipped_pr_fails_without_scan(self):
        self.assertNotEqual(self.verdict("pull_request", {"public-pr-security": "skipped"}).returncode, 0)

    def test_failed_cancelled_skipped_and_unknown_dependencies_fail(self):
        for event in ("pull_request", "push", "merge_group", "workflow_dispatch"):
            for job in self.needs:
                for result in ("failure", "cancelled", "skipped", "unknown", None):
                    if job == "public-pr-security" and event != "pull_request" and result == "skipped":
                        continue
                    with self.subTest(event=event, job=job, result=result):
                        self.assertNotEqual(self.verdict(event, {job: result}).returncode, 0)

    def test_missing_or_malformed_evidence_fails(self):
        for raw in ("{}", "null", "[]", "not-json", '{"rust-test":{"result":"success"}}',
                    '{"public-pr-security":{"result":"success"}}'):
            with self.subTest(raw=raw):
                self.assertNotEqual(self.verdict("pull_request", raw=raw).returncode, 0)
        for skip, complete in (("", "true"), ("null", "true"), ("[]", "false"), ("[]", "")):
            with self.subTest(skip=skip, complete=complete):
                self.assertNotEqual(self.verdict("pull_request", skip=skip, complete=complete).returncode, 0)

    def test_only_planned_pr_job_skips_pass(self):
        skip = '["web-e2e"]'
        self.assertEqual(self.verdict("pull_request", {"web-e2e": "skipped"}, skip=skip).returncode, 0)
        for event, status in (("pull_request", "cancelled"), ("pull_request", "failure"),
                              ("push", "skipped"), ("schedule", "skipped"), ("merge_group", "skipped")):
            with self.subTest(event=event, status=status):
                self.assertNotEqual(self.verdict(event, {"web-e2e": status}, skip=skip).returncode, 0)

    def test_actions_dependabot_coverage_already_present(self):
        text = (ROOT / ".github/dependabot.yml").read_text(encoding="utf-8")
        blocks = re.split(r"^  - package-ecosystem: ", text, flags=re.MULTILINE)[1:]
        actions = [block for block in blocks if block.startswith('"github-actions"\n')]
        self.assertEqual(len(actions), 1)
        self.assertIn('    directory: "/"', actions[0])
        self.assertIn('      interval: "weekly"', actions[0])


if __name__ == "__main__":
    unittest.main()
