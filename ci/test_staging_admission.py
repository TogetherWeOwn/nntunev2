#!/usr/bin/env python3
"""Offline fixtures for ci/staging_admission.py (pure evaluator and fake Actions)."""

import copy
import importlib.util
import io
import json
import os
import subprocess
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
import unittest
from unittest.mock import patch
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("staging_admission_under_test", HERE / "staging_admission.py")
sa = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sa)

SHA = "a" * 40
OTHER_SHA = "b" * 40
REPO = "org/repo"
REPO_ID = 7
WORKFLOW_ID = 5
RUN_ID = 100
ARTIFACT_ID = 900
MANIFEST = b"[migrations]\n"
NOW = datetime.now(timezone.utc)


def iso(minutes):
    return (NOW + timedelta(minutes=minutes)).isoformat()


CONTRACT = {
    "schema": 1, "evaluator": "junit-map-v1", "spec_revision": "spec-1",
    "criteria": [
        {"id": "signup", "applicable": True,
         "evidence": [{"classname": "journey", "name": "signup", "outcome": "passed"}]},
        {"id": "billing", "applicable": False, "rationale": "not offered in this release"},
    ],
}
CONTRACT_BYTES = json.dumps(CONTRACT).encode()


def make_policy(contract_bytes=CONTRACT_BYTES):
    return {"schema": 1, "evaluator": "junit-map-v1", "approvals": [{
        "sha256": sa.digest(contract_bytes), "spec_revision": "spec-1",
        "product_approval": "synthetic-product", "qa_approval": "synthetic-qa"}]}


def make_junit(skip=False, fail=False, omit=False, final_sha=SHA):
    suite = ET.Element("testsuite")
    if not omit:
        case = ET.SubElement(suite, "testcase", classname="journey", name="signup")
        if skip:
            ET.SubElement(case, "skipped")
        if fail:
            ET.SubElement(case, "failure")
    for phase, minutes, web in (("build-identity", -3, SHA), ("build-identity-final", -2, final_sha)):
        case = ET.SubElement(suite, "testcase", classname="identity", name=phase)
        props = ET.SubElement(case, "properties")
        value = {"phase": phase, "web": web, "api": SHA, "expected": SHA, "observed_at": iso(minutes)}
        ET.SubElement(props, "property", name="candidate-identity", value=json.dumps(value))
    return ET.tostring(suite)


def make_env(**over):
    env = {"GITHUB_SHA": SHA, "GITHUB_REPOSITORY": REPO, "GITHUB_WORKFLOW_SHA": SHA,
           "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_RUN_ID": str(RUN_ID),
           "GITHUB_RUN_ATTEMPT": "1", "API_DEPLOYED": "true", "API_DEPLOY_OUTCOME": "deployed",
           "API_SOURCE_SHA": SHA, "JOURNEY_GATE": "success"}
    env.update(over)
    return env


def make_zip(members):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in members:
            z.writestr(name, data)
    return buf.getvalue()


class FakeActions(sa.Actions):
    """Real listing/pagination checks over an in-memory route table; no network."""

    def __init__(self, routes, hook=None):
        super().__init__(REPO, "synthetic-token")
        self.routes = routes
        self.hook = hook
        self.calls = []

    def read(self, route, limit=sa.MAX_JSON):
        self.calls.append(route)
        if self.hook:
            self.hook(route, self.routes)
        require = sa.require
        require(route in self.routes, "authenticated Actions evidence unavailable")
        data = self.routes[route]
        if not isinstance(data, bytes):
            data = json.dumps(data).encode()
        require(len(data) <= limit, "Actions response exceeds bounds")
        return data


def page(key, rows, total=None):
    return {"total_count": len(rows) if total is None else total, key: rows}


class World:
    """Coherent synthetic run: one main-push attempt, three green jobs, one receipt."""

    def __init__(self, env=None, xml=None, policy=None, contract_bytes=CONTRACT_BYTES, manifest=MANIFEST):
        self.policy = policy or make_policy(contract_bytes)
        self.contract_bytes = contract_bytes
        self.manifest = manifest
        self.receipt = sa.produce(make_env(**(env or {})), xml or make_junit(), self.policy,
                                  contract_bytes, manifest)
        self.workflow = {"id": WORKFLOW_ID, "path": sa.WORKFLOW, "state": "active"}
        repo = {"full_name": REPO, "id": REPO_ID}
        self.run = {"id": RUN_ID, "run_attempt": 1, "workflow_id": WORKFLOW_ID, "path": sa.WORKFLOW,
                    "head_sha": SHA, "head_branch": "main", "event": "push", "status": "completed",
                    "conclusion": "success", "repository": repo, "head_repository": dict(repo),
                    "created_at": iso(-20), "updated_at": iso(5)}
        self.runs = [self.run]
        self.others = {}
        self.jobs = [self.job("staging-web", -10), self.job("staging-api", -10),
                     self.job(sa.JOURNEY, 5, started=-9, steps=[
                         self.step("Run full journey", 1), self.step(
                             "Enforce journey evidence gate (all-skipped must fail)", 2),
                         self.step(sa.RECORD, 3), self.step(sa.UPLOAD, 4)])]
        self.artifact = None
        self.archive = None
        self.set_archive()

    @staticmethod
    def step(name, number, conclusion="success"):
        return {"name": name, "number": number, "status": "completed", "conclusion": conclusion}

    @staticmethod
    def job(name, done, started=-15, steps=None):
        return {"name": name, "run_id": RUN_ID, "run_attempt": 1, "head_sha": SHA, "status": "completed",
                "conclusion": "success", "started_at": iso(started), "completed_at": iso(done),
                "steps": steps or []}

    def set_archive(self, archive=None):
        self.archive = archive if archive is not None else make_zip(
            [("staging-admission.json", json.dumps(self.receipt))])
        self.artifact = {
            "id": ARTIFACT_ID, "name": sa.artifact_name(SHA, RUN_ID, 1), "expired": False,
            "size_in_bytes": len(self.archive), "digest": "sha256:" + sa.digest(self.archive),
            "expires_at": iso(60 * 24),
            "workflow_run": {"id": RUN_ID, "repository_id": REPO_ID, "head_repository_id": REPO_ID,
                             "head_branch": "main", "head_sha": SHA}}

    def add_other(self, conclusion, status="completed", job="staging-api"):
        other = {"id": 101, "run_attempt": 1, "event": "push", "head_sha": OTHER_SHA,
                 "created_at": iso(-1), "updated_at": iso(-1), "status": status,
                 "conclusion": "success" if status == "completed" else None}
        self.runs.append(other)
        self.others[101] = [{"name": job, "status": status, "conclusion": conclusion,
                             "completed_at": iso(-1), "run_attempt": 1},
                            {"name": "staging-web" if job == "staging-api" else "staging-api",
                             "status": "completed", "conclusion": "skipped", "run_attempt": 1}]

    def routes(self, artifacts=None, jobs=None):
        base = "?per_page=100&page=1"
        r = {
            "workflows/deploy.yml": self.workflow,
            f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push&per_page=100&page=1":
                page("workflow_runs", self.runs),
            f"runs/{RUN_ID}": self.run,
            f"runs/{RUN_ID}/attempts/1/jobs{base}": jobs or page("jobs", self.jobs),
            f"runs/{RUN_ID}/artifacts{base}": artifacts or page("artifacts", [self.artifact]),
            f"artifacts/{ARTIFACT_ID}/zip": self.archive,
        }
        # Discovery fixtures expose only the bounded queries. The unfiltered
        # lifetime endpoint remains above to demonstrate that it is not needed.
        candidate = [run for run in self.runs if run.get("head_sha") == SHA]
        queries = {f"head_sha={SHA}": candidate, f"head_sha={OTHER_SHA}": []}
        for status in ("queued", "in_progress", "waiting", "pending", "requested"):
            queries[f"status={status}"] = [run for run in self.runs if run.get("status") == status]
        cutoff = (datetime.fromisoformat(self.receipt["identities"]["build-identity"]["observed_at"])
                  - timedelta(days=66)).date().isoformat()
        queries[f"created=%3E%3D{cutoff}"] = [
            run for run in self.runs if datetime.fromisoformat(run["created_at"]).date().isoformat() >= cutoff]
        for query, rows in queries.items():
            for start in range(0, max(1, len(rows)), 100):
                route = (f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push&{query}"
                         f"&per_page=100&page={start // 100 + 1}")
                r[route] = page("workflow_runs", rows[start:start + 100], total=len(rows))
        for rid, jobs_ in self.others.items():
            r[f"runs/{rid}/attempts/1/jobs{base}"] = page("jobs", jobs_)
            r[f"runs/{rid}/jobs?filter=all&per_page=100&page=1"] = page("jobs", jobs_)
        return r

    def verify(self, routes=None, hook=None, **over):
        args = dict(repo=REPO, repository_id=str(REPO_ID), sha=SHA, policy=self.policy,
                    contract_bytes=self.contract_bytes, manifest_hash=sa.digest(self.manifest),
                    producer_source_ok=lambda source: None)
        args.update(over)
        return sa.verify(FakeActions(routes or self.routes(), hook), **args)

    def verify_with_actions(self, actions):
        return sa.verify(actions, repo=REPO, repository_id=str(REPO_ID), sha=SHA,
                         policy=self.policy, contract_bytes=self.contract_bytes,
                         manifest_hash=sa.digest(self.manifest), producer_source_ok=lambda source: None)


class Base(unittest.TestCase):
    def refused(self, reason, fn, *a, **kw):
        with self.assertRaisesRegex(sa.Refused, reason):
            fn(*a, **kw)

    def assertWorldRefused(self, world, reason, **kw):
        self.refused(reason, world.verify, **kw)


class PolicyTests(Base):
    def test_default_policy_has_no_approvals_and_refuses(self):
        policy = json.loads((HERE / "staging-admission-policy.json").read_text())
        self.assertEqual(policy["approvals"], [])
        self.refused("unavailable or ambiguous", sa.approved_contract, policy, CONTRACT_BYTES)

    def test_missing_or_ambiguous_approval(self):
        policy = make_policy()
        self.refused("unavailable", sa.approved_contract, dict(policy, approvals=[]), CONTRACT_BYTES)
        self.refused("unavailable", sa.approved_contract,
                     dict(policy, approvals=policy["approvals"] * 2), CONTRACT_BYTES)

    def test_self_enrolled_contract_hash_not_approved(self):
        edited = json.dumps(dict(CONTRACT, criteria=CONTRACT["criteria"][:1] * 1)).encode() + b" "
        self.refused("unapproved product contract hash", sa.approved_contract, make_policy(), edited)

    def test_approval_reference_and_spec_revision(self):
        policy = make_policy()
        del policy["approvals"][0]["qa_approval"]
        self.refused("missing authenticated approval", sa.approved_contract, policy, CONTRACT_BYTES)
        other = json.dumps(dict(CONTRACT, spec_revision="spec-2")).encode()
        self.refused("contract policy mismatch", sa.approved_contract, make_policy(other), other)

    def test_contract_shape(self):
        for criteria, reason in (
            ([], "empty or invalid"),
            ([{"id": "x", "applicable": False, "rationale": " "}], "rationale"),
            ([{"id": "x", "applicable": False, "rationale": "n/a"}], "no applicable"),
            ([{"id": "x", "applicable": True, "evidence": []}], "no executable evidence"),
            (CONTRACT["criteria"][:1] * 2, "invalid or duplicate"),
        ):
            data = json.dumps(dict(CONTRACT, criteria=criteria)).encode()
            self.refused(reason, sa.approved_contract, make_policy(data), data)


class ProduceTests(Base):
    def test_positive_receipt_is_satisfied(self):
        receipt = World().receipt
        self.assertEqual((receipt["result"], receipt["reasons"]), ("satisfied", []))
        self.assertEqual(receipt["coverage"], [{"id": "signup", "outcome": "passed"}])

    def test_incomplete_criteria_are_never_satisfied(self):
        for kind in ({"skip": True}, {"fail": True}, {"omit": True}):
            receipt = World(xml=make_junit(**kind)).receipt
            self.assertEqual(receipt["result"], "incomplete", kind)
            self.assertTrue(receipt["reasons"], kind)

    def test_unapproved_policy_never_satisfied(self):
        w = World(policy={"schema": 1, "evaluator": "junit-map-v1", "approvals": []})
        self.assertEqual(w.receipt["result"], "incomplete")

    def test_undeployed_or_unconfigured_api(self):
        for env in ({"API_DEPLOYED": "false"}, {"API_DEPLOYED": ""}, {"API_DEPLOY_OUTCOME": "skipped"},
                    {"API_SOURCE_SHA": OTHER_SHA}, {"API_SOURCE_SHA": ""}):
            self.assertEqual(World(env=env).receipt["result"], "incomplete", env)

    def test_gate_failure_and_identity_disagreement(self):
        self.assertEqual(World(env={"JOURNEY_GATE": "failure"}).receipt["result"], "incomplete")
        self.assertEqual(World(xml=make_junit(final_sha=OTHER_SHA)).receipt["result"], "incomplete")

    def test_receipt_drops_unapproved_annotation_fields_and_raw_diagnostics(self):
        xml = make_junit().replace(b'&quot;phase&quot;', b'&quot;extra&quot;: &quot;private-value&quot;, &quot;phase&quot;')
        receipt = World(xml=xml).receipt
        self.assertEqual(receipt["result"], "satisfied")
        self.assertNotIn("private-value", json.dumps(receipt))
        self.refused("invalid API outcome", sa.produce,
                     make_env(API_DEPLOY_OUTCOME="private-value"), make_junit(), make_policy(),
                     CONTRACT_BYTES, MANIFEST)

    def test_reporter_expected_failures_and_flaky_pass_are_not_product_pass(self):
        # Locked reporter shape: expected failures have a fail property rather
        # than a failure element; includeRetries adds flakyFailure/flakyError.
        for marker in ('<properties><property name="fail" value="known gap"/></properties>',
                       '<flakyFailure message="retry failure"/>', '<flakyError/>', '<rerunFailure/>'):
            xml = make_junit().replace(b'name="signup" />',
                                      b'name="signup">' + marker.encode() + b'</testcase>')
            self.assertNotEqual(xml, make_junit(), "fixture must actually insert the marker")
            with self.subTest(marker=marker):
                self.assertEqual(World(xml=xml).receipt["result"], "incomplete", marker)

    def test_unsafe_junit(self):
        self.refused("unsafe", sa.junit_evidence, b"<!DOCTYPE x><a/>", CONTRACT)
        self.refused("malformed", sa.junit_evidence, b"<a>", CONTRACT)
        self.refused("empty", sa.junit_evidence, b"<testsuite/>", CONTRACT)


class VerifyTests(Base):
    def test_positive(self):
        w = World()
        self.assertEqual(w.verify()["result"], "satisfied")

    def test_positive_ignores_skipped_gate_only_later_run(self):
        w = World()
        w.add_other("skipped")
        self.assertEqual(w.verify()["result"], "satisfied")

    def test_hash_source_and_manifest_mismatch(self):
        w = World()
        self.assertWorldRefused(w, "manifest mismatch", manifest_hash=sa.digest(b"other"))
        self.assertWorldRefused(w, "no candidate", sha=OTHER_SHA)
        for field, value in (("sha", OTHER_SHA), ("repository", "org/other"), ("ref", "refs/heads/x"),
                             ("event", "workflow_dispatch"), ("workflow_sha", OTHER_SHA),
                             ("run_id", "101"), ("run_attempt", "2"), ("contract_sha256", "0" * 64),
                             ("policy_sha256", "0" * 64), ("junit_sha256", "xyz")):
            w = World()
            w.receipt[field] = value
            w.set_archive()
            self.assertWorldRefused(w, ".", )

    def test_tampered_result_and_coverage(self):
        for field, value in (("coverage", []), ("result", "incomplete"), ("reasons", ["x"]),
                             ("legacy_gate", "failure"), ("api", {"job": "staging-api", "deployed": False,
                                                                  "outcome": "", "source_sha": ""})):
            w = World()
            w.receipt[field] = value
            w.set_archive()
            self.assertWorldRefused(w, ".")

    def test_identity_and_time_ordering(self):
        w = World()
        w.receipt["identities"]["build-identity-final"]["web"] = OTHER_SHA
        w.set_archive()
        self.assertWorldRefused(w, "live identities disagree")
        w = World()
        ids = w.receipt["identities"]
        ids["build-identity"]["observed_at"], ids["build-identity-final"]["observed_at"] = (
            ids["build-identity-final"]["observed_at"], ids["build-identity"]["observed_at"])
        w.set_archive()
        self.assertWorldRefused(w, "incoherent evidence ordering")

    def test_journey_timing(self):
        w = World()
        w.jobs[2]["completed_at"] = iso(-4)
        self.assertWorldRefused(w, "journey timing mismatch")
        w = World()
        w.jobs[0]["completed_at"] = iso(-2.5)
        self.assertWorldRefused(w, "preceded candidate deployment")

    def test_foreign_repository_and_manual_workflow(self):
        w = World()
        w.run["head_repository"] = {"full_name": "fork/repo", "id": 8}
        self.assertWorldRefused(w, "foreign repository")
        w = World()
        w.run["repository"]["id"] = 8
        self.assertWorldRefused(w, "foreign repository")
        w = World()
        w.run["event"] = "workflow_dispatch"
        self.assertWorldRefused(w, "no candidate")
        w = World()
        w.workflow["path"] = ".github/workflows/other.yml"
        self.assertWorldRefused(w, "wrong staging workflow")
        w = World()
        w.run["head_branch"] = "topic"
        self.assertWorldRefused(w, "superseded or unsuccessful")
        w = World()
        w.run["conclusion"] = "failure"
        self.assertWorldRefused(w, "superseded or unsuccessful")

    def test_producer_source_drift_refused(self):
        def drift(source):
            raise sa.Refused("relevant producer or trusted policy drift")
        self.assertWorldRefused(World(), "drift", producer_source_ok=drift)

    def test_superseded_and_mixed_attempts(self):
        w = World()
        w.runs = [dict(w.run)]
        w.run["run_attempt"] = 2
        self.assertWorldRefused(w, "superseded or unsuccessful")
        w = World()
        w.runs = [dict(w.run, run_attempt=1)]

        def rerun(route, routes):
            # A rerun appears after the artifact download.
            if route == f"artifacts/{ARTIFACT_ID}/zip":
                routes[f"runs/{RUN_ID}"] = dict(w.run, run_attempt=2)
        self.assertWorldRefused(w, "superseded or unsuccessful", hook=rerun)
        w = World()
        w.jobs[1]["run_attempt"] = 2
        self.assertWorldRefused(w, "required job did not pass")

    def test_later_conflicting_deploy_invalidates(self):
        for status, conclusion in (("completed", "success"), ("in_progress", None)):
            w = World()
            w.add_other(conclusion, status=status, job="staging-web")
            self.assertWorldRefused(w, "intervening staging deployment")

    def test_older_queued_run_can_deploy_late(self):
        w = World()
        w.add_other("success")
        w.runs[1]["created_at"] = iso(-30)
        self.assertWorldRefused(w, "intervening staging deployment")
        w.others[101][0]["completed_at"] = iso(-15)
        self.assertEqual(w.verify()["result"], "satisfied")

    def test_large_lifetime_history_and_complete_relevant_pagination(self):
        w = World()
        for rid in range(200, 986):
            # More than a hundred relevant rows, plus old lifetime history.
            recent = rid < 401
            w.runs.append(dict(w.run, id=rid, head_sha=OTHER_SHA,
                               created_at=iso(-60 if recent else -100 * 24 * 60),
                               updated_at=iso(-30)))
        actions = FakeActions(w.routes())
        self.assertEqual(w.verify_with_actions(actions)["result"], "satisfied")
        self.assertTrue(any("page=3" in route for route in actions.calls))
        self.assertNotIn(f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push&per_page=100&page=1",
                         actions.calls)

    def test_incomplete_relevant_discovery_refuses(self):
        w = World()
        routes = w.routes()
        route = next(r for r in routes if "&created=" in r)
        routes[route]["total_count"] = 201
        self.assertWorldRefused(w, "incomplete or exhausted workflow_runs", routes=routes)

    def test_older_unresolved_run_is_discovered_independently(self):
        w = World()
        w.add_other(None, status="waiting")
        w.runs[1]["created_at"] = iso(-100 * 24 * 60)
        self.assertWorldRefused(w, "intervening staging deployment")

    def test_predeployment_failure_does_not_invalidate(self):
        for lane, deploy in (("staging-web", "Deploy to staging"),
                             ("staging-api", "Deploy api to staging")):
            with self.subTest(lane=lane):
                w = World()
                w.add_other("failure", job=lane)
                w.others[101][0]["steps"] = [w.step("Prerequisite", 1, "failure"),
                                            w.step(deploy, 2, "skipped")]
                self.assertEqual(w.verify()["result"], "satisfied")

    def test_executed_or_uncertain_deployment_invalidates(self):
        for steps in ([], [World.step("Deploy api to staging", 2, "failure")],
                      [World.step("Deploy api to staging", 2)],
                      [World.step("Deploy api to staging", 2, "cancelled")],
                      [World.step("Deploy api to staging", 2, "skipped")],
                      [World.step("Prerequisite", 3, "failure"),
                       World.step("Deploy api to staging", 2, "skipped")]):
            w = World()
            w.add_other("failure")
            w.others[101][0]["steps"] = steps
            self.assertWorldRefused(w, "intervening staging deployment")

    def test_every_unresolved_job_and_missing_conclusion_refuses(self):
        for status in ("queued", "waiting", "pending", "requested", "in_progress", "unknown", None):
            for conclusion in (None, "skipped"):
                with self.subTest(status=status, conclusion=conclusion):
                    w = World()
                    w.add_other(conclusion)
                    w.others[101][0]["status"] = status
                    self.assertWorldRefused(w, "intervening staging deployment")
        w = World()
        w.add_other(None)
        w.others[101][0]["completed_at"] = iso(-30)
        self.assertWorldRefused(w, "intervening staging deployment")

    def test_prior_rerun_mutation_is_not_hidden_by_latest_predeploy_failure(self):
        w = World()
        w.add_other("failure")
        w.others[101][0]["steps"] = [w.step("Prerequisite", 1, "failure"),
                                    w.step("Deploy api to staging", 2, "skipped")]
        w.others[101].append(dict(w.others[101][0], conclusion="success", steps=[]))
        self.assertWorldRefused(w, "intervening staging deployment")

    def test_pending_rerun_cannot_reuse_historical_lanes(self):
        for status in ("queued", "waiting", "in_progress"):
            with self.subTest(status=status):
                w = World()
                w.add_other("success", status=status)
                w.runs[1]["run_attempt"] = 2
                w.others[101][0].update(status="completed", completed_at=iso(-30))
                w.others[101].append({"name": "gate", "status": status,
                                      "run_attempt": 2, "conclusion": None})
                self.assertWorldRefused(w, "missing current-attempt conflicting staging jobs")

    def test_current_attempt_terminal_lanes_can_finish_before_other_jobs(self):
        w = World()
        w.add_other("success", status="in_progress")
        w.others[101][0].update(status="completed", completed_at=iso(-30))
        self.assertEqual(w.verify()["result"], "satisfied")

    def test_predeploy_exemption_requires_reviewed_producer(self):
        w = World()
        w.add_other("failure")
        w.others[101][0]["steps"] = [w.step("Prerequisite", 1, "failure"),
                                    w.step("Deploy api to staging", 2, "skipped")]
        def source_ok(source):
            if source != SHA:
                raise sa.Refused("conflicting producer drift")
        self.assertWorldRefused(w, "conflicting producer drift", producer_source_ok=source_ok)

    def test_old_created_rerun_with_recent_mutation_is_discovered(self):
        w = World()
        w.add_other("success")
        w.runs[1].update(created_at=iso(-60 * 24 * 60), run_attempt=2)
        w.others[101][0]["run_attempt"] = 2
        self.assertWorldRefused(w, "intervening staging deployment")

    def test_run_listing_bounds_drift_duplicates_and_response_size(self):
        route = "workflows/5/runs?branch=main&event=push"
        rows = [dict(World().run, id=rid) for rid in range(1, 102)]
        first = route + "&per_page=100&page=1"
        second = route + "&per_page=100&page=2"
        for routes in ({first: page("workflow_runs", rows[:100], total=1000)},
                       {first: page("workflow_runs", rows[:100], total=101)},
                       {first: page("workflow_runs", rows[:100], total=101),
                        second: page("workflow_runs", rows[100:], total=102)},
                       {first: page("workflow_runs", rows[:100], total=101),
                        second: page("workflow_runs", rows[:1], total=101)}):
            self.refused("incomplete or exhausted|unavailable", FakeActions(routes).runs, route)
        self.refused("response exceeds bounds", FakeActions({first: b" " * (sa.MAX_RUN_PAGE + 1)}).runs, route)

    def test_missing_conflicting_lanes_refuses(self):
        w = World()
        w.add_other("success")
        w.others[101] = [{"name": "gate", "status": "completed", "conclusion": "success"}]
        self.assertWorldRefused(w, "missing conflicting staging jobs")

    def test_jobs_missing_duplicate_or_failed(self):
        w = World()
        w.jobs.pop(0)
        self.assertWorldRefused(w, "missing or ambiguous required")
        w = World()
        w.jobs.append(dict(w.jobs[1]))
        self.assertWorldRefused(w, "missing or ambiguous required")
        w = World()
        w.jobs[1]["conclusion"] = "skipped"
        self.assertWorldRefused(w, "required job did not pass")
        w = World()
        w.jobs[0]["head_sha"] = OTHER_SHA
        self.assertWorldRefused(w, "required job did not pass")

    def test_step_ordering_and_outcomes(self):
        w = World()
        steps = w.jobs[2]["steps"]
        steps[0]["number"], steps[1]["number"] = 2, 1
        self.assertWorldRefused(w, "steps must succeed in order")
        w = World()
        w.jobs[2]["steps"][1]["conclusion"] = "skipped"
        self.assertWorldRefused(w, "steps must succeed in order")
        w = World()
        w.jobs[2]["steps"].pop()
        self.assertWorldRefused(w, "steps must succeed in order")
        w = World()
        w.jobs[2]["steps"].append(dict(w.jobs[2]["steps"][0], number=9))
        self.assertWorldRefused(w, "steps must succeed in order")

    def test_artifact_missing_duplicate_or_wrong_name(self):
        w = World()
        w.artifact["name"] = "other"
        self.assertWorldRefused(w, "missing or ambiguous")
        w = World()
        self.assertWorldRefused(w, "missing or ambiguous",
                                routes=w.routes(artifacts=page("artifacts", [w.artifact, dict(w.artifact, id=901)])))

    def test_artifact_provenance_and_expiry(self):
        for edit in (lambda a: a.update(expired=True),
                     lambda a: a.update(expires_at=iso(-1)),
                     lambda a: a.update(size_in_bytes=0),
                     lambda a: a.update(digest="md5:abc"),
                     lambda a: a["workflow_run"].update(head_sha=OTHER_SHA),
                     lambda a: a["workflow_run"].update(head_repository_id=8),
                     lambda a: a["workflow_run"].update(id=101),
                     lambda a: a["workflow_run"].update(head_branch="topic")):
            w = World()
            edit(w.artifact)
            self.assertWorldRefused(w, "invalid artifact provenance|expired")

    def test_artifact_digest_mismatch(self):
        w = World()
        w.artifact["digest"] = "sha256:" + "0" * 64
        self.assertWorldRefused(w, "content digest mismatch")

    def test_incomplete_listings(self):
        w = World()
        self.assertWorldRefused(w, "incomplete or exhausted jobs",
                                routes=w.routes(jobs=page("jobs", w.jobs, total=101)))
        self.assertWorldRefused(w, "incomplete or exhausted jobs",
                                routes=w.routes(jobs=page("jobs", w.jobs, total=len(w.jobs) + 1)))
        self.assertWorldRefused(w, "incomplete or exhausted artifacts",
                                routes=w.routes(artifacts=page("artifacts", [w.artifact], total=2)))
        w.runs = w.runs * 1
        routes = w.routes()
        routes[f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push&head_sha={SHA}&per_page=100&page=1"] = page(
            "workflow_runs", w.runs, total=250)
        self.assertWorldRefused(w, "incomplete or exhausted workflow_runs", routes=routes)

    def test_unavailable_evidence(self):
        w = World()
        routes = w.routes()
        del routes[f"runs/{RUN_ID}"]
        self.assertWorldRefused(w, "unavailable", routes=routes)

    def test_approval_absence_refused_even_when_everything_is_green(self):
        w = World()
        self.assertWorldRefused(w, "unavailable or ambiguous",
                                policy={"schema": 1, "evaluator": "junit-map-v1", "approvals": []})


class SourceBindingTests(Base):
    """Exercise main/verify with real Git objects; only Actions/live HTTP are fake."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.objects = {}
        self.git("init", "--bare", "--quiet")
        self.contract = json.dumps(dict(CONTRACT, criteria=[{
            "id": "signup", "applicable": True, "evidence": [{
                "classname": "signup.spec.ts", "name": "signup", "outcome": "passed"}]}])).encode()
        self.policy = make_policy(self.contract)
        for name, data in ((sa.POLICY, json.dumps(self.policy).encode()), (sa.CONTRACT, self.contract)):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.files = {name: ("100644", b"reviewed source") for name in sa.TRUSTED_FILES}
        self.files.update({f"{name}/fixture": ("100644", b"reviewed tree") for name in sa.TRUSTED_TREES})
        self.files.update({
            "e2e/helpers/build-identity-live.ts": ("100644", b"listed identity helper"),
            "e2e/tests/signup.spec.ts": ("100644", b"mapped journey implementation"),
            "e2e/helpers/accounts.ts": ("100644", b"identity dependency"),
            "e2e/helpers/access.ts": ("100644", b"transitive dependency"),
            "e2e/package.json": ("100644", b"reporter runtime"),
            "e2e/package-lock.json": ("100644", b"locked reporter runtime"),
            "e2e/tsconfig.json": ("100644", b"compiler configuration"),
            "e2e/fixtures/bot-contract/grants.json": ("100644", b"evidence fixture"),
            "bin/smoke.mjs": ("100644", b"external executable"),
            "fixtures/cc0/fetch_fixture.py": ("100644", b"external helper"),
            "migrations/manifest.toml": ("100644", MANIFEST),
            sa.POLICY: ("100644", json.dumps(self.policy).encode()),
            sa.CONTRACT: ("100644", self.contract),
        })

    def git(self, *args, data=None):
        env = dict(os.environ, GIT_AUTHOR_NAME="Fixture", GIT_COMMITTER_NAME="Fixture",
                   GIT_AUTHOR_EMAIL="fixture@example.invalid", GIT_COMMITTER_EMAIL="fixture@example.invalid")
        key = (args, data)
        immutable = args[0] in ("hash-object", "mktree")
        if immutable and key in self.objects:
            return self.objects[key]
        result = subprocess.check_output(["git", "-C", str(self.root), *args], input=data,
                                         stderr=subprocess.PIPE, env=env, timeout=15)
        if immutable:
            self.objects[key] = result
        return result

    def commit(self, files, parent=None):
        nested = {}
        for path, value in files.items():
            node = nested
            parts = path.split("/")
            for part in parts[:-1]:
                node = node.setdefault(part, {})
            node[parts[-1]] = value
        def tree(node):
            rows = []
            for name, value in node.items():
                if isinstance(value, dict):
                    mode, kind, oid = "040000", "tree", tree(value)
                else:
                    mode, content = value
                    kind, oid = "blob", self.git("hash-object", "-w", "--stdin", data=content).strip()
                rows.append(f"{mode} {kind} ".encode() + oid + b"\t" + name.encode() + b"\0")
            return self.git("mktree", "-z", data=b"".join(rows)).strip()
        args = ["commit-tree", tree(nested).decode(), "-m", "synthetic source"]
        if parent:
            args.extend(["-p", parent])
        return self.git(*args).decode().strip()

    def run_main(self, candidate_files=None, trusted_files=None, current_files=None):
        candidate = self.commit(candidate_files or self.files)
        trusted = self.commit(trusted_files or self.files, candidate)
        current = self.commit(current_files or trusted_files or self.files, trusted)
        self.git("update-ref", "HEAD", trusted)
        self.git("update-ref", "refs/remotes/origin/main", current)
        with patch.dict(globals(), SHA=candidate):
            world = World(policy=self.policy, contract_bytes=self.contract,
                          xml=make_junit(final_sha=candidate).replace(
                              b'classname="journey"', b'classname="signup.spec.ts"'))
            actions = FakeActions(world.routes())
        env = make_env(GITHUB_SHA=trusted, DEPLOY_SHA=candidate, GITHUB_EVENT_NAME="workflow_dispatch",
                       GITHUB_REPOSITORY_ID=str(REPO_ID), GITHUB_TOKEN="synthetic-token")
        with patch.object(sa, "ROOT", self.root), patch.dict(sa.os.environ, env, clear=True), \
                patch.object(sa.sys, "argv", ["staging_admission.py", "verify"]), \
                patch.object(sa, "Actions", return_value=actions), \
                patch.object(sa, "current_identity") as live, patch("sys.stdout", new=io.StringIO()):
            sa.main()
            live.assert_called_once_with(sa.os.environ, candidate)
        self.assertIn(f"artifacts/{ARTIFACT_ID}/zip", actions.calls)

    def test_main_accepts_matching_closure_despite_unrelated_main_movement(self):
        current = dict(self.files, **{"docs/unrelated.md": ("100644", b"unrelated main change")})
        self.run_main(current_files=current)

    def test_main_refuses_older_executable_evidence_inputs(self):
        for name in ("e2e/tests/signup.spec.ts", "e2e/helpers/accounts.ts", "e2e/helpers/access.ts",
                     "e2e/package.json", "e2e/package-lock.json", "e2e/tsconfig.json",
                     "e2e/fixtures/bot-contract/grants.json", "bin/smoke.mjs",
                     "fixtures/cc0/fetch_fixture.py"):
            with self.subTest(path=name):
                corrected = dict(self.files, **{name: ("100644", b"corrected evidence input")})
                self.refused("producer.*drift", self.run_main, trusted_files=corrected)

    def test_main_refuses_current_main_only_dependency_drift(self):
        corrected = dict(self.files, **{"e2e/helpers/accounts.ts": ("100644", b"current main correction")})
        self.refused("producer.*drift", self.run_main, current_files=corrected)

    def test_main_refuses_trusted_dispatch_only_dependency_drift(self):
        corrected = dict(self.files, **{"e2e/package-lock.json": ("100644", b"dispatch correction")})
        self.refused("producer.*drift", self.run_main, trusted_files=corrected, current_files=self.files)

    def test_main_refuses_dependency_addition_deletion_and_mode_change(self):
        added = dict(self.files, **{"e2e/helpers/new-dependency.ts": ("100644", b"new dependency")})
        deleted = dict(self.files)
        del deleted["e2e/helpers/accounts.ts"]
        executable = dict(self.files, **{"e2e/helpers/accounts.ts": ("100755", b"identity dependency")})
        symlink = dict(self.files, **{"e2e/helpers/accounts.ts": ("120000", b"access.ts")})
        for changed in (added, deleted, executable, symlink):
            with self.subTest(files=changed):
                self.refused("producer.*drift", self.run_main, trusted_files=changed)

    def test_main_refuses_external_tree_input_drift(self):
        for tree in sa.TRUSTED_TREES:
            with self.subTest(tree=tree):
                changed = dict(self.files, **{f"{tree}/fixture": ("100644", b"corrected external input")})
                self.refused("producer.*drift", self.run_main, trusted_files=changed)

    def test_main_refuses_missing_and_non_tree_dependency_roots(self):
        missing = {path: value for path, value in self.files.items() if not path.startswith("e2e/")}
        symlink = dict(missing, e2e=("120000", b"elsewhere"))
        for changed in (missing, symlink):
            with self.subTest(root=changed.get("e2e")):
                self.refused("missing or unsupported", self.run_main, trusted_files=changed)

    def test_main_existing_identity_helper_drift_negative_control(self):
        changed = dict(self.files, **{"e2e/helpers/build-identity-live.ts": ("100644", b"correction")})
        self.refused("producer.*drift", self.run_main, trusted_files=changed)


class TransportTests(Base):
    def test_archive_redirect_drops_repository_token(self):
        request = urllib.request.Request("https://api.github.com/archive", headers={"Authorization": "Bearer dummy"})
        redirected = sa.ArtifactRedirect().redirect_request(
            request, None, 302, "", {}, "https://storage.example.invalid/signed")
        self.assertIsNone(redirected.get_header("Authorization"))
        self.refused("insecure", sa.ArtifactRedirect().redirect_request,
                     request, None, 302, "", {}, "http://storage.example.invalid/signed")

    def test_identity_never_redirects_access_pair(self):
        self.refused("redirect refused", sa.NoRedirect().redirect_request,
                     None, None, 302, "", {}, "https://login.example.invalid")

    def test_admission_point_reads_both_current_identities(self):
        env = {"STAGING_BASE_URL": "https://staging.example.invalid", "CF_ACCESS_CLIENT_ID": "dummy-id",
               "CF_ACCESS_CLIENT_SECRET": "dummy-secret"}
        class Response:
            status = 200
            def __init__(self, data):
                self.data = data
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return False
            def read(self, limit):
                return json.dumps(self.data).encode()
        with patch.object(sa.urllib.request, "build_opener") as opener:
            opener.return_value.open.side_effect = [Response({"sha": SHA}), Response({"build_sha": SHA})]
            sa.current_identity(env, SHA)
            self.assertEqual(opener.return_value.open.call_count, 2)
            opener.return_value.open.side_effect = [Response({"sha": SHA}), Response({"build_sha": OTHER_SHA})]
            self.refused("current staging identity drift", sa.current_identity, env, SHA)
        for origin in ("http://staging.example.invalid", "https://user:pass@staging.example.invalid",
                       "https://staging.example.invalid/?token=x", ""):
            self.refused("invalid staging identity origin", sa.current_identity,
                         dict(env, STAGING_BASE_URL=origin), SHA)
        self.refused("missing staging identity Access pair", sa.current_identity,
                     {"STAGING_BASE_URL": env["STAGING_BASE_URL"]}, SHA)


class WiringTests(Base):
    def test_production_requires_receipt_in_addition_to_green_jobs(self):
        workflow = (HERE.parent / ".github/workflows/deploy-production.yml").read_text()
        evidence = workflow.split("  staging-evidence:\n", 1)[1].split("\n  deploy-production:", 1)[0]
        self.assertIn("persist-credentials: false", evidence)
        self.assertIn("fetch-depth: 0", evidence)
        self.assertIn("run: python3 -I ci/staging_admission.py verify", evidence)
        self.assertNotIn("continue-on-error", evidence)
        self.assertIn("needs: [preflight, staging-evidence]", workflow)
        self.assertIn("PRODUCTION_DEPLOY_ENABLED", workflow)
        self.assertIn("CF_ACCESS_CLIENT_SECRET: ${{ secrets.CF_ACCESS_CLIENT_SECRET }}", evidence)

    def test_producer_uses_actual_api_outputs_and_attempt_bound_artifact(self):
        workflows = HERE.parent / ".github/workflows"
        deploy = (workflows / "deploy.yml").read_text()
        e2e = (workflows / "e2e-staging.yml").read_text().split("  e2e:\n", 1)[1]
        for key, output in (("api_deployed", "deployed"), ("api_source_sha", "source_sha"),
                            ("api_deploy_outcome", "outcome")):
            self.assertIn(f"{key}: ${{{{ needs.staging-api.outputs.{output} }}}}", deploy)
        self.assertIn("run: python3 -I ci/staging_admission.py produce", e2e)
        self.assertIn("name: staging-admission-${{ github.sha }}-${{ github.run_id }}-${{ github.run_attempt }}", e2e)
        self.assertIn("path: staging-admission.json", e2e)
        self.assertIn("if-no-files-found: error", e2e)
        self.assertIn("JOURNEY_GATE: ${{ steps.journey-gate.outcome }}", e2e)
        self.assertIn("PLAYWRIGHT_JUNIT_INCLUDE_RETRIES: 'true'", e2e)
        self.assertIn("includeRetries: true", (HERE.parent / "e2e/playwright.config.ts").read_text())
        ci = (workflows / "ci.yml").read_text()
        self.assertIn("run: python3 ci/test_staging_admission.py", ci)


class ArchiveTests(Base):
    def test_valid(self):
        self.assertEqual(sa.read_archive(make_zip([("staging-admission.json", "{}")])), {})

    def test_unsafe_archives(self):
        good = ("staging-admission.json", "{}")
        for archive, reason in (
            (b"", "oversized or absent"),
            (b"not a zip", "unreadable"),
            (make_zip([]), "invalid receipt archive member"),
            (make_zip([good, ("extra.txt", "x")]), "invalid receipt archive member"),
            (make_zip([("../staging-admission.json", "{}")]), "invalid receipt archive member"),
            (make_zip([("nested/staging-admission.json", "{}")]), "invalid receipt archive member"),
            (make_zip([("staging-admission.json/", "")]), "invalid receipt archive member"),
            (make_zip([good]) + b"\0" * sa.MAX_ARCHIVE, "oversized or absent"),
            (make_zip([("staging-admission.json", "x" * (sa.MAX_JSON + 1))]), "invalid receipt archive member"),
            (make_zip([("staging-admission.json", '{"a":1,"a":2}')]), "duplicate JSON field"),
            (make_zip([("staging-admission.json", "not json")]), "malformed JSON"),
        ):
            self.refused(reason, sa.read_archive, archive)

    def test_unsafe_archive_through_verify(self):
        w = World()
        w.set_archive(make_zip([("staging-admission.json", "{}"), ("x", "y")]))
        self.assertWorldRefused(w, "invalid receipt archive member")


if __name__ == "__main__":
    unittest.main()
