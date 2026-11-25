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
        deploy = (WORKFLOWS / "deploy.yml").read_text()
        for name in ("staging-api", "staging-mb", "staging-ingest", "staging-stream"):
            with self.subTest(path="deploy.yml", job=name):
                section = job(deploy, name)
                self.assertIn("python3 -I .github/scripts/worker_rust_provenance.py rust", section)
                self.assertNotIn("rustup toolchain install stable", section)

    def test_deployment_gate_groups_do_not_hold_deploy_or_e2e_locks(self):
        for path in ("deploy.yml", "deploy-alerts-staging.yml"):
            with self.subTest(path=path):
                text = (WORKFLOWS / path).read_text()
                # The separate alerts deploy must not hold a run-level lock.
                if path == "deploy-alerts-staging.yml":
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
        self.assertNotIn("wrangler@", production)
        self.assertIn('version="$("$shim/npx" wrangler --version)"', production)
        self.assertIn('test "$version" = "${WRANGLER_VERSION:?}"', production)
        self.assertIn('printf \'%s\\n\' "$shim" >> "$GITHUB_PATH"', production)
        deploy = (WORKFLOWS / "deploy.yml").read_text()
        self.assertEqual(deploy.count('./check_hyperdrive_cache.sh --config-id "$HYPERDRIVE_ID_STAGING"'), 2)
        self.assertNotIn('--config-id "${{ vars.', deploy)
        leaks = (WORKFLOWS / "gitleaks.yml").read_text()
        self.assertIn('if [ "$GITHUB_EVENT_NAME" = pull_request ]; then', leaks)
        self.assertIn('base="refs/remotes/origin/${GITHUB_BASE_REF}"', leaks)
        self.assertNotIn('${{ github.base_ref }}', leaks)

    def test_gitleaks_history_scan_uses_the_fixture_options(self):
        leaks = job((WORKFLOWS / "gitleaks.yml").read_text(), "gitleaks")
        self.assertIn("GITLEAKS_LOG_OPTS: '--text --diff-merges=first-parent HEAD'", leaks)
        scan_name = "- name: Scan the full history\n"
        fixture_name = "- name: Self-test history scanning\n"
        self.assertLess(leaks.index(scan_name), leaks.index(fixture_name))
        self.assertNotIn(".github/scripts/", leaks)
        scan, fixture = leaks.split(scan_name, 1)[1].split(fixture_name, 1)
        for name, step in (("scan", scan), ("fixture", fixture)):
            with self.subTest(step=name):
                self.assertEqual(step.count('"$GITLEAKS_BIN" git '), 1)
                self.assertEqual(step.count('--log-opts='), 1)
                self.assertIn('--log-opts="$GITLEAKS_LOG_OPTS" --ignore-gitleaks-allow', step)

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
        without_presence_checks = re.sub(
            r"secrets\.PRODUCTION_(?:CLOUDFLARE_(?:API_TOKEN|ACCOUNT_ID)|ROLLBACK_(?:CUSTODY|INTENT_KEY)) != ''",
            "", evidence,
        )
        self.assertEqual(
            sorted(re.findall(r"secrets\.([A-Z_]+)", without_presence_checks)),
            ["CF_ACCESS_CLIENT_ID", "CF_ACCESS_CLIENT_SECRET", "STAGING_BASE_URL"],
        )
        self.assertIn("persist-credentials: false", evidence)
        self.assertIn("run: python3 -I ci/staging_admission.py verify", evidence)
        self.assertNotIn("environment:", job(production, "preflight"))
        self.assertIn("      name: production\n", job(production, "deploy-production"))
        self.assertIn("needs: [preflight, staging-evidence]", job(production, "deploy-production"))

    def test_snapshot_spike_is_build_and_recall_only(self):
        text = (WORKFLOWS / "mb-snapshot-spike.yml").read_text()
        self.assertRegex(text, r"(?m)^  workflow_dispatch:$")
        self.assertIn("  pull_request:\n    types: [labeled]", text)
        self.assertIn("github.event.label.name == 'mb-snapshot-spike'", text)
        self.assertIn("permissions:\n  contents: read", text)
        self.assertNotRegex(text, r"(?m)^\s*(stage|environment|workflow_call):")
        self.assertNotRegex(text, r"\b(secrets|inputs\.stage|CLOUDFLARE_\w+|wrangler)\b")

        spike = job(text, "spike")
        self.assertIn("persist-credentials: false", spike)
        for command in ("bash workers/mb/spike/measure.sh", "python3 workers/mb/spike/recall.py"):
            self.assertRegex(spike, rf"(?m)^[ \t]+{re.escape(command)}[ \t]+")
        self.assertRegex(spike, r"(?m)^ {10}name: mb-snapshot-spike-results$")
        for retired in ("stage.sh", "lifecycle.py", "bench.py", "live-archive"):
            self.assertNotIn(retired, text)

        for helper in ("stage.sh", "lifecycle.py", "test_lifecycle.py"):
            self.assertFalse((ROOT / "workers/mb/spike" / helper).exists())
        ci = (WORKFLOWS / "ci.yml").read_text()
        self.assertNotIn("spike/test_lifecycle.py", ci)
        self.assertNotIn("spike/live-archive", ci)

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
        # The evidence step runs only once apply has started: a refused
        # dispatch (validation, checkout or verify failure) stops before
        # the credential is reached, while a started apply that fails
        # still records evidence.
        self.assertIn("if: always() && steps.apply.conclusion != 'skipped'", apply)

    def test_private_trial_schema_requires_a_protected_single_dispatch(self):
        text = (WORKFLOWS / "migrations-staging-apply.yml").read_text()
        apply = job(text, "apply")
        self.assertIn('"$MIGRATION_FILE" == private-trial && "$APPLY_MODE" == single', apply)
        self.assertIn("steps.apply.conclusion != 'skipped' && inputs.file != 'private-trial'", apply)
        for name in ("deploy.yml", "deploy-production.yml"):
            self.assertNotIn("staging_trial_schema", (WORKFLOWS / name).read_text())
            self.assertNotIn("migrations-staging-apply.yml", (WORKFLOWS / name).read_text())

    def test_isolated_trial_setup_uses_protected_dispatch_and_dedicated_credentials(self):
        name = "staging-trial-isolated-setup.yml"
        text = (WORKFLOWS / name).read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertEqual(re.findall(r"^  (\w+):", triggers, re.M), ["workflow_dispatch"])
        self.assertIn("default: check", triggers)
        self.assertIn("group: migrations-staging-apply", text)
        setup = job(text, "setup")
        self.assertIn("github.ref == 'refs/heads/main' && github.ref_protected && github.workflow_sha == github.sha", setup)
        self.assertIn("    environment:\n      name: staging\n      deployment: false\n", setup)
        self.assertIn('"$REVIEWED_REF" == "$GITHUB_SHA"', setup)
        self.assertIn('test "$(git rev-parse HEAD)" = "$GITHUB_SHA"', setup)
        self.assertIn("persist-credentials: false", setup)
        self.assertIn('staging_trial_role_lifecycle.py "$OPERATION" --isolated', setup)
        self.assertEqual(set(re.findall(r"secrets\.(\w+)", setup)),
                         {f"ISOLATED_TRIAL_{key}" for key in
                          ("TARGET", "ROLE_SCOPE", "OWNER_URL", "EXECUTOR_URL", "READER_URL", "WRITER_URL",
                           "AMBIENT_PLAN", "BRANCH_READ")})
        self.assertIn("default: ''", triggers)
        self.assertIn("if: ${{ !endsWith(inputs.operation, '-ambient') }}", setup)
        self.assertIn("if: ${{ endsWith(inputs.operation, '-ambient') }}", setup)
        self.assertIn("AMBIENT_PLAN_SHA256: ${{ inputs.ambient_plan_sha256 }}", setup)

    def test_staging_readback_is_observation_only(self):
        text = (WORKFLOWS / "migrations-staging-readback.yml").read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertIn("workflow_dispatch:", triggers)
        for event in ("pull_request", "push", "schedule"):
            self.assertNotIn(event + ":", triggers)
        self.assertIn('"$WORKFLOW_REF" == refs/heads/main', text)
        self.assertIn("contents: read", text)
        self.assertIn("environment:\n      name: staging\n      deployment: false", text)
        self.assertEqual(text.count("persist-credentials: false"), 2)
        self.assertIn("ref: ${{ inputs.ref }}", text)
        self.assertIn("ref: ${{ github.sha }}", text)
        self.assertIn("git -C data rev-parse HEAD", text)
        self.assertIn("git -C trusted rev-parse HEAD", text)
        self.assertEqual(text.count("staging_apply.py readback"), 1)
        self.assertEqual(text.count("staging_apply.py audit"), 1)
        self.assertIn("staging_apply.py audit --trusted-root trusted --data-root data", text)
        self.assertIn("AUDIT_RECEIPT: ledger-audit-receipt.json", text)
        self.assertIn("path: ledger-audit-receipt.json", text)
        self.assertIn("default: readback", text)
        self.assertEqual(text.count("if: ${{ inputs.mode == 'audit' }}"), 2)
        self.assertEqual(text.count("if: ${{ inputs.mode == 'readback' }}"), 2)
        for forbidden in ("staging_apply.py apply", "apply_migration.py", "all-pending", "--record-only", "$GITHUB_ENV"):
            self.assertNotIn(forbidden, text)
        self.assertIn("path: readback-receipt.json", text)
        self.assertIn("if-no-files-found: error", text)
        self.assertIn("group: migrations-staging-apply", text)
        self.assertIn("if: ${{ inputs.mode != 'purge-census' }}", text.split("Check out trusted code")[0])
        self.assertIn('test "$REF_PROTECTED" = true', text)
        self.assertIn('test "$WORKFLOW_SHA" = "$TRUSTED_REF"', text)
        census = text.split("- name: Observe staging purge census", 1)[1]
        self.assertEqual(census.count("if: ${{ inputs.mode == 'purge-census' }}"), 2)
        self.assertIn("staging_apply.py purge-census", census)
        self.assertNotRegex(census, r"secrets\.|--data-root")
        self.assertIn("path: purge-census-receipt.json", census)

    def test_provider_preflight_refuses_untrusted_triggers_and_isolates_credentials(self):
        text = (WORKFLOWS / "staging-trial-provider-preflight.yml").read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertEqual(re.findall(r"^  (\w+):", triggers, re.M), ["workflow_dispatch"])
        for field in ("ref", "scope_sha256"):
            self.assertRegex(triggers, rf"(?m)^      {field}:\n[^\n]+\n        type: string\n        required: true$")
        self.assertEqual(re.findall(r"^permissions:\n((?:  [^\n]+\n)+)", text, re.M), ["  contents: read\n"])
        self.assertEqual(re.findall(r"^  ([\w-]+):\n", text.split("\njobs:\n", 1)[1], re.M), ["preflight"])
        preflight = job(text, "preflight")
        admission = re.search(r"    if: >-\n(.*?)\n    runs-on:", preflight, re.S).group(1)
        self.assertEqual(" ".join(admission.split()),
                         "${{ github.event_name == 'workflow_dispatch' && "
                         "github.repository == 'TogetherWeOwn/nntunev2' && "
                         "github.ref == 'refs/heads/main' && github.ref_protected && "
                         "github.workflow_sha == github.sha && github.run_attempt == 1 }}")
        self.assertIn("environment:\n      name: staging\n      deployment: false", preflight)
        self.assertIn('[[ "$REVIEWED_REF" =~ ^[0-9a-f]{40}$ && "$REVIEWED_REF" == "$GITHUB_SHA" ]]', preflight)
        self.assertIn('[[ "$SCOPE_SHA256" =~ ^[0-9a-f]{64}$ ]]', preflight)
        setup, probe = preflight.split("- name: Run the bound disposable provider probe\n")
        self.assertNotIn("secrets.", text.split("- name: Run the bound disposable provider probe\n")[0])
        self.assertIn("ref: ${{ github.sha }}\n          persist-credentials: false", setup)
        self.assertIn('test "$(git rev-parse HEAD)" = "$GITHUB_SHA"', setup)
        self.assertIn("cargo build --locked --manifest-path workers/ingest/Cargo.toml", setup)
        self.assertIn("--no-default-features --features fixture-provider-preflight", setup)
        self.assertIn('"$WASM_BINDGEN_BIN" --target nodejs', setup)
        match, settings = probe.split("          PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256:", 1)
        self.assertEqual(" ".join(match.split()), "env: PRIVATE_TRIAL_PREFLIGHT_PROVIDER_MATCH: >- "
                         "${{ vars.PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT != '' && "
                         "vars.PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT == secrets.PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT && "
                         "vars.PRIVATE_PLAYBACK_STAGING_S3_REGION != '' && "
                         "vars.PRIVATE_PLAYBACK_STAGING_S3_REGION == secrets.PRIVATE_PLAYBACK_STAGING_S3_REGION }}")
        self.assertNotIn("vars.", settings)
        self.assertEqual(re.findall(r"secrets\.(\w+)", settings), [
            "PRIVATE_TRIAL_PREFLIGHT_SCOPE", "PRIVATE_TRIAL_PREFLIGHT_S3",
            "PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT", "PRIVATE_PLAYBACK_STAGING_S3_REGION",
        ])
        self.assertIn("PRIVATE_TRIAL_PREFLIGHT_SCOPE_SHA256: ${{ inputs.scope_sha256 }}", probe)
        self.assertIn('--provider-dispatch "$RUNNER_TEMP/trial-provider-preflight/nntune_ingest.js" 2>/dev/null', probe)
        self.assertNotRegex(probe, r"(?m)^      - ")
        self.assertNotRegex(text, r"upload-artifact|continue-on-error|\b(?:wrangler|psql)\b")

    def test_codeql_reports_on_every_pull_request(self):
        # The code_scanning ruleset waits for CodeQL on every PR; a path filter
        # leaves Rust/docs-only PRs waiting forever.
        text = (WORKFLOWS / "codeql.yml").read_text()
        triggers = re.search(r"^on:\n(.*?)(?=^\S)", text, re.M | re.S).group(1)
        self.assertIn("  pull_request:\n", triggers)
        self.assertNotRegex(triggers, r"(?m)^\s+paths(-ignore)?:")


if __name__ == "__main__":
    unittest.main()
