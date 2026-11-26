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
            "# guards: required:ci-ok\non: {pull_request: {}, push: {branches: ['**', '!main']}}",
            "# guards: required:proof\non: {workflow_run: {workflows: [CI], types: [completed]}}",
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


class RequiredLintTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = (ACTIONS / "pr-lint.yml").read_text()
        cls.doc = yaml.load(cls.text, Loader=WorkflowLoader)
        cls.job = cls.doc["jobs"]["pr-lint"]
        cls.title_command = cls.job["steps"][2]["run"]

    def test_guard_runs_on_pinned_tree_in_existing_required_job(self):
        checkout, guard, _ = self.job["steps"]
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
            self.assertEqual({"types": ["opened", "synchronize", "reopened"]}, events["pull_request"])
            self.assertIn("merge_group", events)
        events = workflows(self.text)
        self.assertEqual(["main"], events["push"]["branches"])
        self.assertEqual("true", events["workflow_dispatch"]["inputs"]["pr_number"]["required"])
        codeql = workflows((ACTIONS / "codeql.yml").read_text())
        self.assertEqual({"pull_request", "merge_group", "schedule"}, set(codeql))
        self.assertNotIn("paths", codeql["pull_request"])

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
