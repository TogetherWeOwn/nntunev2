"""Pin the CC0 fixture R2 secret wiring and the retired staging NZB leg.

The CC0 fixture tests fetch real audio from R2 through the read-only
``NNTUNE_FIXTURE_R2_*`` repo secrets (docs/fixture-audio-licences.md).
Only the jobs whose tests actually fetch may receive them:

- ``rust-ingest``: workers/ingest/tests/cc0_fixture_album.rs reads
  track 1 through crates/nntune-test-fixtures.
- ``web-e2e``: apps/web/tests/player-cc0-flac.spec.ts reads track 1
  through e2e/helpers/cc0-fixture.ts.

Jobs that never fetch must not receive the secrets (least exposure):
``e2e-lint`` runs the cc0 helper's offline unit spec with the config
env cleared, ``vitest`` has no fixture consumer, and the workspace
crate self-tests clear the config too. Fork PRs receive no secrets at
all, so every consumer still skips through the fetcher's exit-77 path.

The secret-held real-NZB staging leg is retired under the same CC0-only
rule: no workflow may reference the retired ``NNTUNE_TEST_NZB_XZ_B64_*``
secrets, so deleting them cannot fail a check.
"""

import os
import unittest

from test_ci_pnpm_retry import check_required_step, split_jobs

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")
E2E_STAGING_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "e2e-staging.yml")
DEPLOY_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "deploy.yml")

SELF_COMMAND = "python3 .github/scripts/test_ci_fixture_secrets.py"

FIXTURE_SECRET_NAMES = (
    "NNTUNE_FIXTURE_R2_ACCESS_KEY_ID",
    "NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY",
    "NNTUNE_FIXTURE_R2_ACCOUNT_ID",
)

FETCH_STEP = "Fetch CC0 fixture track 1 (R2, sha256-verified against the manifest)"


def _read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


class TestFixtureSecretWiring(unittest.TestCase):
    """The fetching jobs get the read-only secrets; no other job does."""

    def setUp(self):
        """Load the real workflows for every test."""
        self.ci = _read(CI_PATH)
        self.jobs = split_jobs(self.ci)
        self.e2e_staging = _read(E2E_STAGING_PATH)
        self.deploy = _read(DEPLOY_PATH)

    def test_fetching_jobs_receive_every_fixture_secret(self):
        """rust-ingest and web-e2e pass each secret under its own name."""
        for job_id in ("rust-ingest", "web-e2e"):
            job = self.jobs[job_id]
            for name in FIXTURE_SECRET_NAMES:
                self.assertIn(name + ": ${{ secrets." + name + " }}", job, job_id)

    def test_fetching_jobs_run_the_visible_fetch_proof_step(self):
        """Each fetching job runs the sha256-verifying fetch step.

        The fetcher exits 0 only when the cached file matches the
        manifest bytes + sha256, so the step is the run-log proof that a
        real fetch happened; without it a green job could hide a skip.
        """
        for job_id in ("rust-ingest", "web-e2e"):
            self.assertIn(FETCH_STEP, self.jobs[job_id], job_id)
            self.assertIn(
                "python3 fixtures/cc0/fetch_fixture.py --position 1",
                self.jobs[job_id],
                job_id,
            )

    def test_fetch_proof_step_notices_when_secrets_are_absent(self):
        """Fork PRs (no secrets) must notice-and-skip, never fail."""
        for job_id in ("rust-ingest", "web-e2e"):
            self.assertIn(
                "::notice::CC0 fixture R2 secrets unavailable (fork PR)",
                self.jobs[job_id],
                job_id,
            )

    def test_non_fetching_jobs_receive_no_fixture_secrets(self):
        """Offline-only jobs stay secret-free.

        e2e-lint's cc0 spec clears the config env itself, vitest has no
        fixture consumer, and rust-test's crate self-tests clear the
        config for the child process — none of them needs R2 access.
        """
        for job_id in ("e2e-lint", "vitest", "rust-test", "public-text"):
            self.assertNotIn("secrets.NNTUNE_FIXTURE_R2_", self.jobs[job_id], job_id)

    def test_no_workflow_references_the_retired_nzb_secrets(self):
        """The retired staging NZB halves can be deleted safely."""
        for label, text in (
            ("ci.yml", self.ci),
            ("e2e-staging.yml", self.e2e_staging),
            ("deploy.yml", self.deploy),
        ):
            self.assertNotIn("secrets.NNTUNE_TEST_NZB_XZ_B64", text, label)

    def test_guard_step_is_required_in_public_text(self):
        """This suite's own public-text step stays required."""
        self.assertEqual(
            check_required_step("public-text", self.jobs["public-text"], SELF_COMMAND),
            [],
        )


if __name__ == "__main__":
    unittest.main()
