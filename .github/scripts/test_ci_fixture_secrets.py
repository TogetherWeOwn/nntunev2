"""Pin the CC0 fixture R2 secret wiring and the retired staging NZB leg.

The CC0 fixture tests fetch real audio from R2 through the read-only
``NNTUNE_FIXTURE_R2_*`` secrets through ``ci-fixtures`` (docs/fixture-audio-licences.md).
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
import re
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


def check_fixture_step_bindings(job_id, job):
    steps = re.split(r"(?m)(?=^      - )", job)[1:]
    command = (
        "cargo test --locked --manifest-path workers/ingest/Cargo.toml --all-targets"
        if job_id == "rust-ingest" else "pnpm test:e2e --reporter=dot,./tests/reporters/runtime-inventory.ts"
    )
    selectors = (
        rf"^      - name: {re.escape(FETCH_STEP)}$",
        rf"^\s+(?:- )?run: {re.escape(command)}$",
    )
    errors = []
    for selector in selectors:
        matches = [step for step in steps if re.search(selector, step, re.M)]
        if len(matches) != 1:
            errors.append(f"{job_id}: expected exactly one step matching {selector}")
            continue
        for name in FIXTURE_SECRET_NAMES:
            binding = name + ": ${{ env.FIXTURE_SECRETS_ALLOWED == 'true' && secrets." + name + " || '' }}"
            if matches[0].count("          " + binding + "\n") != 1:
                errors.append(f"{job_id}: missing or duplicate {name} in {selector}")
    return errors


class TestFixtureSecretWiring(unittest.TestCase):
    """The fetching jobs get the read-only secrets; no other job does."""

    def setUp(self):
        """Load the real workflows for every test."""
        self.ci = _read(CI_PATH)
        self.jobs = split_jobs(self.ci)
        self.e2e_staging = _read(E2E_STAGING_PATH)
        self.deploy = _read(DEPLOY_PATH)

    def test_fetching_jobs_receive_every_fixture_secret(self):
        """Only eligible fixture steps receive values, under their own names."""
        for job_id in ("rust-ingest", "web-e2e"):
            job = self.jobs[job_id]
            self.assertIn("    environment:\n      name: ci-fixtures\n      deployment: false\n", job)
            self.assertEqual(check_fixture_step_bindings(job_id, job), [])
            for name in FIXTURE_SECRET_NAMES:
                binding = name + ": ${{ env.FIXTURE_SECRETS_ALLOWED == 'true' && secrets." + name + " || '' }}"
                self.assertEqual(job.count(binding), 2, job_id)
                self.assertNotIn(name + ": ${{ secrets." + name + " }}", job, job_id)

    def test_bindings_moved_to_unrelated_steps_are_rejected(self):
        for job_id in ("rust-ingest", "web-e2e"):
            job = self.jobs[job_id]
            for name in FIXTURE_SECRET_NAMES:
                binding = name + ": ${{ env.FIXTURE_SECRETS_ALLOWED == 'true' && secrets." + name + " || '' }}"
                for occurrence in (0, 1):
                    with self.subTest(job=job_id, secret=name, occurrence=occurrence):
                        positions = [match.start() for match in re.finditer(re.escape("          " + binding + "\n"), job)]
                        start = positions[occurrence]
                        mutated = job[:start] + job[start:].replace("          " + binding + "\n", "", 1)
                        mutated += "      - name: Unrelated lint\n        env:\n          " + binding + "\n        run: true\n"
                        self.assertEqual(mutated.count(binding), 2)
                        self.assertTrue(check_fixture_step_bindings(job_id, mutated))

    def test_secret_trust_guard_evaluates_real_workflow_expression(self):
        """Fork/Dependabot denial never skips the noncredentialed test jobs."""
        cases = [
            ("pull_request", "contributor", "own/repo", "refs/pull/1/merge", True),
            ("pull_request", "contributor", "fork/repo", "refs/pull/1/merge", False),
            ("pull_request", "dependabot[bot]", "own/repo", "refs/pull/1/merge", False),
            ("push", "contributor", "", "refs/heads/main", True),
            ("push", "contributor", "", "refs/heads/topic", False),
            ("merge_group", "contributor", "", "refs/heads/gh-readonly-queue/main/entry", True),
            ("workflow_dispatch", "contributor", "", "refs/heads/topic", True),
            ("pull_request_target", "contributor", "own/repo", "refs/heads/main", False),
            ("schedule", "contributor", "", "refs/heads/main", False),
            ("workflow_dispatch", "dependabot[bot]", "", "refs/heads/main", False),
        ]
        for job_id in ("rust-ingest", "web-e2e"):
            job = self.jobs[job_id]
            self.assertNotRegex(job, r"(?m)^    if:")
            guard = re.search(r"^      FIXTURE_SECRETS_ALLOWED: \$\{\{ (.+) \}\}$", job, re.M)
            self.assertIsNotNone(guard, job_id)
            for event, actor, head_repo, ref, expected in cases:
                with self.subTest(job=job_id, event=event, actor=actor, head=head_repo):
                    expression = guard.group(1)
                    terms = {
                        "github.actor != 'dependabot[bot]'": actor != "dependabot[bot]",
                        "github.event.pull_request.head.repo.full_name == github.repository": head_repo == "own/repo",
                        "github.ref == 'refs/heads/main'": ref == "refs/heads/main",
                    }
                    for name in ("pull_request", "push", "merge_group", "workflow_dispatch"):
                        terms[f"github.event_name == '{name}'"] = event == name
                    for term, value in terms.items():
                        expression = expression.replace(term, str(value))
                    expression = expression.replace("&&", " and ").replace("||", " or ")
                    self.assertRegex(expression, r"^(?:True|False|and|or|\s|[()])+$")
                    self.assertEqual(eval(expression, {"__builtins__": {}}), expected)

    def test_staging_presence_gates_use_only_the_staging_environment(self):
        for name in ("deploy.yml", "deploy-alerts-staging.yml"):
            jobs = split_jobs(_read(os.path.join(REPO_ROOT, ".github", "workflows", name)))
            self.assertIn("    environment:\n      name: staging\n      deployment: false\n", jobs["gate"], name)
            for job_id, job in jobs.items():
                if job_id != "gate":
                    self.assertNotIn("deployment: false", job, name + ":" + job_id)
            self.assertNotIn("environment: production", jobs["gate"], name)

    def test_fetching_jobs_run_the_visible_fetch_proof_step(self):
        """Each fetching job runs the sha256-verifying fetch step.

        The fetcher exits 0 only when the cached file matches the
        manifest bytes + sha256, so the step is the run-log proof that a
        real fetch happened; without it a green job could hide a skip.
        """
        for job_id in ("rust-ingest", "web-e2e"):
            self.assertIn(FETCH_STEP, self.jobs[job_id], job_id)
            self.assertIn(
                "fixtures/cc0/fetch_fixture.py --position 1",
                self.jobs[job_id],
                job_id,
            )

    def test_web_e2e_fetch_step_selects_an_ssl_capable_python(self):
        """The container's toolcache Python cannot import ssl.

        Without the fallback selection the web-e2e download dies with
        "unknown url type: https" even though the step is present, so
        pin the ssl probe and the system-python fallback explicitly.
        """
        job = self.jobs["web-e2e"]
        self.assertIn("python3 -c 'import ssl'", job)
        self.assertIn("fetcher=/usr/bin/python3", job)

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
