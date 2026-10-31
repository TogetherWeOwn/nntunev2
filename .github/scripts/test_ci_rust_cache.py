"""Structural tests for the Rust build cache and coverage concurrency.

Every Swatinem/rust-cache step in ci.yml restores on any ref but saves
only on main, so per-PR copies do not evict main's entries from the
10 GB repo cache. Standalone crates (workers/*) build into their own
target dir; without a matching `workspaces:` input only the registry
is cached and every run compiles cold. coverage.yml is report-only, so
a newer push to main cancels an in-flight run.

Reverting any of these settings leaves the workflows valid, so this
suite is what turns CI red. Stdlib only, reusing the indentation-aware
reader in test_ci_pnpm_retry.
"""

import os
import re
import unittest

from test_ci_pnpm_retry import check_required_step, split_jobs, split_steps, step_level_field

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CI_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")
COVERAGE_PATH = os.path.join(REPO_ROOT, ".github", "workflows", "coverage.yml")

SELF_COMMAND = "python3 .github/scripts/test_ci_rust_cache.py"
SAVE_IF_MAIN = "${{ github.ref == 'refs/heads/main' }}"
# Job id -> the standalone crate dir its rust-cache step must name.
STANDALONE_WORKSPACES = {
    "api": "workers/api",
    "api-migrations": "workers/api",
    "rust-mb": "workers/mb",
    "rust-ingest": "workers/ingest",
    "rust-repair": "workers/repair",
    "rust-transcode": "workers/transcode",
}
# Floor on cached Rust lanes so deleting cache steps cannot pass vacuously.
MIN_CACHE_STEPS = 11


def with_input(step_chunk, name):
    """A `with:` input's scalar value (ten-space indent), comment-stripped."""
    pattern = re.compile(rf"^          {re.escape(name)}:\s*(.*?)\s*$")
    for line in step_chunk.splitlines():
        match = pattern.match(line)
        if match:
            return re.sub(r"\s+#.*$", "", match.group(1))
    return None


def cache_steps(job_chunk):
    """The job's rust-cache steps."""
    return [
        step
        for step in split_steps(job_chunk)
        if (step_level_field(step, "uses") or "").startswith("Swatinem/rust-cache@")
    ]


def cache_errors(text):
    """Policy violations in a ci.yml source; empty when compliant."""
    errors = []
    jobs = split_jobs(text)
    total = 0
    for job_id, chunk in jobs.items():
        for step in cache_steps(chunk):
            total += 1
            if with_input(step, "save-if") != SAVE_IF_MAIN:
                errors.append(f"{job_id}: rust-cache must save only on main")
    if total < MIN_CACHE_STEPS:
        errors.append(f"expected >= {MIN_CACHE_STEPS} rust-cache steps, found {total}")
    for job_id, workspace in STANDALONE_WORKSPACES.items():
        steps = cache_steps(jobs.get(job_id, ""))
        if not steps:
            errors.append(f"{job_id}: missing rust-cache step")
        for step in steps:
            if with_input(step, "workspaces") != workspace:
                errors.append(f"{job_id}: rust-cache must name workspaces: {workspace}")
    return errors


def concurrency_errors(text):
    """Violations of the coverage workflow's top-level concurrency block."""
    block = re.search(r"^concurrency:\s*\n((?:  .*\n?)+)", text, re.MULTILINE)
    if not block:
        return ["coverage.yml: missing top-level concurrency"]
    body = block.group(1)
    errors = []
    if not re.search(r"^  group: \$\{\{ github\.workflow \}\}-\$\{\{ github\.ref \}\}\s*$", body, re.MULTILINE):
        errors.append("coverage.yml: concurrency must group by workflow and ref")
    if not re.search(r"^  cancel-in-progress: true\s*$", body, re.MULTILINE):
        errors.append("coverage.yml: concurrency must cancel in progress")
    return errors


class TestRustCachePolicy(unittest.TestCase):
    """Pin save-if, standalone workspaces, and this suite's wiring."""

    def setUp(self):
        """Load the real workflows for every test."""
        with open(CI_PATH, encoding="utf-8") as handle:
            self.ci = handle.read()
        with open(COVERAGE_PATH, encoding="utf-8") as handle:
            self.coverage = handle.read()

    def test_real_ci_is_compliant(self):
        """The committed ci.yml satisfies the cache policy."""
        self.assertEqual(cache_errors(self.ci), [])

    def test_dropped_save_if_fails(self):
        """Removing one lane's main-only save-if is caught."""
        mutated = self.ci.replace(
            "          workspaces: workers/mb\n          save-if: " + SAVE_IF_MAIN + "\n",
            "          workspaces: workers/mb\n",
            1,
        )
        self.assertNotEqual(mutated, self.ci)
        self.assertIn("rust-mb: rust-cache must save only on main", cache_errors(mutated))

    def test_widened_save_if_fails(self):
        """A save-if that also saves on PRs is caught."""
        mutated = self.ci.replace(SAVE_IF_MAIN, "true", 1)
        self.assertNotEqual(mutated, self.ci)
        self.assertTrue(cache_errors(mutated))

    def test_each_standalone_workspace_is_required(self):
        """Removing any standalone lane's workspaces input is caught."""
        for job_id, workspace in STANDALONE_WORKSPACES.items():
            with self.subTest(job=job_id):
                chunk = split_jobs(self.ci)[job_id]
                bad = chunk.replace(f"          workspaces: {workspace}\n", "", 1)
                self.assertNotEqual(bad, chunk)
                self.assertIn(
                    f"{job_id}: rust-cache must name workspaces: {workspace}",
                    cache_errors(self.ci.replace(chunk, bad, 1)),
                )

    def test_deleted_cache_step_fails(self):
        """Deleting a standalone lane's cache step is caught."""
        chunk = split_jobs(self.ci)["rust-ingest"]
        step = cache_steps(chunk)[0]
        errors = cache_errors(self.ci.replace(step + "\n", "", 1))
        self.assertIn("rust-ingest: missing rust-cache step", errors)

    def test_real_coverage_is_compliant(self):
        """The committed coverage.yml cancels superseded runs."""
        self.assertEqual(concurrency_errors(self.coverage), [])

    def test_removed_coverage_concurrency_fails(self):
        """Deleting or disabling the coverage concurrency block is caught."""
        removed = re.sub(r"^concurrency:\n(?:  .*\n)+", "", self.coverage, flags=re.MULTILINE)
        self.assertNotEqual(removed, self.coverage)
        self.assertTrue(concurrency_errors(removed))
        disabled = self.coverage.replace("cancel-in-progress: true", "cancel-in-progress: false")
        self.assertNotEqual(disabled, self.coverage)
        self.assertTrue(concurrency_errors(disabled))

    def test_self_step_is_required_in_public_text(self):
        """This suite's public-text step stays required."""
        jobs = split_jobs(self.ci)
        self.assertEqual(check_required_step("public-text", jobs["public-text"], SELF_COMMAND), [])


if __name__ == "__main__":
    unittest.main()
