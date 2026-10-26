"""Keep web-e2e setup off apt mirrors without skipping browser execution.

The full suite keeps its existing deadline and fail-closed aggregate. Offline
preflight doubles cover unavailable browsers; CI must still launch real Chromium.
"""

import re
import unittest

from test_ci_playwright_image import ROOT, WORKFLOW, run_body
from test_ci_pnpm_retry import split_jobs, split_steps, step_field

VERIFY_NAME = "Verify preinstalled web Playwright chromium"


class TestWebPlaywrightImage(unittest.TestCase):
    def setUp(self):
        self.workflow = WORKFLOW.read_text()
        self.jobs = split_jobs(self.workflow)
        self.job = self.jobs["web-e2e"]

    def test_image_matches_all_three_locked_web_browser_packages(self):
        lock = (ROOT / "apps/web/pnpm-lock.yaml").read_text()
        versions = []
        for package in ("@playwright/test", "playwright", "playwright-core"):
            matches = re.findall(
                rf"^  '?{re.escape(package)}@([0-9]+\.[0-9]+\.[0-9]+)'?:$",
                lock, re.MULTILINE,
            )
            self.assertTrue(matches, f"no locked {package}")
            versions.extend(matches)
        self.assertEqual(len(set(versions)), 1, "web browser packages drifted")
        image = step_field(self.job, "container")
        self.assertIsNotNone(image, "web-e2e must not install OS/browser dependencies per run")
        self.assertRegex(
            image,
            rf"^mcr\.microsoft\.com/playwright:v{re.escape(versions[0])}-noble@sha256:[0-9a-f]{{64}}$",
        )
        self.assertNotIn("playwright install", self.job)
        self.assertNotIn("apt-get", self.job)

    def test_full_suite_and_deadline_remain_required(self):
        steps = split_steps(self.job)
        suite = next(s for s in steps if step_field(s, "name") == "Run apps/web Playwright suite")
        verify = next((s for s in steps if step_field(s, "name") == VERIFY_NAME), None)
        self.assertIsNotNone(verify, "require an actual Chromium launch/close before the suite")
        self.assertEqual(step_field(self.job, "timeout-minutes"), "30")
        self.assertEqual(step_field(self.job, "runs-on"), "ubuntu-latest")
        self.assertEqual(step_field(self.job, "working-directory"), "apps/web")
        self.assertIsNone(step_field(verify, "working-directory"))
        self.assertIsNone(step_field(suite, "working-directory"))
        self.assertTrue(any(step_field(s, "run") == "pnpm install --frozen-lockfile" for s in steps))
        self.assertEqual(step_field(suite, "run"), "pnpm test:e2e")
        self.assertEqual(step_field(verify, "shell"), "bash")
        self.assertEqual(step_field(verify, "timeout-minutes"), "1")
        self.assertLess(steps.index(verify), steps.index(suite))
        for step in (verify, suite):
            self.assertIsNone(step_field(step, "continue-on-error"))
            self.assertIsNone(step_field(step, "if"))
        self.assertIn("web-e2e", step_field(self.jobs["ci-ok"], "needs"))
        self.assertIn('all(.[]; .result == "success")', self.jobs["ci-ok"])
        self.assertIn("python3 .github/scripts/test_ci_web_e2e_image.py", self.jobs["e2e-lint"])

    def test_preflight_reuses_the_exercised_launch_close_contract(self):
        # The neighboring suite executes this exact body with a usable browser,
        # missing executable/library/package, and failed close. Reuse its body,
        # not an untested alternative that merely checks a file exists.
        web = next((s for s in split_steps(self.job) if step_field(s, "name") == VERIFY_NAME), None)
        self.assertIsNotNone(web, "missing web browser preflight")
        helper = next(s for s in split_steps(self.jobs["e2e-lint"])
                      if step_field(s, "name") == "Verify preinstalled Playwright chromium (browser-level self-tests)")
        self.assertEqual(run_body(web), run_body(helper))


if __name__ == "__main__":
    unittest.main()
