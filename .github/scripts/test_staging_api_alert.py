"""Unit tests for the staging-api red-deploy tracking issue (no network)."""

import importlib.util
import pathlib
import re
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "staging_api_alert", REPO_ROOT / ".github/scripts/staging_api_alert.py"
)
alert = importlib.util.module_from_spec(spec)
spec.loader.exec_module(alert)

ENV = {
    "GITHUB_REPOSITORY": "o/r",
    "GITHUB_SHA": "a" * 40,
    "GITHUB_RUN_ID": "42",
    "GITHUB_SERVER_URL": "https://github.com",
}


class FakeGitHub:
    def __init__(self, issues=()):
        self.issues = list(issues)
        self.calls = []

    def __call__(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        if method == "GET":
            return self.issues
        if method == "POST" and path == "/repos/o/r/issues":
            return {"number": 7}
        return {}


class StagingApiAlertTest(unittest.TestCase):
    def test_failure_opens_issue_with_run_link(self):
        fake = FakeGitHub()
        self.assertEqual(0, alert.main(["failed"], ENV, fake))
        method, path, payload = fake.calls[-1]
        self.assertEqual(("POST", "/repos/o/r/issues"), (method, path))
        self.assertEqual(alert.TITLE, payload["title"])
        self.assertIn("https://github.com/o/r/actions/runs/42", payload["body"])
        self.assertIn("a" * 40, payload["body"])

    def test_repeat_failure_comments_instead_of_duplicating(self):
        fake = FakeGitHub([{"number": 3, "title": alert.TITLE}])
        self.assertEqual(0, alert.main(["failed"], ENV, fake))
        self.assertEqual([("GET",), ("POST",)], [c[:1] for c in fake.calls])
        self.assertEqual("/repos/o/r/issues/3/comments", fake.calls[-1][1])

    def test_same_titled_pull_request_is_not_the_tracking_issue(self):
        fake = FakeGitHub([{"number": 3, "title": alert.TITLE, "pull_request": {}}])
        alert.main(["failed"], ENV, fake)
        self.assertEqual("/repos/o/r/issues", fake.calls[-1][1])

    def test_recovery_comments_and_closes(self):
        fake = FakeGitHub([{"number": 9, "title": "other"}, {"number": 3, "title": alert.TITLE}])
        self.assertEqual(0, alert.main(["recovered"], ENV, fake))
        self.assertEqual("/repos/o/r/issues/3/comments", fake.calls[1][1])
        self.assertEqual(("PATCH", "/repos/o/r/issues/3"), fake.calls[2][:2])
        self.assertEqual("closed", fake.calls[2][2]["state"])

    def test_recovery_without_open_issue_is_a_no_op(self):
        fake = FakeGitHub()
        self.assertEqual(0, alert.main(["recovered"], ENV, fake))
        self.assertEqual(["GET"], [c[0] for c in fake.calls])

    def test_bad_usage_and_api_errors_fail(self):
        self.assertEqual(2, alert.main([], ENV, FakeGitHub()))
        self.assertEqual(1, alert.main(["bogus"], ENV, FakeGitHub()))

        def broken(method, path, payload=None):
            raise OSError("HTTP Error 403: Forbidden")

        self.assertEqual(1, alert.main(["failed"], ENV, broken))

    def test_missing_token_refuses_before_network(self):
        self.assertEqual(2, alert.main(["failed"], ENV))


class DeployWorkflowWiringTest(unittest.TestCase):
    def test_deploy_yml_alerts_on_red_staging_api(self):
        text = (REPO_ROOT / ".github/workflows/deploy.yml").read_text(encoding="utf-8")
        match = re.search(r"\n  staging-api-alert:\n(.*?)(?=\n  [a-z][a-z0-9-]*:\n|\Z)", text, re.DOTALL)
        self.assertIsNotNone(match, "deploy.yml must define a staging-api-alert job")
        job = match.group(1)
        self.assertIn("needs: [staging-api]", job)
        self.assertIn("always()", job)
        self.assertIn("needs.staging-api.result == 'failure'", job)
        self.assertIn("issues: write", job)
        self.assertIn("staging_api_alert.py failed", job)
        self.assertIn("staging_api_alert.py recovered", job)


if __name__ == "__main__":
    unittest.main()
