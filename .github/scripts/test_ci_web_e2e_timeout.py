"""Pin the web-e2e job timeout to its measured budget.

Measured on GitHub-hosted ubuntu-latest, 2026-10-26/27: the Playwright
suite (1,425-1,613 tests) reported 18.8-20.6 minutes, and container
setup (image pull + pnpm install) takes under a minute on the fast
path. Pre-container worst setup was 17m of apt/browser downloads;
slow image pulls are unmeasured, so the budget keeps headroom:

    21m suite (20.6m worst measured, rounded up)
  + 24m setup/headroom
  = 45m

Under the previous 30m timeout, a run with more than ~9 minutes of
setup was cancelled mid-suite with zero test failures. Changing the
timeout requires fresh measurements: update the comment on the
web-e2e job in ci.yml and this file together.
"""

import os
import unittest

from test_ci_pnpm_retry import check_required_step, job_level_field, job_needs, split_jobs

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORKFLOW_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")

SELF_COMMAND = "python3 .github/scripts/test_ci_web_e2e_timeout.py"
PNPM_COMMAND = "python3 .github/scripts/test_ci_pnpm_retry.py"
RETURNING_LISTENER_COMMAND = "python3 .github/scripts/test_ci_returning_listener.py"


class TestWebE2eTimeout(unittest.TestCase):
    """Pin the timeout value, the ci-ok gate, and this suite's wiring."""

    def setUp(self):
        """Load the real workflow's jobs for every test."""
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            self.jobs = split_jobs(handle.read())

    def test_web_e2e_timeout_matches_measured_budget(self):
        """web-e2e's timeout stays at the measured 45-minute budget."""
        self.assertEqual(job_level_field(self.jobs["web-e2e"], "timeout-minutes"), "45")

    def test_nested_timeout_does_not_satisfy_job_budget(self):
        """A step-level timeout cannot pose as the job's timeout."""
        self.assertIsNone(
            job_level_field("        timeout-minutes: 45\n", "timeout-minutes")
        )
        self.assertEqual(
            job_level_field(
                "    timeout-minutes: 45\n        timeout-minutes: 1\n",
                "timeout-minutes",
            ),
            "45",
        )

    def test_web_e2e_stays_in_ci_ok_gate(self):
        """ci-ok needs the exact web-e2e and public-text job ids.

        public-text is the job that executes this guard, so the gate is
        only intact while both exact ids are required.
        """
        needs = job_needs(self.jobs["ci-ok"])
        self.assertIn("web-e2e", needs)
        self.assertIn("public-text", needs)

    def test_shadow_job_names_do_not_satisfy_ci_ok_gate(self):
        """Job ids merely containing `web-e2e` are not the browser suite."""
        needs = job_needs("    needs: [web-e2e-shadow, pre-web-e2e, public-text]\n")
        self.assertEqual(needs, ["web-e2e-shadow", "pre-web-e2e", "public-text"])
        self.assertNotIn("web-e2e", needs)

    def test_nested_needs_do_not_satisfy_ci_ok_gate(self):
        """A nested matrix `needs` axis is not the job dependency list."""
        needs = job_needs(
            "    strategy:\n"
            "      matrix:\n"
            "        needs: [web-e2e, public-text, e2e-lint]\n"
            "    needs: [public-text, e2e-lint]\n"
        )
        self.assertEqual(needs, ["public-text", "e2e-lint"])
        self.assertNotIn("web-e2e", needs)
        self.assertEqual(
            job_needs(
                "    strategy:\n"
                "      matrix:\n"
                "        needs: [web-e2e, public-text]\n"
            ),
            [],
        )

    def test_block_and_scalar_needs_parse_exactly(self):
        """Block-list and scalar `needs` forms parse to exact job ids."""
        self.assertEqual(
            job_needs("    needs:\n      - web-e2e\n      - public-text\n"),
            ["web-e2e", "public-text"],
        )
        self.assertEqual(job_needs("    needs: web-e2e\n"), ["web-e2e"])

    def test_timeout_guard_step_is_required_in_public_text(self):
        """This suite's own public-text step stays required.

        The pnpm structural suite pins the same step independently, so
        deleting this step still turns CI red even though this test
        would no longer run.
        """
        self.assertEqual(
            check_required_step("public-text", self.jobs["public-text"], SELF_COMMAND),
            [],
        )

    def test_pnpm_guard_step_is_required_in_public_text(self):
        """The pnpm suite that backstops this suite stays required too."""
        self.assertEqual(
            check_required_step("public-text", self.jobs["public-text"], PNPM_COMMAND),
            [],
        )

    def test_returning_listener_guard_step_is_required_in_public_text(self):
        """The returning-listener guard's public-text step stays required.

        That suite cannot catch deletion of its own run step, so this
        independently executed suite pins it; the returning-listener
        suite reciprocally pins this suite's step.
        """
        self.assertEqual(
            check_required_step(
                "public-text", self.jobs["public-text"], RETURNING_LISTENER_COMMAND
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
