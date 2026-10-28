"""Pin the returning-listener fixture suite to required CI.

The fixture-check job runs the synthetic returning-listener unittest
suite and feeds the ci-ok aggregate. This suite pins that wiring:
neutralizing, gating, masking, or dropping the fixture command, or
removing fixture-check from ci-ok, must turn CI red.

This suite runs in the public-text job, so it cannot catch deletion of
its own run step. The web-e2e timeout suite pins this suite's
public-text step independently, and this suite reciprocally pins the
timeout guard's step (the same cross-pin the pnpm and timeout suites
use for each other).
"""

import os
import unittest

from test_ci_pnpm_retry import check_required_step, job_needs, split_jobs

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
WORKFLOW_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")

FIXTURE_COMMAND = (
    "python3 -m unittest discover -s fixtures/returning-listener "
    "-p 'test_*.py' -v"
)
SELF_COMMAND = "python3 .github/scripts/test_ci_returning_listener.py"
TIMEOUT_COMMAND = "python3 .github/scripts/test_ci_web_e2e_timeout.py"


def fixture_check_with(step_block, job_extra="", job_after_steps=""):
    """A minimal fixture-check job chunk carrying `step_block`."""
    return (
        "  fixture-check:\n"
        "    runs-on: ubuntu-latest\n"
        f"{job_extra}"
        "    steps:\n"
        f"{step_block}"
        f"{job_after_steps}"
    )


def fixture_step(extra="", command=FIXTURE_COMMAND):
    """The returning-listener fixture step, plus `extra` lines."""
    return (
        "      - name: Reconcile synthetic returning-listener fixtures\n"
        f"        run: {command}\n"
        f"{extra}"
    )


def fixture_errors(text):
    """Wiring errors for the fixture suite in a fixture-check chunk."""
    return check_required_step("fixture-check", text, FIXTURE_COMMAND)


class TestReturningListenerWiring(unittest.TestCase):
    """Pin the fixture command, the ci-ok gate, and this suite's wiring."""

    def setUp(self):
        """Load the real workflow's jobs for every test."""
        with open(WORKFLOW_PATH, encoding="utf-8") as handle:
            self.jobs = split_jobs(handle.read())

    def test_fixture_step_is_required_in_fixture_check(self):
        """fixture-check runs the exact fixture suite unconditionally."""
        self.assertIn("fixture-check", self.jobs)
        self.assertEqual(fixture_errors(self.jobs["fixture-check"]), [])

    def test_fixture_check_and_public_text_stay_in_ci_ok_gate(self):
        """ci-ok needs fixture-check and the public-text job running this guard."""
        needs = job_needs(self.jobs["ci-ok"])
        self.assertIn("fixture-check", needs)
        self.assertIn("public-text", needs)

    def test_shadow_job_names_do_not_satisfy_ci_ok_gate(self):
        """Job ids merely containing `fixture-check` are not the fixture job."""
        needs = job_needs("    needs: [fixture-check-shadow, pre-fixture-check]\n")
        self.assertEqual(needs, ["fixture-check-shadow", "pre-fixture-check"])
        self.assertNotIn("fixture-check", needs)

    def test_self_guard_step_is_required_in_public_text(self):
        """This suite's own public-text step stays required.

        The timeout suite pins the same step independently, so deleting
        this step still turns CI red even though this test would no
        longer run.
        """
        self.assertEqual(
            check_required_step("public-text", self.jobs["public-text"], SELF_COMMAND),
            [],
        )

    def test_timeout_guard_step_is_required_in_public_text(self):
        """The timeout suite that backstops this suite stays required too."""
        self.assertEqual(
            check_required_step("public-text", self.jobs["public-text"], TIMEOUT_COMMAND),
            [],
        )

    def test_valid_fixture_step_stays_clean(self):
        """Negative control: a plain exact fixture step is not flagged."""
        self.assertEqual(fixture_errors(fixture_check_with(fixture_step())), [])

    def test_missing_fixture_step_fails(self):
        """Deleting the fixture step turns this suite red."""
        errors = fixture_errors(fixture_check_with(""))
        self.assertTrue(any("no step runs" in e for e in errors), errors)

    def test_neutralized_fixture_command_fails(self):
        """Replacing the fixture command with a no-op turns this suite red."""
        errors = fixture_errors(fixture_check_with(fixture_step(command="true")))
        self.assertTrue(any("no step runs" in e for e in errors), errors)

    def test_command_in_another_job_does_not_satisfy_fixture_check(self):
        """The exact command must run in fixture-check, not only elsewhere."""
        text = (
            "jobs:\n"
            + fixture_check_with("")
            + "  other:\n"
            + "    runs-on: ubuntu-latest\n"
            + "    steps:\n"
            + fixture_step()
        )
        errors = fixture_errors(split_jobs(text)["fixture-check"])
        self.assertTrue(any("no step runs" in e for e in errors), errors)

    def test_conditional_fixture_step_fails(self):
        """An `if` gate on the fixture step turns this suite red."""
        errors = fixture_errors(
            fixture_check_with(fixture_step("        if: always()\n"))
        )
        self.assertTrue(any("must be unconditional" in e for e in errors), errors)

    def test_continue_on_error_fixture_step_fails(self):
        """A continue-on-error fixture step turns this suite red."""
        errors = fixture_errors(
            fixture_check_with(fixture_step("        continue-on-error: true\n"))
        )
        self.assertTrue(any("not continue-on-error" in e for e in errors), errors)

    def test_masked_fixture_command_fails(self):
        """Masking the fixture command's exit code turns this suite red."""
        errors = fixture_errors(
            fixture_check_with(fixture_step(command=f"{FIXTURE_COMMAND} || true"))
        )
        self.assertTrue(any("must be exactly" in e for e in errors), errors)

    def test_job_level_gates_fail(self):
        """Job-level `if`/`continue-on-error` on fixture-check turn this red."""
        cases = (
            ("    continue-on-error: true\n", "not continue-on-error"),
            ("    if: ${{ false }}\n", "no job-level if"),
        )
        for extra, expected in cases:
            for placement in ("job_extra", "job_after_steps"):
                with self.subTest(extra=extra, placement=placement):
                    errors = fixture_errors(
                        fixture_check_with(fixture_step(), **{placement: extra})
                    )
                    self.assertTrue(any(expected in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
