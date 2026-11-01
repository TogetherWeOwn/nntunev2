"""Structural tests for the required-check workflows' concurrency blocks.

`pr-lint` and `migrations-staging` are required checks on main. Both
trigger on pull_request `opened` and `edited`, so Dependabot PR creation
fires two events about a second apart for the same head SHA, and its
label bookkeeping can add more. A per-PR group with cancel-in-progress
cancelled the first run, leaving a permanent CANCELLED required check in
the rollup beside its successful twin — five of the eight 2026-11-01
Dependabot PRs read as stuck though every required check had passed.

Policy: keep the per-PR group (runs stay serialized) but never cancel —
`cancel-in-progress: false` so an in-flight run finishes, and
`queue: max` so a third rapid event cannot cancel a still-pending run
(the default queue keeps only the newest pending run per group).

Reverting any of these settings leaves the workflows valid, so this
suite is what turns CI red. Stdlib only, reusing the helpers in
test_ci_pnpm_retry.
"""

import os
import re
import unittest

from test_ci_pnpm_retry import check_required_step, split_jobs

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")
WORKFLOW_DIR = os.path.join(REPO_ROOT, ".github", "workflows")

SELF_COMMAND = "python3 .github/scripts/test_ci_required_check_concurrency.py"
FORMER_CANCEL = "cancel-in-progress: ${{ github.event_name == 'pull_request' }}"
# Workflow file -> the exact per-PR concurrency group it must keep.
REQUIRED_CHECK_WORKFLOWS = {
    "pr-lint.yml": "${{ github.workflow }}-${{ github.event.pull_request.number || inputs.pr_number || github.sha }}",
    "migrations-staging.yml": "${{ github.workflow }}-${{ github.event.pull_request.number || github.sha }}",
}


def concurrency_errors(text, label, group):
    """Violations of one required-check workflow's concurrency policy."""
    block = re.search(r"^concurrency:\s*\n((?:  .*\n?)+)", text, re.MULTILINE)
    if not block:
        return [f"{label}: missing top-level concurrency"]
    body = block.group(1)
    errors = []
    if not re.search(rf"^  group: {re.escape(group)}\s*$", body, re.MULTILINE):
        errors.append(f"{label}: concurrency must group by PR number")
    if not re.search(r"^  cancel-in-progress: false\s*$", body, re.MULTILINE):
        errors.append(f"{label}: must not cancel duplicate required-check runs")
    if not re.search(r"^  queue: max\s*$", body, re.MULTILINE):
        errors.append(f"{label}: must retain all pending runs (queue: max)")
    return errors


class TestRequiredCheckConcurrency(unittest.TestCase):
    """Pin the no-cancel policy for the two required-check workflows."""

    def setUp(self):
        """Load ci.yml and both guarded workflows for every test."""
        with open(CI_PATH, encoding="utf-8") as handle:
            self.ci = handle.read()
        self.workflows = {}
        for name in REQUIRED_CHECK_WORKFLOWS:
            with open(os.path.join(WORKFLOW_DIR, name), encoding="utf-8") as handle:
                self.workflows[name] = handle.read()

    def test_real_workflows_are_compliant(self):
        """The committed workflows never cancel duplicate runs."""
        for name, group in REQUIRED_CHECK_WORKFLOWS.items():
            with self.subTest(workflow=name):
                self.assertEqual(concurrency_errors(self.workflows[name], name, group), [])

    def test_former_cancel_expression_fails(self):
        """Reverting to cancelling pull_request runs is caught."""
        for name, group in REQUIRED_CHECK_WORKFLOWS.items():
            with self.subTest(workflow=name):
                mutated = self.workflows[name].replace(
                    "cancel-in-progress: false", FORMER_CANCEL, 1
                )
                self.assertNotEqual(mutated, self.workflows[name])
                self.assertIn(
                    f"{name}: must not cancel duplicate required-check runs",
                    concurrency_errors(mutated, name, group),
                )

    def test_dropped_queue_fails(self):
        """Losing queue: max (a third event cancels the pending run) is caught."""
        for name, group in REQUIRED_CHECK_WORKFLOWS.items():
            with self.subTest(workflow=name):
                mutated = self.workflows[name].replace("  queue: max\n", "", 1)
                self.assertNotEqual(mutated, self.workflows[name])
                self.assertIn(
                    f"{name}: must retain all pending runs (queue: max)",
                    concurrency_errors(mutated, name, group),
                )

    def test_widened_group_fails(self):
        """Grouping by workflow alone (no per-PR key) is caught."""
        for name, group in REQUIRED_CHECK_WORKFLOWS.items():
            with self.subTest(workflow=name):
                mutated = self.workflows[name].replace(
                    group, "${{ github.workflow }}", 1
                )
                self.assertNotEqual(mutated, self.workflows[name])
                self.assertIn(
                    f"{name}: concurrency must group by PR number",
                    concurrency_errors(mutated, name, group),
                )

    def test_removed_block_fails(self):
        """Deleting the concurrency block is caught."""
        for name, group in REQUIRED_CHECK_WORKFLOWS.items():
            with self.subTest(workflow=name):
                removed = re.sub(
                    r"^concurrency:\n(?:  .*\n)+", "", self.workflows[name], flags=re.MULTILINE
                )
                self.assertNotEqual(removed, self.workflows[name])
                self.assertEqual(
                    concurrency_errors(removed, name, group),
                    [f"{name}: missing top-level concurrency"],
                )

    def test_self_step_is_required_in_public_text(self):
        """This suite's public-text step stays required.

        test_ci_web_e2e_timeout.py pins the same step independently, so
        deleting it still turns CI red even though this test stops running.
        """
        jobs = split_jobs(self.ci)
        self.assertEqual(check_required_step("public-text", jobs["public-text"], SELF_COMMAND), [])


if __name__ == "__main__":
    unittest.main()
