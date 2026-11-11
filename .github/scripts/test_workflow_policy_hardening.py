"""Regression tests for workflow setup, shell data and deploy cache boundaries."""

from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"


def job(text, name):
    match = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
    if not match:
        raise AssertionError(f"missing job {name}")
    return match.group(1)


class WorkflowPolicyHardeningTests(unittest.TestCase):
    def test_native_rustup_keeps_each_jobs_components_and_targets(self):
        expected = {
            "ci.yml": {
                "rust-test": ("rustfmt,clippy", None),
                "rust-wasm": ("clippy", "wasm32-unknown-unknown"),
                "cargo-deny": (None, None),
                "worker-wasm-budget": (None, "wasm32-unknown-unknown"),
                "worker-wasm-sizes": (None, "wasm32-unknown-unknown"),
                "test-nntp": ("rustfmt,clippy", None),
                "api": ("rustfmt,clippy", "wasm32-unknown-unknown"),
                "api-migrations": (None, None),
                "rust-mb": ("rustfmt,clippy", "wasm32-unknown-unknown"),
                "rust-ingest": ("rustfmt,clippy", "wasm32-unknown-unknown"),
                "rust-repair": ("rustfmt,clippy", "wasm32-unknown-unknown"),
                "rust-transcode": ("rustfmt,clippy", "wasm32-unknown-unknown"),
            },
            "coverage.yml": {"coverage": ("llvm-tools-preview", None)},
            "deploy.yml": {
                "staging-api": (None, "wasm32-unknown-unknown"),
                "staging-mb": (None, "wasm32-unknown-unknown"),
                "staging-ingest": (None, "wasm32-unknown-unknown"),
            },
        }
        for path, jobs in expected.items():
            text = (WORKFLOWS / path).read_text()
            self.assertNotIn("uses: dtolnay/rust-toolchain@", text)
            for name, (components, target) in jobs.items():
                with self.subTest(path=path, job=name):
                    section = job(text, name)
                    command = "rustup toolchain install stable --profile minimal --no-self-update"
                    if components:
                        command += f" --component {components}"
                    if target:
                        command += f" --target {target}"
                    self.assertIn(command + "\n          rustup default stable\n", section)

    def test_deployment_gate_groups_do_not_hold_deploy_or_e2e_locks(self):
        for path in ("deploy.yml", "deploy-alerts-staging.yml"):
            with self.subTest(path=path):
                text = (WORKFLOWS / path).read_text()
                self.assertNotRegex(text, r"(?m)^concurrency:")
                gate = job(text, "gate")
                self.assertIn("group: ${{ github.workflow }}-gate-${{ github.ref }}", gate)
                self.assertIn("cancel-in-progress: false", gate)
                self.assertIn("queue: max", gate)

    def test_staging_build_does_not_restore_or_save_package_caches(self):
        web = job((WORKFLOWS / "deploy.yml").read_text(), "staging-web")
        self.assertIn("package-manager-cache: false", web)
        self.assertNotRegex(web, r"(?m)^\s+cache:")
        self.assertIn("pnpm install --frozen-lockfile", web)
        self.assertIn("command: deploy --env staging", web)

    def test_shell_uses_environment_data_not_source_interpolation(self):
        ci = (WORKFLOWS / "ci.yml").read_text()
        for script in ("check_web_bundle_budget.py", "test_check_web_bundle_budget.py"):
            self.assertIn(f'python3 "$GITHUB_WORKSPACE/.github/scripts/{script}"', ci)
        production = (WORKFLOWS / "deploy-production.yml").read_text()
        self.assertNotIn("wrangler@${{ env.WRANGLER_VERSION }}", production)
        self.assertEqual(production.count('"wrangler@${WRANGLER_VERSION:?}"'), 3)
        deploy = (WORKFLOWS / "deploy.yml").read_text()
        self.assertEqual(deploy.count('./check_hyperdrive_cache.sh --config-id "$HYPERDRIVE_ID_STAGING"'), 2)
        self.assertNotIn('--config-id "${{ vars.', deploy)
        leaks = (WORKFLOWS / "gitleaks.yml").read_text()
        self.assertIn('if [ "$GITHUB_EVENT_NAME" = pull_request ]; then', leaks)
        self.assertIn('base="refs/remotes/origin/${GITHUB_BASE_REF}"', leaks)
        self.assertNotIn('${{ github.base_ref }}', leaks)

    def test_local_workflows_use_the_same_source_revision(self):
        for path, callee in (("deploy.yml", "e2e-staging.yml"), ("deploy-production.yml", "release.yml")):
            with self.subTest(path=path):
                text = (WORKFLOWS / path).read_text()
                self.assertIn(f"uses: $/.github/workflows/{callee}\n", text)
                self.assertNotIn(f"uses: ./.github/workflows/{callee}", text)

    def test_candidate_evidence_uses_staging_without_a_deployment(self):
        production = (WORKFLOWS / "deploy-production.yml").read_text()
        evidence = job(production, "staging-evidence")
        self.assertIn("    environment:\n      name: staging\n      deployment: false\n", evidence)
        self.assertEqual(
            sorted(re.findall(r"secrets\.([A-Z_]+)", evidence)),
            ["CF_ACCESS_CLIENT_ID", "CF_ACCESS_CLIENT_SECRET", "STAGING_BASE_URL"],
        )
        self.assertIn("persist-credentials: false", evidence)
        self.assertIn("run: python3 -I ci/staging_admission.py verify", evidence)
        self.assertNotIn("environment:", job(production, "preflight"))
        self.assertIn("      name: production\n", job(production, "deploy-production"))
        self.assertIn("needs: [preflight, staging-evidence]", job(production, "deploy-production"))

    def test_regression_suite_is_a_required_ci_step(self):
        ci = (WORKFLOWS / "ci.yml").read_text()
        self.assertIn("python3 .github/scripts/test_workflow_policy_hardening.py", job(ci, "public-text"))
        self.assertIn("public-text", job(ci, "ci-ok"))

    def test_staging_apply_is_agent_dispatchable_and_pinned(self):
        # The authorized agent dispatches this workflow by hand after board
        # approval; it must pin the exact ref, target only staging, never
        # print its credential, and record the receipt as evidence.
        text = (WORKFLOWS / "migrations-staging-apply.yml").read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertIn("workflow_dispatch:", triggers)
        for event in ("pull_request", "push", "schedule"):
            self.assertNotIn(event + ":", triggers)
        for name in ("ref:", "file:", "applied_by:", "board_approval:", "mode:"):
            self.assertIn(name, triggers)
        self.assertIn("group: migrations-staging-apply", text)
        self.assertIn("cancel-in-progress: false", text)
        apply = job(text, "apply")
        self.assertIn("    environment:\n      name: staging\n", apply)
        self.assertIn("ref: ${{ inputs.ref }}", apply)
        # Two checkouts: dispatch-supplied ref as data only, the workflow's
        # own ref as the only executed code.
        self.assertIn("path: data", apply)
        self.assertIn("path: trusted", apply)
        self.assertIn("ref: ${{ github.sha }}", apply)
        self.assertEqual(apply.count("persist-credentials: false"), 2)
        self.assertIn("staging_apply.py", apply)
        self.assertIn("--expect-database", apply)
        self.assertIn("nntunev2_staging", apply)
        self.assertIn("secrets.PLANETSCALE_MIGRATOR_URL_STAGING", apply)
        # The credential is env-only: no echo of the secret and no GITHUB_ENV
        # hand-off of a derived URL (masking would not cover a modified copy).
        self.assertNotIn('echo "$PLANETSCALE', apply)
        self.assertNotIn("$GITHUB_ENV", apply)
        # Main-branch dispatch guard: the trusted code runs only from the
        # protected branch, so a dispatch from any other branch is refused
        # before the checkouts or the credential are reached.
        self.assertIn("WORKFLOW_REF: ${{ github.ref }}", apply)
        self.assertIn('"$WORKFLOW_REF" != "refs/heads/main"', apply)
        self.assertIn("must be dispatched from refs/heads/main; refusing", apply)
        # Both driver invocations run with -E -s (no PYTHON* environment and
        # no user site directory) so dispatch-supplied environment cannot
        # reach the trusted driver; any bare python3 invocation fails here.
        self.assertEqual(
            apply.count("python3 -E -s trusted/.github/scripts/staging_apply.py"),
            2,
        )
        self.assertEqual(apply.count("python3 "), 2)
        # all-pending walks manifest order and refuses on the first failure.
        self.assertIn("all-pending", apply)

    def test_codeql_reports_on_every_pull_request(self):
        # The code_scanning ruleset waits for CodeQL on every PR; a path filter
        # leaves Rust/docs-only PRs waiting forever.
        text = (WORKFLOWS / "codeql.yml").read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertIn("  pull_request:\n", triggers)
        self.assertNotRegex(triggers, r"(?m)^\s+paths(-ignore)?:")


if __name__ == "__main__":
    unittest.main()
