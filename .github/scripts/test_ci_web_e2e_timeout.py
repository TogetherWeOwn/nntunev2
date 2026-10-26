"""Pin the web-e2e job timeout to its measured budget.

Measured on GitHub-hosted ubuntu-latest, 2026-10-25/26: the Playwright
suite (1,425-1,478 tests) reported 18.8-20.6 minutes, and the
"Install Playwright chromium" step took 11 minutes on a slow-download
run (22 seconds on a fast run). The budget this test pins:

    17m worst-observed setup (dependency/browser downloads)
  + 21m suite (20.6m worst measured, rounded up)
  +  7m headroom
  = 45m

Under the previous 30m timeout, a run with more than ~9 minutes of
setup was cancelled mid-suite with zero test failures. Changing the
timeout requires fresh measurements: update the comment on the
web-e2e job in ci.yml and this file together.
"""

import os
import unittest

from test_ci_pnpm_retry import split_jobs, step_field

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORKFLOW_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")


class TestWebE2eTimeout(unittest.TestCase):
    def setUp(self):
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            self.jobs = split_jobs(handle.read())

    def test_web_e2e_timeout_matches_measured_budget(self):
        self.assertEqual(step_field(self.jobs["web-e2e"], "timeout-minutes"), "45")

    def test_web_e2e_stays_in_ci_ok_gate(self):
        self.assertIn("web-e2e", step_field(self.jobs["ci-ok"], "needs"))


if __name__ == "__main__":
    unittest.main()
