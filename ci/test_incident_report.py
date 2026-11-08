"""Unit tests for incident_report (stdlib unittest, fake API, no network).

Run with `python3 ci/test_incident_report.py`.
"""

import importlib.util
import os
import re
import unittest
from datetime import datetime, timedelta, timezone

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


def issue(number, updated="2026-01-01T00:00:00Z", body=MARKER, source=(9, 1), **extra):
    if source is not None:
        body += f"\n<!-- incident-source: {source[0]}:{source[1]} -->"
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
            return {"number": 1}
        page = int(m.group(1)) if (m := re.search(r"[?&]page=(\d+)", path)) else 1
        window = slice((page - 1) * 100, page * 100)
        if re.fullmatch(r"/actions/runs/\d+", path):
            self.run_gets += 1
            return self.run if self.run_gets == 1 else self.current
        if "/jobs" in path:
            return {"jobs": self.jobs[window]}
        if re.search(r"/issues/\d+/comments", path):
            return []
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
        self.assertEqual([w[1] for w in api.writes], ["/issues/7", "/issues/7/comments"])
        self.assertIn("<!-- incident-source: 10:1 -->", api.writes[0][2]["body"])

    def test_pull_request_issue_is_not_a_match(self):
        api = FakeAPI(jobs=[smoke("failure")], issues=[issue(7, pull_request={})])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])

    def test_recovery_comments_and_closes(self):
        api = FakeAPI(jobs=[smoke("success")], issues=[issue(7)])
        self.assertEqual(ir.report(api, 10), [f"recovered #7: {SMOKE}"])
        self.assertEqual([w[:2] for w in api.writes],
                         [("POST", "/issues/7/comments"), ("PATCH", "/issues/7")])
        self.assertEqual(api.writes[1][2]["state"], "closed")

    def test_recovery_uses_failure_source_not_delayed_issue_activity(self):
        for updated in ("2026-01-03T00:00:00Z", "garbage"):
            api = FakeAPI(jobs=[smoke("success")], issues=[issue(7, updated=updated)])
            self.assertEqual(ir.report(api, 10), [f"recovered #7: {SMOKE}"])

    def test_unknown_failure_source_holds_recovery(self):
        api = FakeAPI(jobs=[smoke("success")], issues=[issue(7, source=None)])
        self.assertEqual(ir.report(api, 10), ["held #7: unknown source"])
        self.assertEqual(api.writes, [])

    def test_recorded_newer_failure_or_recovery_refuses_stale_replay(self):
        for state in ("open", "closed"):
            for conclusion in ("success", "failure"):
                api = FakeAPI(jobs=[smoke(conclusion)], issues=[issue(7, source=(11, 1), state=state)])
                self.assertEqual(ir.report(api, 10), ["superseded"])
                self.assertEqual(api.writes, [])

    def test_recurrence_reopens_closed_key_instead_of_creating_duplicate(self):
        api = FakeAPI(jobs=[smoke("failure")], issues=[issue(7, state="closed")])
        self.assertEqual(ir.report(api, 10), [f"commented #7: {SMOKE}"])
        self.assertEqual(api.writes[0][2]["state"], "open")

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

    def test_newer_skipped_gate_does_not_erase_failure_or_recovery(self):
        for conclusion, expected in (("failure", f"commented #7: {SMOKE}"),
                                     ("success", f"recovered #7: {SMOKE}")):
            api = FakeAPI(jobs=[smoke(conclusion)], issues=[issue(7)],
                          runs=[make_run(), make_run(11)])
            pages = api.pages
            api.pages = lambda path, key=None: ([smoke("skipped")] if "/runs/11/" in path
                                                else pages(path, key))
            self.assertEqual(ir.report(api, 10), [expected])

    def test_all_existing_staging_deployments_are_observed_without_enabling_them(self):
        for lane in ("mb", "ingest"):
            name, step = f"staging-{lane}", f"Deploy {lane} to staging"
            job = {"name": name, "conclusion": "failure", "steps": []}
            run = make_run(workflow="deploy.yml", event="push")
            api = FakeAPI(run=run, jobs=[job])
            self.assertEqual(ir.report(api, 10), [f"opened: {name}"])
            key = f"<!-- incident-key: deploy.yml:staging-{lane} -->"
            for outcome in ("skipped", "success"):
                job = {"name": name, "conclusion": "success", "steps": [
                    {"number": 1, "name": step, "conclusion": outcome}]}
                api = FakeAPI(run=run, jobs=[job], issues=[issue(7, body=key)])
                self.assertEqual(ir.report(api, 10), [] if outcome == "skipped"
                                 else [f"recovered #7: {name}"])

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


def ago(minutes):
    stamp = datetime.now(timezone.utc) - timedelta(minutes=minutes)
    return stamp.strftime("%Y-%m-%dT%H:%M:%SZ")


class StatefulAPI(ir.API):
    """Runs, jobs, issues and comments live in memory; writes mutate them.

    `faults` is a list of (method, path regex, mode), each consumed once:
    "before" refuses without effect; "after" applies the write, then raises
    as if the response were lost.
    """

    def __init__(self, runs=(), jobs=None, issues=(), comments=None, faults=()):
        super().__init__("token")
        self.runs = list(runs)
        self.jobs = jobs or {}
        self.issues = [dict(i) for i in issues]
        self.comments = {n: [dict(c) for c in rows] for n, rows in (comments or {}).items()}
        self.faults = list(faults)
        self.writes, self.listings = [], []

    def request(self, method, path, body=None):
        fault = next((f for f in self.faults
                      if f[0] == method and re.fullmatch(f[1], path)), None)
        if fault:
            self.faults.remove(fault)
            if fault[2] == "before":
                raise ir.ReportError(f"GitHub {method} refused (HTTP 403)")
        result = self.apply(method, path, body)
        if fault:
            raise ir.ReportError("GitHub transport failed")
        return result

    def apply(self, method, path, body=None):
        if method != "GET":
            self.writes.append((method, path, body))
            if (m := re.fullmatch(r"/issues/(\d+)/comments", path)):
                self.comments.setdefault(int(m.group(1)), []).append({"body": body["body"]})
                return {"id": 1}
            if path == "/issues":
                self.issues.append({"number": 100 + len(self.issues), "state": "open",
                                    "body": body["body"], "updated_at": ago(0)})
                return {"number": self.issues[-1]["number"]}
            if (m := re.fullmatch(r"/issues/(\d+)", path)):
                target = next(i for i in self.issues if i["number"] == int(m.group(1)))
                target["body"] = body["body"]
                target["state"] = body["state"]
            return {"number": 1}
        page = int(m.group(1)) if (m := re.search(r"[?&]page=(\d+)", path)) else 1
        window = slice((page - 1) * 100, page * 100)
        if (m := re.fullmatch(r"/actions/runs/(\d+)", path)):
            return next(r for r in self.runs if r["id"] == int(m.group(1)))
        if (m := re.fullmatch(r"/actions/workflows/([\w.-]+)/runs\?.*", path)):
            self.listings.append(path)
            rows = [r for r in self.runs if r["path"].endswith("/" + m.group(1))]
            return {"workflow_runs": rows[window]}
        if (m := re.search(r"/actions/runs/(\d+)/attempts/(\d+)/jobs", path)):
            return {"jobs": self.jobs.get((int(m.group(1)), int(m.group(2))), [])[window]}
        if (m := re.search(r"/issues/(\d+)/comments", path)):
            return self.comments.get(int(m.group(1)), [])[window]
        return self.issues[window]


def recent_run(run_id, minutes=5, updated=1, **over):
    """Created `minutes` ago, last updated (completed) `updated` minutes ago."""
    return make_run(run_id, created_at=ago(minutes), updated_at=ago(updated), **over)


class RecentTests(unittest.TestCase):
    def incident_state(self, api):
        return [(i["number"], i["state"]) for i in api.issues]

    def test_ordered_fail_repeat_green_opens_comments_closes_once(self):
        runs = [recent_run(23, 1), recent_run(22, 2), recent_run(21, 3), recent_run(20, 4)]
        jobs = {(20, 1): [smoke("failure")], (21, 1): [smoke("failure")],
                (22, 1): [smoke("success")]}
        api = StatefulAPI(runs=runs, jobs=jobs)
        ir.recent(api)
        self.assertEqual([w[:2] for w in api.writes], [
            ("POST", "/issues"), ("PATCH", "/issues/100"), ("POST", "/issues/100/comments"),
            ("POST", "/issues/100/comments"), ("PATCH", "/issues/100")])
        self.assertEqual(self.incident_state(api), [(100, "closed")])
        self.assertEqual(sum(1 for w in api.writes if w[1] == "/issues"), 1)
        self.assertEqual(len(api.listings), len(ir.WORKFLOWS))
        self.assertTrue(all("branch=main" in p and "per_page=100" in p for p in api.listings))
        # Rerunning the scan over the same window must not duplicate anything.
        done = len(api.writes)
        ir.recent(api)
        self.assertEqual(len(api.writes), done)
        self.assertEqual(self.incident_state(api), [(100, "closed")])

    def test_ordered_scan_matches_unordered_default_on_history_lookup(self):
        # Default report() still supersedes an older run when newer history exists.
        runs = [recent_run(21), recent_run(20)]
        api = StatefulAPI(runs=runs, jobs={(20, 1): [smoke("failure")],
                                           (21, 1): [smoke("failure")]})
        self.assertEqual(ir.report(api, 20), ["superseded"])
        self.assertEqual(api.writes, [])

    def test_skipped_later_run_preserves_earlier_failure(self):
        runs = [recent_run(31, 1), recent_run(30, 2)]
        jobs = {(30, 1): [smoke("failure")], (31, 1): [smoke("skipped")]}
        api = StatefulAPI(runs=runs, jobs=jobs)
        ir.recent(api)
        self.assertEqual([w[:2] for w in api.writes], [("POST", "/issues")])
        self.assertEqual(self.incident_state(api), [(100, "open")])

    def test_old_and_untrusted_and_incomplete_rows_are_ignored(self):
        failing = {(n, 1): [smoke("failure")] for n in (40, 41, 42, 43, 44)}
        runs = [recent_run(40, 100, updated=90), recent_run(41, head_branch="feature"),
                recent_run(42, status="in_progress"),
                recent_run(43, head_repository={"full_name": "evil/fork"}),
                recent_run(44, event="pull_request")]
        api = StatefulAPI(runs=runs, jobs=failing)
        ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_old_updated_at_is_ignored_even_if_created_recently(self):
        run = make_run(50, created_at=ago(10), updated_at=ago(120))
        api = StatefulAPI(runs=[run], jobs={(50, 1): [smoke("failure")]})
        ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_long_running_older_created_run_completed_recently_is_retained(self):
        run = make_run(51, created_at=ago(180), updated_at=ago(2))
        api = StatefulAPI(runs=[run], jobs={(51, 1): [smoke("failure")]})
        ir.recent(api)
        self.assertEqual([w[:2] for w in api.writes], [("POST", "/issues")])

    def test_more_than_ten_recent_runs_raises_before_any_write(self):
        runs = [recent_run(60 + n, 1 + n % 5) for n in range(11)]
        api = StatefulAPI(runs=runs, jobs={(r["id"], 1): [smoke("failure")] for r in runs})
        with self.assertRaises(ir.ReportError):
            ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_ten_recent_runs_is_within_bound(self):
        runs = [recent_run(60 + n, 1 + n % 5) for n in range(10)]
        api = StatefulAPI(runs=runs, jobs={(r["id"], 1): [smoke("success")] for r in runs})
        ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_full_page_with_history_older_than_window_is_allowed(self):
        rows = [recent_run(1000 + n, 180 + n, updated=170) for n in range(99)]
        api = StatefulAPI(runs=[recent_run(2000)] + rows, jobs={(2000, 1): [smoke("failure")]})
        self.assertEqual(len(api.runs), 100)
        ir.recent(api)
        self.assertEqual([w[:2] for w in api.writes], [("POST", "/issues")])

    def test_saturated_recent_history_refuses_before_any_write(self):
        rows = [recent_run(1000 + n, 30, updated=30) for n in range(99)]
        api = StatefulAPI(runs=[recent_run(70)] + rows, jobs={(70, 1): [smoke("failure")]})
        self.assertEqual(len(api.runs), 100)
        with self.assertRaises(ir.ReportError):
            ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_more_than_one_hundred_rows_is_malformed(self):
        api = StatefulAPI(runs=[recent_run(70)], jobs={(70, 1): [smoke("failure")]})
        real = api.request
        api.request = lambda m, path, body=None: (
            {"workflow_runs": [recent_run(2000 + n, 180, updated=170) for n in range(101)]}
            if m == "GET" and path.startswith("/actions/workflows/") else real(m, path, body))
        with self.assertRaises(ir.ReportError):
            ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_supplied_newer_evidence_snapshot_skips_latest_lookup_entirely(self):
        runs = [recent_run(92), recent_run(91)]
        api = StatefulAPI(runs=runs, jobs={(91, 1): [smoke("failure")]})
        self.assertEqual(ir.report(api, 91, newer_evidence={}), [f"opened: {SMOKE}"])
        self.assertEqual(api.listings, [])

    def test_supplied_newer_evidence_is_used_to_supersede(self):
        runs = [recent_run(92), recent_run(91)]
        api = StatefulAPI(runs=runs, jobs={(91, 1): [smoke("failure")]})
        evidence = {(92, 1): [smoke("success")]}
        self.assertEqual(ir.report(api, 91, newer_evidence=evidence), ["superseded"])
        self.assertEqual(api.listings, [])
        self.assertEqual(api.writes, [])

    def test_supplied_newer_evidence_still_checks_recorded_source_and_trust(self):
        run = recent_run(80)
        api = StatefulAPI(runs=[run], jobs={(80, 1): [smoke("failure")]},
                          issues=[issue(7, source=(81, 1), state="open")])
        self.assertEqual(ir.report(api, 80, newer_evidence={}), ["superseded"])
        self.assertEqual(api.writes, [])
        bad = StatefulAPI(runs=[recent_run(82, head_branch="x")],
                          jobs={(82, 1): [smoke("failure")]})
        with self.assertRaises(ir.ReportError):
            ir.report(bad, 82, newer_evidence={})
        self.assertEqual(bad.writes, [])

    def test_newer_success_outside_window_supersedes_older_delayed_failure(self):
        outside = recent_run(71, 130, updated=120)
        for issues in ([], [issue(7, state="closed")], [issue(7, state="open")]):
            api = StatefulAPI(runs=[recent_run(70), outside], issues=issues,
                              jobs={(70, 1): [smoke("failure")], (71, 1): [smoke("success")]})
            ir.recent(api)
            self.assertEqual(api.writes, [], issues)
            self.assertEqual([i.get("state") for i in api.issues],
                             [i.get("state") for i in issues])

    def test_latest_attempt_failure_versus_newer_old_success_run(self):
        # GitHub lists one row per run, carrying its latest attempt.
        failed = recent_run(102, 150, updated=2, run_attempt=2)
        newer = recent_run(103, 140, updated=120)
        jobs = {(102, 2): [smoke("failure")], (103, 1): [smoke("success")]}
        for issues in ([], [issue(7, source=(101, 1), state="closed")]):
            api = StatefulAPI(runs=[newer, failed], jobs=jobs, issues=issues)
            ir.recent(api)
            self.assertEqual(api.writes, [], issues)
            self.assertEqual([i.get("state") for i in api.issues],
                             [i.get("state") for i in issues])

    def test_newer_rerun_success_outside_window_supersedes_older_attempt_failure(self):
        rerun = make_run(72, attempt=2, created_at=ago(150), updated_at=ago(120))
        api = StatefulAPI(runs=[recent_run(72), rerun],
                          jobs={(72, 1): [smoke("failure")], (72, 2): [smoke("success")]})
        ir.recent(api)
        self.assertEqual(api.writes, [])

    def test_skipped_or_unevidenced_newer_success_outside_window_does_not_suppress(self):
        for newer in (smoke("skipped"), smoke("success", step="skipped"),
                      {**smoke("success"), "steps": []}):
            api = StatefulAPI(runs=[recent_run(70), recent_run(71, 130, updated=120)],
                              jobs={(70, 1): [smoke("failure")], (71, 1): [newer]})
            ir.recent(api)
            self.assertEqual([w[:2] for w in api.writes], [("POST", "/issues")])

    def test_newer_evidence_outside_window_does_not_break_trail_inside_window(self):
        runs = [recent_run(23, 1), recent_run(22, 2), recent_run(21, 3),
                recent_run(10, 150, updated=140)]
        jobs = {(21, 1): [smoke("failure")], (22, 1): [smoke("failure")],
                (23, 1): [smoke("success")], (10, 1): [smoke("success")]}
        api = StatefulAPI(runs=runs, jobs=jobs)
        ir.recent(api)
        self.assertEqual([w[:2] for w in api.writes], [
            ("POST", "/issues"), ("PATCH", "/issues/100"), ("POST", "/issues/100/comments"),
            ("POST", "/issues/100/comments"), ("PATCH", "/issues/100")])
        self.assertEqual(self.incident_state(api), [(100, "closed")])

    def test_more_than_ten_newer_history_evidence_rows_raise_before_any_write(self):
        rows = [make_run(100 + n, created_at=ago(150 + n), updated_at=ago(120))
                for n in range(11)]
        jobs = {(r["id"], 1): [smoke("success")] for r in rows}
        jobs[(70, 1)] = [smoke("failure")]
        api = StatefulAPI(runs=[recent_run(70)] + rows, jobs=jobs)
        with self.assertRaises(ir.ReportError):
            ir.recent(api)
        self.assertEqual(api.writes, [])


KEY = "e2e-staging.yml:synthetic-smoke-health"
NOTIFY = "<!-- incident-notification: failure -->"


def occurrence(run_id, attempt=1, kind="failure"):
    return f"<!-- incident-occurrence: {KEY} {run_id}:{attempt} {kind} -->"


def comment_count(api, number, marker):
    return sum(1 for c in api.comments.get(number, []) if marker in c["body"])


class CommentDeliveryTests(unittest.TestCase):
    def api(self, conclusion="failure", run_id=10, issues=None, **kw):
        return StatefulAPI(runs=[make_run(run_id)], jobs={(run_id, 1): [smoke(conclusion)]},
                           issues=[issue(7)] if issues is None else issues, **kw)

    def test_repeat_patches_notification_marker_then_posts_marked_comment(self):
        api = self.api()
        self.assertEqual(ir.report(api, 10), [f"commented #7: {SMOKE}"])
        self.assertEqual([w[:2] for w in api.writes],
                         [("PATCH", "/issues/7"), ("POST", "/issues/7/comments")])
        self.assertIn("<!-- incident-source: 10:1 -->", api.writes[0][2]["body"])
        self.assertIn(NOTIFY, api.writes[0][2]["body"])
        self.assertIn(occurrence(10), api.writes[1][2]["body"])

    def test_initial_issue_creation_posts_no_repeat_comment(self):
        api = self.api(issues=[])
        self.assertEqual(ir.report(api, 10), [f"opened: {SMOKE}"])
        self.assertEqual([w[:2] for w in api.writes], [("POST", "/issues")])
        self.assertEqual(api.comments, {})

    def test_existing_marked_comment_is_not_posted_again(self):
        api = self.api(comments={7: [{"body": "Still failing.\n" + occurrence(10)}]})
        ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)
        self.assertNotIn("/issues/7/comments", [w[1] for w in api.writes])

    def test_marker_on_later_comment_page_is_found(self):
        filler = [{"body": f"note {n}"} for n in range(100)]
        api = self.api(comments={7: filler + [{"body": occurrence(10)}]})
        ir.report(api, 10)
        self.assertNotIn("/issues/7/comments", [w[1] for w in api.writes])

    def test_other_occurrence_or_kind_marker_does_not_suppress(self):
        others = [{"body": occurrence(9)}, {"body": occurrence(10, kind="recovery")},
                  {"body": occurrence(10, 2)}]
        api = self.api(comments={7: others})
        ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)

    def test_equal_source_repeat_with_notification_marker_reconciles_missing_comment(self):
        stale = issue(7, source=(10, 1), body=MARKER + "\n" + NOTIFY)
        api = self.api(issues=[stale])
        ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)
        done = len(api.writes)
        ir.report(api, 10)
        self.assertEqual(len(api.writes), done)

    def test_equal_source_without_notification_marker_stays_superseded(self):
        api = self.api(issues=[issue(7, source=(10, 1))])
        self.assertEqual(ir.report(api, 10), ["superseded"])
        self.assertEqual(api.writes, [])

    def test_patch_ok_then_comment_refused_retry_delivers_exactly_once(self):
        api = self.api(faults=[("POST", r"/issues/7/comments", "before")])
        with self.assertRaises(ir.ReportError):
            ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 0)
        self.assertIn(NOTIFY, api.issues[0]["body"])
        ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)
        done = len(api.writes)
        ir.report(api, 10)
        self.assertEqual(len(api.writes), done)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)

    def test_comment_posted_but_response_lost_retry_posts_once(self):
        api = self.api(faults=[("POST", r"/issues/7/comments", "after")])
        with self.assertRaises(ir.ReportError):
            ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)
        ir.report(api, 10)
        self.assertEqual(comment_count(api, 7, occurrence(10)), 1)

    def test_recovery_ensures_marked_comment_before_closing_patch(self):
        api = self.api("success", run_id=11)
        self.assertEqual(ir.report(api, 11), [f"recovered #7: {SMOKE}"])
        self.assertEqual([w[:2] for w in api.writes],
                         [("POST", "/issues/7/comments"), ("PATCH", "/issues/7")])
        self.assertIn(occurrence(11, kind="recovery"), api.writes[0][2]["body"])
        self.assertEqual(api.writes[1][2]["state"], "closed")

    def test_recovery_comment_posted_closing_patch_fails_retry_no_duplicate(self):
        api = self.api("success", run_id=11, faults=[("PATCH", r"/issues/7", "before")])
        with self.assertRaises(ir.ReportError):
            ir.report(api, 11)
        self.assertEqual(comment_count(api, 7, occurrence(11, kind="recovery")), 1)
        self.assertNotEqual(api.issues[0].get("state"), "closed")
        self.assertEqual(ir.report(api, 11), [f"recovered #7: {SMOKE}"])
        self.assertEqual(comment_count(api, 7, occurrence(11, kind="recovery")), 1)
        self.assertEqual(api.issues[0]["state"], "closed")

    def test_recovery_comment_response_lost_retry_posts_once_and_closes(self):
        api = self.api("success", run_id=11, faults=[("POST", r"/issues/7/comments", "after")])
        with self.assertRaises(ir.ReportError):
            ir.report(api, 11)
        ir.report(api, 11)
        self.assertEqual(comment_count(api, 7, occurrence(11, kind="recovery")), 1)
        self.assertEqual(api.issues[0]["state"], "closed")

    def test_dry_run_never_writes_comments_or_patches(self):
        for conclusion, run_id in (("failure", 10), ("success", 11)):
            api = self.api(conclusion, run_id=run_id)
            self.assertTrue(ir.report(api, run_id, dry_run=True))
            self.assertEqual(api.writes, [])
            self.assertEqual(api.comments, {})
        stale = issue(7, source=(10, 1), body=MARKER + "\n" + NOTIFY)
        api = self.api(issues=[stale])
        ir.report(api, 10, dry_run=True)
        self.assertEqual(api.writes, [])
        self.assertEqual(api.comments, {})


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

    def test_schedule_and_dispatch_triggers_with_documented_scopes(self):
        lines = [l for l in self.wf.splitlines() if not l.lstrip().startswith("#")]
        code = "\n".join(lines)
        on = code.split("\npermissions:")[0]
        self.assertIn("schedule:", on)
        self.assertRegex(on, r"cron: ['\"]?7,17,27,37,47,57 \* \* \* \*['\"]?")
        self.assertIn("workflow_dispatch:", on)
        self.assertNotRegex(on, r"workflow_dispatch:\s*\n\s+inputs:")
        for banned in ("workflow_run", "pull_request_target"):
            self.assertNotIn(banned, code)
        self.assertIn("python3 -I ci/incident_report.py --recent", code)
        self.assertNotIn("--run-id", code)
        jobs = code.split("\njobs:\n")[1]
        self.assertRegex(jobs, r"(?m)^  report:")
        for scope in ("contents: read", "actions: read", "issues: write"):
            self.assertIn(scope, jobs)

    def test_serial_queue_and_ci_command(self):
        self.assertIn("group: gate-incident-writer", self.wf)
        self.assertIn("cancel-in-progress: false", self.wf)
        self.assertIn("queue: max", self.wf)
        self.assertIn("python3 -I ci/test_incident_report.py", self.ci)


if __name__ == "__main__":
    unittest.main()
