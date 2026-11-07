"""Unit tests for incident_report (stdlib unittest, fake API, no network).

Run with `python3 ci/test_incident_report.py`.
"""

import importlib.util
import os
import re
import unittest

CI_DIR = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "incident_report", os.path.join(CI_DIR, "incident_report.py"))
ir = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ir)

SHA = "a" * 40
SMOKE = "synthetic smoke (health)"
MARKER = "<!-- incident-key: e2e-staging.yml:synthetic-smoke-health -->"


def make_run(run_id=10, attempt=1, workflow="e2e-staging.yml", **over):
    run = {"id": run_id, "run_attempt": attempt, "status": "completed",
           "path": ".github/workflows/" + workflow, "head_branch": "main",
           "head_repository": {"full_name": ir.REPO}, "event": "schedule",
           "head_sha": SHA}
    run.update(over)
    return run


def smoke(conclusion, step="success", started="2026-01-02T00:00:00Z"):
    return {"name": SMOKE, "conclusion": conclusion, "started_at": started,
            "steps": [{"number": 3, "name": "Run synthetic smoke", "conclusion": step}]}


def issue(number, updated="2026-01-01T00:00:00Z", body=MARKER, **extra):
    return {"number": number, "body": body, "updated_at": updated, **extra}


class FakeAPI(ir.API):
    """Real pagination, canned GETs; records writes."""

    def __init__(self, run=None, jobs=(), issues=(), runs=None, current=None, fail=None):
        super().__init__("token")
        self.run = run or make_run()
        self.current = current or self.run
        self.jobs, self.issues = list(jobs), list(issues)
        self.runs = [self.run] if runs is None else runs
        self.fail, self.writes, self.run_gets = fail, [], 0

    def request(self, method, path, body=None):
        if self.fail and self.fail in path:
            raise ir.ReportError("GitHub GET refused (HTTP 500)")
        if method != "GET":
            self.writes.append((method, path, body))
            return None
        page = int(m.group(1)) if (m := re.search(r"[?&]page=(\d+)", path)) else 1
        window = slice((page - 1) * 100, page * 100)
        if re.fullmatch(r"/actions/runs/\d+", path):
            self.run_gets += 1
            return self.run if self.run_gets == 1 else self.current
        if "/jobs" in path:
            return {"jobs": self.jobs[window]}
        if path.startswith("/actions/workflows/"):
            return {"workflow_runs": self.runs[window]}
        return self.issues[window]


class ReportTests(unittest.TestCase):
    def test_failure_opens_then_repeat_comments(self):
        api = FakeAPI(jobs=[smoke("failure")])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])
        method, path, body = api.writes[0]
        self.assertEqual((method, path), ("POST", "/issues"))
        self.assertIn(MARKER, body["body"])
        self.assertEqual(body["labels"], ["incident"])
        api = FakeAPI(jobs=[smoke("failure")], issues=[issue(7), issue(9)])
        self.assertEqual(ir.report(api, 10), [f"commented #7: {SMOKE}"])
        self.assertEqual([w[1] for w in api.writes], ["/issues/7/comments"])

    def test_pull_request_issue_is_not_a_match(self):
        api = FakeAPI(jobs=[smoke("failure")], issues=[issue(7, pull_request={})])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])

    def test_recovery_comments_and_closes(self):
        api = FakeAPI(jobs=[smoke("success")], issues=[issue(7)])
        self.assertEqual(ir.report(api, 10), [f"recovered #7: {SMOKE}"])
        self.assertEqual([w[:2] for w in api.writes],
                         [("POST", "/issues/7/comments"), ("PATCH", "/issues/7")])
        self.assertEqual(api.writes[1][2]["state"], "closed")

    def test_recovery_held_when_issue_has_newer_activity_or_bad_time(self):
        for updated in ("2026-01-03T00:00:00Z", "garbage"):
            api = FakeAPI(jobs=[smoke("success")], issues=[issue(7, updated=updated)])
            self.assertEqual(ir.report(api, 10), ["held #7: newer activity"])
            self.assertEqual(api.writes, [])

    def test_skipped_or_unevidenced_success_does_not_recover(self):
        for job in (smoke("skipped"), smoke("success", step="skipped"),
                    smoke("neutral"), {**smoke("success"), "steps": []}):
            api = FakeAPI(jobs=[job], issues=[issue(7)])
            self.assertEqual(ir.report(api, 10), [])
            self.assertEqual(api.writes, [])

    def test_newer_run_or_attempt_is_superseded(self):
        for newer in (make_run(11), make_run(10, attempt=2)):
            api = FakeAPI(jobs=[smoke("success")], issues=[issue(7)], runs=[make_run(), newer])
            self.assertEqual(ir.report(api, 10), ["superseded"])
            self.assertEqual(api.writes, [])

    def test_untrusted_newer_run_does_not_supersede(self):
        api = FakeAPI(jobs=[smoke("failure")], runs=[make_run(), make_run(11, head_branch="x")])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])

    def test_attempt_changed_or_incomplete_on_recheck_is_superseded(self):
        for current in (make_run(attempt=2), make_run(status="in_progress")):
            api = FakeAPI(jobs=[smoke("failure")], current=current)
            self.assertEqual(ir.report(api, 10), ["superseded"])
            self.assertEqual(api.writes, [])

    def test_untrusted_runs_rejected(self):
        bad = [{"head_repository": {"full_name": "evil/fork"}}, {"head_branch": "feature"},
               {"event": "pull_request"}, {"status": "in_progress"}, {"head_sha": "abc"},
               {"path": ".github/workflows/ci.yml"}, {"event": "schedule", "path":
                ".github/workflows/deploy.yml"}, {"event": "push", "path":
                ".github/workflows/deploy-production.yml"}]
        for over in bad:
            api = FakeAPI(run=make_run(**over), jobs=[smoke("failure")])
            with self.assertRaises(ir.ReportError, msg=over):
                ir.report(api, 10)
            self.assertEqual(api.writes, [])

    def test_jobs_paginate_and_find_gate_on_later_page(self):
        filler = [{"name": f"j{i}", "steps": []} for i in range(100)]
        api = FakeAPI(jobs=filler + [smoke("failure")])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])

    def test_duplicate_or_missing_gate_job_is_ignored(self):
        for jobs in ([smoke("failure"), smoke("failure")], []):
            api = FakeAPI(jobs=jobs)
            self.assertEqual(ir.report(api, 10), [])
            self.assertEqual(api.writes, [])

    def test_api_errors_propagate_without_writes(self):
        for fail in ("/jobs", "/issues?", "/actions/workflows/"):
            api = FakeAPI(jobs=[smoke("failure")], fail=fail)
            with self.assertRaises(ir.ReportError):
                ir.report(api, 10)
            self.assertEqual(api.writes, [])

    def test_pagination_bound_and_malformed_list(self):
        api = FakeAPI(jobs=[{"name": "x"}] * 1000)
        with self.assertRaises(ir.ReportError):
            api.pages("/actions/runs/1/attempts/1/jobs", "jobs")
        api.request = lambda *a, **k: {"jobs": "nope"}
        with self.assertRaises(ir.ReportError):
            api.pages("/x", "jobs")

    def test_dry_run_reports_but_never_writes(self):
        for jobs, issues in (([smoke("failure")], []), ([smoke("failure")], [issue(7)]),
                             ([smoke("success")], [issue(7)])):
            api = FakeAPI(jobs=jobs, issues=issues)
            self.assertTrue(ir.report(api, 10, dry_run=True))
            self.assertEqual(api.writes, [])

    def test_past_failed_journey_replay_plans_open_without_writes(self):
        # Bounded public jobs-API facts, not logs or artifacts:
        # https://github.com/TogetherWeOwn/nntunev2/actions/runs/42334183409
        name = "post-deploy-e2e / full journey (signup → library → meter → limits)"
        run = make_run(42334183409, workflow="deploy.yml", event="push",
                       head_sha="d5a22bc5931a0c6a17deaace0dfedcb6d691b714")
        job = {"name": name, "conclusion": "failure", "steps": [
            {"number": 7, "name": "Run full journey", "conclusion": "failure"},
            {"number": 8, "name": "Enforce journey evidence gate (all-skipped must fail)",
             "conclusion": "failure"}]}
        api = FakeAPI(run=run, jobs=[job])
        self.assertEqual(ir.report(api, run["id"], dry_run=True), ["opened: " + name])
        self.assertEqual(api.writes, [])

    def test_configuration_gate_recovers_only_after_actual_web_deploy(self):
        run = make_run(workflow="deploy.yml", event="push")
        key = "<!-- incident-key: deploy.yml:gate -->"
        gate = {"name": "gate", "conclusion": "success", "steps": [],
                "started_at": "2026-01-02T00:00:00Z"}
        api = FakeAPI(run=run, jobs=[gate], issues=[issue(7, body=key)])
        self.assertEqual(ir.report(api, 10), [])
        web = {"name": "staging-web", "conclusion": "success", "steps": [
            {"name": "Deploy to staging", "conclusion": "success"}]}
        api = FakeAPI(run=run, jobs=[gate, web], issues=[issue(7, body=key)])
        self.assertIn("recovered #7: gate", ir.report(api, 10))

    def test_production_failure_body_mentions_source_sha_caveat(self):
        job = {"name": "production-preflight", "conclusion": "failure", "steps": []}
        api = FakeAPI(run=make_run(workflow="deploy-production.yml", event="workflow_dispatch"),
                      jobs=[job])
        self.assertEqual(ir.report(api, 10), ["opened: production-preflight"])
        self.assertIn("No production change is authorized", api.writes[0][2]["body"])


class WorkflowWiringTests(unittest.TestCase):
    def setUp(self):
        root = os.path.dirname(CI_DIR)
        with open(os.path.join(root, ".github/workflows/gate-incidents.yml")) as f:
            self.wf = f.read()
        with open(os.path.join(root, ".github/workflows/ci.yml")) as f:
            self.ci = f.read()

    def test_trust_and_permissions(self):
        self.assertIn("ref: ${{ github.sha }}", self.wf)
        self.assertIn("persist-credentials: false", self.wf)
        self.assertNotIn("head_sha", self.wf)
        top, jobs = self.wf.split("\njobs:\n")
        self.assertNotIn("issues:", top)
        self.assertEqual(jobs.count("issues: write"), 1)
        self.assertNotIn("upload-artifact", self.wf)
        self.assertNotIn("download-artifact", self.wf)
        self.assertNotIn("/logs", self.wf)

    def test_serial_queue_and_ci_command(self):
        self.assertIn("group: gate-incident-writer", self.wf)
        self.assertIn("cancel-in-progress: false", self.wf)
        self.assertIn("queue: max", self.wf)
        self.assertIn("python3 -I ci/test_incident_report.py", self.ci)


if __name__ == "__main__":
    unittest.main()
