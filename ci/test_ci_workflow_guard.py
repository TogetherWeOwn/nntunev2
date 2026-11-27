"""CI demand policy fixtures and required pr-lint execution checks."""

import os
from pathlib import Path
import subprocess
import unittest

import yaml

from ci_workflow_guard import WorkflowLoader, findings, workflows


ROOT = Path(__file__).resolve().parents[1]
ACTIONS = ROOT / ".github/workflows"


class GuardFixtures(unittest.TestCase):
    def test_supported_triggers_and_guards(self):
        fixtures = (
            "on: workflow_dispatch",
            "on: [schedule, workflow_dispatch]",
            "# guards: required:ci-ok\non: pull_request",
            '# guards: paths\n"on": {pull_request: {paths: [src/**]}}',
            "# guards: paths\non: {push: {branches: [main], paths: [src/**]}}",
            "# guards: release:exact staging SHA\non: [pull_request, push]",
            "# guards: required:ci-ok\non: {pull_request: {}, push: {branches: [release]}}",
            "# guards: required:ci-ok\non: {pull_request: {}, push: {branches: ['release/v[0-9]+']}}",
            "# guards: required:ci-ok\non: {pull_request: {}, push: {branches: ['**', '!main']}}",
            "# guards: required:proof\non: {workflow_run: {workflows: [CI], types: [completed]}}",
            "# guards: required:proof\non: {workflow_run: {workflows: [CI], types: completed}}",
        )
        for fixture in fixtures:
            with self.subTest(fixture=fixture):
                self.assertEqual([], list(findings(fixture)))

    def test_rejects_redundant_or_unscoped_runs(self):
        fixtures = (
            ("on: pull_request", "exactly one"),
            ("# guards: paths-other\non: push", "exactly one"),
            ("# guards: required:\non: push", "exactly one"),
            ("# guards: paths\n# guards: required:foo\non: push", "exactly one"),
            ("# guards: paths\non: {pull_request: {paths: [src/**]}, push: {branches: [dev]}}",
             "push declares guards: paths"),
            ("# guards: required:pr-lint\non: {pull_request: {types: [opened, edited]}}", "edited"),
            ("# guards: required:pr-lint\non: {pull_request_target: {types: [edited]}}", "edited"),
            ("# guards: release:staging\non: {workflow_run: {types: [requested, in_progress]}}",
             "types: [completed]"),
            ("# guards: release:staging\non: {workflow_run: {workflows: [ci]}}", "types: [completed]"),
            ("# guards: required:ci-ok\non: [push, pull_request]", "PR and main-push"),
            ("# guards: required:ci-ok\non: {pull_request: {}, push: {branches: ['m*']}}",
             "PR and main-push"),
            ("# guards: required:ci-ok\non: {pull_request: {}, push: {branches-ignore: [dev]}}",
             "PR and main-push"),
        )
        for fixture, expected in fixtures:
            with self.subTest(fixture=fixture):
                self.assertTrue(any(expected in error for error in findings(fixture)))

    def test_ambiguous_triggers_fail_closed(self):
        with self.assertRaisesRegex(yaml.YAMLError, "duplicate"):
            list(findings("on: pull_request\n'on': workflow_dispatch"))
        with self.assertRaisesRegex(yaml.YAMLError, "merge keys"):
            list(findings("on: {pull_request: {<<: {types: [edited]}}}"))

    def test_malformed_filters_fail_closed(self):
        for trigger in (
            "[]", "[push, push]", "{push: {paths: src/**}}", "{push: {paths: [[], src/**]}}",
            "{push: {branches: [main], branches-ignore: [main], paths: [src/**]}}",
            "{pull_request: {types: {opened: yes}, paths: [src/**]}}",
        ):
            with self.subTest(trigger=trigger), self.assertRaises(ValueError):
                list(findings(f"# guards: paths\non: {trigger}"))


class TrustedPRRunnerTests(unittest.TestCase):
    def test_runner_selection_stays_scoped_to_trusted_prs(self):
        accounts = "fromJSON('[\"Rick7C2\", \"togetherweown[bot]\"]')"
        guard = (
            "github.event_name == 'pull_request' && github.run_attempt == 1"
            " && github.event.pull_request.head.repo.full_name == github.repository"
            f" && contains({accounts}, github.actor)"
            f" && contains({accounts}, github.triggering_actor)"
            f" && contains({accounts}, github.event.pull_request.user.login)"
            " && vars.CI_BLACKSMITH_2VCPU == 'blacksmith-2vcpu-ubuntu-2404'"
            " && vars.CI_BLACKSMITH_2VCPU"
        )
        latest = guard + " || 'ubuntu-latest'"
        pinned = guard + " || 'ubuntu-24.04'"
        expected = {
            "ci": dict.fromkeys((
                "early", "fixture-check", "nzb-guard", "rust-wasm", "cargo-deny",
                "public-text", "cargo-pool", "binding-parity", "migrations-gate",
                "test-nntp", "mb-import-etl", "rust-mb", "config-parity",
                "dependabot-coverage", "rust-repair", "rust-transcode", "cache-policy",
                "production-secrets-shape", "release-proof", "docs-links", "web-checks",
                "web-build", "history-fixture", "web-a11y", "tail-projection",
                "changed-lines-mutation", "ci-ok",
            ), latest) | {"ci-commits": pinned},
            "gitleaks": {"gitleaks": latest},
            "migrations-staging": {"migrations-staging": latest},
            "pr-lint": {"pr-lint": pinned},
            "pr-size": {"pr-size-gate": f"toJSON({latest})"},
        }
        for workflow, jobs in expected.items():
            doc = yaml.load((ACTIONS / f"{workflow}.yml").read_text(), Loader=WorkflowLoader)
            selectors = {}
            for job_id, job in doc["jobs"].items():
                runner = job.get("runs-on", job.get("with", {}).get("runs-on", ""))
                # The existing WASM budget route has its own policy.
                if job_id != "worker-wasm-budget" and "CI_BLACKSMITH_2VCPU" in runner:
                    selectors[job_id] = runner
            with self.subTest(workflow=workflow):
                self.assertEqual(selectors, {
                    job_id: "${{ " + expression + " }}" for job_id, expression in jobs.items()
                })


class RequiredLintTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (ACTIONS / "pr-lint.yml").read_text()
        cls.doc = yaml.load(cls.text, Loader=WorkflowLoader)
        cls.job = cls.doc["jobs"]["pr-lint"]
        cls.title_command = cls.job["steps"][0]["run"]

    def test_guard_runs_on_pinned_tree_in_existing_required_job(self):
        title, checkout, guard = self.job["steps"]
        self.assertEqual("Check PR title convention", title["name"])
        self.assertIn("github.event_name == 'workflow_dispatch'", title["env"]["GH_TOKEN"])
        self.assertEqual("pr-lint", self.job["name"])
        self.assertEqual({"contents": "read", "pull-requests": "read"}, self.doc["permissions"])
        self.assertEqual("${{ github.sha }}", checkout["with"]["ref"])
        self.assertEqual("false", checkout["with"]["persist-credentials"])
        self.assertNotIn("if", guard)
        self.assertNotIn("continue-on-error", guard)
        self.assertIn("-I ci/ci_workflow_guard.py .", guard["run"])
        self.assertIn("--require-hashes", guard["run"])

    def test_supported_events_preserve_reopen_sync_main_and_merge_group(self):
        for name in ("migrations-staging", "pr-lint"):
            events = workflows((ACTIONS / f"{name}.yml").read_text())
            self.assertEqual({"types": ["opened", "synchronize", "reopened"], "branches": ["main"]},
                             events["pull_request"])
            self.assertIn("merge_group", events)
        events = workflows(self.text)
        self.assertEqual(["main"], events["push"]["branches"])
        self.assertEqual("true", events["workflow_dispatch"]["inputs"]["pr_number"]["required"])
        codeql = workflows((ACTIONS / "codeql.yml").read_text())
        self.assertEqual({"pull_request", "merge_group", "schedule", "workflow_dispatch"}, set(codeql))
        self.assertNotIn("paths", codeql["pull_request"])
        coverage = workflows((ACTIONS / "coverage.yml").read_text())
        self.assertEqual({"schedule", "workflow_dispatch"}, set(coverage))

    def test_title_event_dispatch_input_push_and_merge_group(self):
        cases = (
            ("pull_request", {"EVENT_TITLE": "ci: refresh after metadata correction"}, 0),
            ("pull_request", {"EVENT_TITLE": "Bad title."}, 1),
            ("workflow_dispatch", {"INPUT_PR": "42"}, 0),
            ("workflow_dispatch", {"INPUT_PR": "43"}, 71),
            ("push", {"EVENT_COMMITS": '[{"message":"ci: validate release squash"}]'}, 0),
            ("push", {"EVENT_COMMITS": '[{"message":"Bad squash."}]'}, 1),
            ("merge_group", {"EVENT_COMMITS": "null"}, 0),
        )
        gh = '''gh() {
          [ "$*" = "api repos/synthetic/repo/pulls/42 --jq .title" ] || return 71
          echo "ci: validate selected PR"
        }
        '''
        for event, payload, code in cases:
            with self.subTest(event=event, payload=payload):
                result = subprocess.run(
                    ["bash", "-euo", "pipefail", "-c", gh + self.title_command],
                    env={"PATH": os.environ["PATH"], "REPO": "synthetic/repo",
                         "EVENT_NAME": event, **payload},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(code, result.returncode, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
