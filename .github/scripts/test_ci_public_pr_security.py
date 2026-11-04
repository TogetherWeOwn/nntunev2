"""Public-only PR caller wiring and executable ci-ok verdict tests.

Keep the caller in the existing graph so no cross-workflow polling or new
ruleset check is needed. Only this job may skip on non-PR events; every
other required job must still succeed. Stdlib plus CI's existing jq.
"""

import json
import os
from pathlib import Path
import re
import subprocess
import textwrap
import unittest

from test_ci_pnpm_retry import check_required_step, job_level_field, split_jobs

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"
COMMAND = "python3 .github/scripts/test_ci_public_pr_security.py"
PIN = "feb1e0e2111da007f38ca75ee95c6f6944723d36"
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

    def verdict(self, event, overrides=None, raw=None):
        needs = {job: {"result": "success"} for job in self.needs}
        needs["public-pr-security"]["result"] = (
            "success" if event == "pull_request" else "skipped"
        )
        for job, result in (overrides or {}).items():
            needs[job]["result"] = result
        env = {"PATH": os.environ["PATH"], "CI_EVENT_NAME": event,
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
        self.assertRegex(caller, rf"(?m)^    uses: .+@{PIN} # v0\.5\.0$")
        # An exact key set prevents inherited secrets, inputs, skip/soft-fail
        # overrides or a second caller configuration from hiding in this job.
        self.assertEqual(re.findall(r"^    ([\w-]+):", caller, re.MULTILINE),
                         ["if", "permissions", "uses"])
        self.assertEqual(re.findall(r"^      ([\w-]+): (\w+)$", caller, re.MULTILINE),
                         [("actions", "read"), ("contents", "read"),
                          ("security-events", "write")])

    def test_existing_ci_events_and_concurrency_stay_reportable(self):
        events = re.search(r"^on:\n(.*?)^permissions:", self.text,
                           re.MULTILINE | re.DOTALL).group(1)
        self.assertEqual(re.findall(r"^  ([\w_]+):", events, re.MULTILINE),
                         ["pull_request", "merge_group", "push", "workflow_dispatch"])
        self.assertIn("types: [opened, synchronize, reopened, ready_for_review]", events)
        self.assertIn("branches: [main]", events)
        self.assertIn("group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.ref }}", self.text)
        self.assertIn("cancel-in-progress: ${{ github.ref != 'refs/heads/main' && github.event_name != 'merge_group' }}", self.text)
        self.assertRegex(self.text, r"(?m)^permissions:\n  contents: read\n")

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
        self.assertEqual(set(self.needs), set(self.jobs) - {"ci-ok", "worker-wasm-sizes"})
        self.assertEqual(len(self.needs), len(set(self.needs)))

    def test_successful_public_pr(self):
        result = self.verdict("pull_request")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_only_non_pr_events_allow_security_skip(self):
        for event in ("push", "merge_group", "workflow_dispatch"):
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

    def test_actions_dependabot_coverage_already_present(self):
        text = (ROOT / ".github/dependabot.yml").read_text(encoding="utf-8")
        blocks = re.split(r"^  - package-ecosystem: ", text, flags=re.MULTILINE)[1:]
        actions = [block for block in blocks if block.startswith('"github-actions"\n')]
        self.assertEqual(len(actions), 1)
        self.assertIn('    directory: "/"', actions[0])
        self.assertIn('      interval: "weekly"', actions[0])


if __name__ == "__main__":
    unittest.main()
