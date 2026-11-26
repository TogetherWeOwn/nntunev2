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
PRIVATE_CASES = ("upload-to-library", "play", "seek", "queue", "anonymous-refused",
                 "unlisted-refused", "revoked-or-expired-refused", "video-off")
PRIVATE_CONTRACT = dict(CONTRACT, journey="private-music", criteria=[
    {"id": name, "applicable": True,
     "evidence": [{"classname": "private-music", "name": name, "outcome": "passed"}]}
    for name in PRIVATE_CASES
])
PRIVATE_CONTRACT_BYTES = json.dumps(PRIVATE_CONTRACT).encode()
PRIVATE_AUDIO = (HERE.parent / "fixtures/synthetic-flac/private-staging/tone-a.flac").read_bytes()
PRIVATE_AUDIO_SHA = sa.digest(PRIVATE_AUDIO)
PRIVATE_TRACK_ID = "00000000-0000-4000-8000-000000000001"


def make_policy(contract_bytes=CONTRACT_BYTES):
    return {"schema": 1, "evaluator": "junit-map-v1", "approvals": [{
        "sha256": sa.digest(contract_bytes), "spec_revision": "spec-1",
        "product_approval": "synthetic-product", "qa_approval": "synthetic-qa"}]}


def make_junit(skip=False, fail=False, omit=False, final_sha=None):
    if final_sha is None:
        final_sha = SHA
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


def private_pins():
    return {f"staging-{name}": {"worker": f"nntune-{name}-staging", "build_sha": SHA,
            "version_id": f"00000000-0000-4000-8000-{index:012d}",
            "deployment_id": f"10000000-0000-4000-8000-{index:012d}",
            "run_id": str(RUN_ID), "run_attempt": "1"}
            for index, name in enumerate(("web", "api", "stream"), 1)}


def private_report(pins, **track_overrides):
    suite = ET.Element("testsuite")
    track = {"run_id": str(RUN_ID), "run_attempt": "1", "build_sha": SHA,
             "track_id": PRIVATE_TRACK_ID, "sha256": PRIVATE_AUDIO_SHA} | track_overrides
    for name in PRIVATE_CASES:
        case = ET.SubElement(suite, "testcase", classname="private-music", name=name, retry="0")
        proofs = {"upload-to-library": ("private-publication", "private-library"),
                  "play": ("private-play",), "anonymous-refused": ("private-anonymous-refusal",),
                  "unlisted-refused": ("private-unlisted-refusal",),
                  "revoked-or-expired-refused": ("private-cutoff-refusal",)}
        if name in proofs:
            props = ET.SubElement(case, "properties")
            for proof in proofs[name]:
                ET.SubElement(props, "property", name=proof, value=json.dumps(track))
    observations = {lane: {k: v for k, v in pin.items() if k not in ("run_id", "run_attempt")}
                    for lane, pin in pins.items()}
    for phase, minutes in (("build-identity", -3), ("build-identity-final", -2)):
        case = ET.SubElement(suite, "testcase", classname="private-music", name=phase, retry="0")
        props = ET.SubElement(case, "properties")
        value = {"phase": phase, "web": SHA, "api": SHA, "expected": SHA,
                 "observed_at": iso(minutes), "deployments": observations}
        ET.SubElement(props, "property", name="candidate-identity", value=json.dumps(value))
    return suite


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

    def __init__(self, env=None, xml=None, policy=None, contract_bytes=CONTRACT_BYTES, manifest=MANIFEST,
                 private_deployments=None):
        self.policy = policy or make_policy(contract_bytes)
        self.contract_bytes = contract_bytes
        self.manifest = manifest
        self.receipt = sa.produce(make_env(**(env or {})), xml or make_junit(), self.policy,
                                  contract_bytes, manifest, private_pins=private_deployments,
                                  private_track_sha256=PRIVATE_AUDIO_SHA)
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
            "id": ARTIFACT_ID, "name": sa.artifact_name(SHA, RUN_ID, 1, self.receipt.get("journey")),
            "expired": False,
            "size_in_bytes": len(self.archive), "digest": "sha256:" + sa.digest(self.archive),
            "expires_at": iso(60 * 24),
            "workflow_run": {"id": RUN_ID, "repository_id": REPO_ID, "head_repository_id": REPO_ID,
                             "head_branch": "main", "head_sha": SHA}}

    def add_other(self, conclusion, status="completed", job="staging-api"):
        other = {"id": 101, "run_attempt": 1, "event": "push", "head_sha": OTHER_SHA,
                 "head_branch": "main", "workflow_id": WORKFLOW_ID,
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
            f"runs/{RUN_ID}/artifacts{base}": artifacts or page(
                "artifacts", [self.artifact] if self.artifact is not None else []),
        }
        if self.artifact is not None:
            r[f"artifacts/{ARTIFACT_ID}/zip"] = self.archive
        # Discovery fixtures expose only the bounded queries. The unfiltered
        # lifetime endpoint remains above to demonstrate that it is not needed.
        candidate = [run for run in self.runs if run.get("head_sha") == SHA]
        queries = {f"head_sha={SHA}": candidate, f"head_sha={OTHER_SHA}": []}
        for status in ("queued", "in_progress", "waiting", "pending", "requested"):
            queries[f"status={status}"] = [run for run in self.runs if run.get("status") == status]
        opening = sa.timestamp(self.receipt["identities"]["build-identity"]["observed_at"])
        opening_day = opening.astimezone(timezone.utc).date()
        dated = [(sa.timestamp(run["created_at"]).astimezone(timezone.utc).date(), run) for run in self.runs]
        for start, end in ((66, 45), (44, 23), (22, None)):
            first = opening_day - timedelta(days=start)
            last = opening_day - timedelta(days=end) if end is not None else None
            query = f"created={first}..{last}" if last else f"created=%3E%3D{first}"
            queries[query] = [run for day, run in dated if day >= first and (last is None or day <= last)]
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
                    producer_source_ok=lambda source: None, private_track_sha256=PRIVATE_AUDIO_SHA)
        args.update(over)
        return sa.verify(FakeActions(routes or self.routes(), hook), **args)

    def verify_with_actions(self, actions):
        return sa.verify(actions, repo=REPO, repository_id=str(REPO_ID), sha=SHA,
                         policy=self.policy, contract_bytes=self.contract_bytes,
                         manifest_hash=sa.digest(self.manifest), producer_source_ok=lambda source: None,
                         private_track_sha256=PRIVATE_AUDIO_SHA)


class Base(unittest.TestCase):
    def refused(self, reason, fn, *a, **kw):
        with self.assertRaisesRegex(sa.Refused, reason):
            fn(*a, **kw)

    def assertWorldRefused(self, world, reason, **kw):
        self.refused(reason, world.verify, **kw)


class PrivateWorld(World):
    def __init__(self, report=None):
        pins = private_pins()
        super().__init__(env={"JOURNEY_GATE": ""}, xml=ET.tostring(private_report(pins) if report is None else report),
                         contract_bytes=PRIVATE_CONTRACT_BYTES, private_deployments=pins)
        self.jobs = [self.job(lane, -10, steps=[self.step(step, 1)]) for lane, step in (
            ("staging-web", "Deploy to staging"), ("staging-api", "Deploy api to staging"),
            ("staging-stream", "Deploy stream to staging"))]
        self.jobs.append(self.job("post-deploy private music journey", 5, started=-9, steps=[
            self.step("Qualify private listening", 1), self.step(sa.RECORD, 2), self.step(sa.UPLOAD, 3)]))


class PrivateAdmissionTests(Base):
    def test_exact_private_receipt(self):
        world = PrivateWorld()
        receipt = world.verify()
        self.assertEqual(receipt["coverage"], [{"id": name, "outcome": "passed"} for name in PRIVATE_CASES])
        self.assertEqual(receipt["deployments"], private_pins())
        self.assertEqual(receipt["private_track"], {"run_id": str(RUN_ID), "run_attempt": "1",
                         "build_sha": SHA, "track_id": PRIVATE_TRACK_ID, "sha256": PRIVATE_AUDIO_SHA})
        self.assertTrue(world.artifact["name"].endswith("-private-music"))

    def test_private_receipt_rejects_a_different_staging_environment(self):
        world = PrivateWorld()
        receipt = world.verify()
        self.assertEqual(receipt["environment"], "staging")
        self.assertEqual(receipt["result"], "satisfied")

        # Keep the run, attempt and build fixed, and repack with a valid digest.
        world.receipt["environment"] = "staging-isolated"
        world.set_archive()
        self.assertWorldRefused(world, "^receipt source, producer or attempt mismatch$")

    def test_private_receipt_timing_matches_selected_attempt_job_start(self):
        world = PrivateWorld()
        world.receipt["recorded_at"] = iso(-1)
        world.set_archive()
        journey = world.jobs[-1]

        journey["started_at"] = world.receipt["identities"]["build-identity"]["observed_at"]
        self.assertEqual(world.verify()["result"], "satisfied")

        # Keep the same archive and attempt pins with all receipt times before job start.
        journey["started_at"] = iso(0)
        self.assertWorldRefused(world, "^journey timing mismatch$")

    def test_green_report_cannot_mix_private_track_fragments(self):
        pins = private_pins()
        for proof, field, foreign in (
            ("private-publication", "run_id", str(RUN_ID - 1)),
            ("private-library", "build_sha", OTHER_SHA),
            ("private-play", "track_id", "00000000-0000-4000-8000-000000000002"),
            ("private-anonymous-refusal", "run_id", str(RUN_ID - 1)),
            ("private-unlisted-refusal", "sha256", "0" * 64),
            ("private-cutoff-refusal", "run_attempt", "2"),
        ):
            with self.subTest(proof=proof, field=field):
                mixed = private_report(pins)
                path = f".//property[@name='{proof}']"
                mixed.find(path).set("value", private_report(pins, **{field: foreign}).find(path).get("value"))
                world = PrivateWorld(mixed)
                self.assertEqual(world.receipt["coverage"],
                                 [{"id": name, "outcome": "passed"} for name in PRIVATE_CASES])
                self.assertEqual(world.receipt["reasons"], ["private journey observed different tracks, builds or attempts"])
                self.assertNotIn("private_track", world.receipt)
                self.assertWorldRefused(world, "private track evidence")

    def test_green_report_requires_each_private_track_observation_once(self):
        for defect in ("missing", "duplicate", "misplaced"):
            with self.subTest(defect=defect):
                pins = private_pins()
                report = private_report(pins)
                upload = report.find("testcase[@name='upload-to-library']/properties")
                library = upload.find("property[@name='private-library']")
                if defect == "duplicate":
                    upload.append(copy.deepcopy(library))
                else:
                    upload.remove(library)
                    if defect == "misplaced":
                        report.find("testcase[@name='play']/properties").append(library)
                receipt = sa.produce(make_env(), ET.tostring(report), make_policy(PRIVATE_CONTRACT_BYTES),
                                     PRIVATE_CONTRACT_BYTES, MANIFEST, private_pins=pins,
                                     private_track_sha256=PRIVATE_AUDIO_SHA)
                self.assertEqual(receipt["result"], "incomplete")
                self.assertIn("private track evidence", receipt["reasons"][0])

    def test_green_report_requires_private_play_observation(self):
        report = private_report(private_pins())
        complete = PrivateWorld(report).receipt
        self.assertEqual(complete["result"], "satisfied")

        play = report.find("testcase[@name='play']/properties")
        play.remove(play.find("property[@name='private-play']"))

        incomplete = PrivateWorld(report).receipt
        self.assertEqual(incomplete["coverage"], complete["coverage"])
        self.assertEqual(incomplete["result"], "incomplete")
        self.assertEqual(incomplete["reasons"], ["missing private track evidence"])
        self.assertNotIn("private_track", incomplete)

    def test_private_track_must_match_selected_run_build_and_fixture(self):
        for field, foreign in (("run_id", str(RUN_ID - 1)), ("run_attempt", "2"),
                               ("build_sha", OTHER_SHA), ("sha256", "0" * 64)):
            with self.subTest(field=field):
                world = PrivateWorld(private_report(private_pins(), **{field: foreign}))
                self.assertEqual(world.receipt["result"], "incomplete")
                self.assertIn("private track evidence", world.receipt["reasons"][0])
                # Also reject a substituted summary in an otherwise green, digest-correct receipt.
                world = PrivateWorld()
                world.receipt["private_track"][field] = foreign
                world.set_archive()
                self.assertWorldRefused(world, "private track evidence")

    def test_private_receipt_requires_reviewed_policy_and_private_job(self):
        world = PrivateWorld()
        world.receipt["approved"] = True
        world.set_archive()
        empty_policy = dict(world.policy, approvals=[])
        self.assertWorldRefused(world, "unavailable or ambiguous", policy=empty_policy)
        world.jobs[-1] = World().jobs[-1]
        self.assertWorldRefused(world, "missing or ambiguous required staging job")

    def test_ordinary_report_and_producer_cannot_supply_private_cases(self):
        pins = private_pins()
        policy = make_policy(PRIVATE_CONTRACT_BYTES)
        for xml, args in ((make_junit(), {"private_pins": pins}), (ET.tostring(private_report(pins)), {})):
            receipt = sa.produce(make_env(), xml, policy, PRIVATE_CONTRACT_BYTES, MANIFEST, **args)
            self.assertEqual(receipt["result"], "incomplete")

    def test_contract_cannot_exclude_or_substitute_upload_case(self):
        for evidence in (None, [{"classname": "fixture", "name": "uploaded", "outcome": "passed"}]):
            contract = copy.deepcopy(PRIVATE_CONTRACT)
            upload = contract["criteria"][0]
            if evidence is None:
                upload.update(applicable=False, rationale="fixture already published")
            else:
                upload["evidence"] = evidence
            data = json.dumps(contract).encode()
            self.refused("private.*(criteria|criterion)", sa.approved_contract, make_policy(data), data)

    def test_upload_case_must_pass_once_on_first_attempt(self):
        pins = private_pins()
        for outcome in ("absent", "skipped", "failure", "flakyFailure", "duplicate", "retried", "unknown-retry"):
            with self.subTest(outcome=outcome):
                suite = private_report(pins)
                case = suite.find("testcase[@name='upload-to-library']")
                if outcome == "absent":
                    suite.remove(case)
                elif outcome == "duplicate":
                    suite.append(copy.deepcopy(case))
                elif outcome == "retried":
                    case.set("retry", "1")
                elif outcome == "unknown-retry":
                    del case.attrib["retry"]
                else:
                    ET.SubElement(case, outcome).text = "synthetic diagnostic"
                receipt = sa.produce(make_env(), ET.tostring(suite), make_policy(PRIVATE_CONTRACT_BYTES),
                                     PRIVATE_CONTRACT_BYTES, MANIFEST, private_pins=pins)
                self.assertEqual(receipt["result"], "incomplete")
                self.assertEqual(receipt["coverage"][0]["outcome"], "incomplete")
                self.assertNotIn("synthetic diagnostic", json.dumps(receipt))

    def test_private_receipt_requires_selected_run_workflow(self):
        world = PrivateWorld()
        routes = world.routes()
        self.assertEqual(world.verify(routes=routes)["result"], "satisfied")

        # Change only the selected run lookup. The receipt and attempt pins still match.
        for field, foreign in (("workflow_id", WORKFLOW_ID + 1),
                               ("path", ".github/workflows/other.yml")):
            with self.subTest(field=field):
                foreign_run = dict(world.run, **{field: foreign})
                self.assertWorldRefused(world, "^wrong, superseded or unsuccessful staging attempt$",
                                        routes={**routes, f"runs/{RUN_ID}": foreign_run})

    def test_private_jobs_and_deploy_steps_must_belong_to_the_attempt(self):
        world = PrivateWorld()
        stream = world.jobs[2]
        stream["run_attempt"] = 2
        self.assertWorldRefused(world, "did not pass in selected attempt")
        stream["run_attempt"] = 1
        stream["steps"][0]["conclusion"] = "skipped"
        self.assertWorldRefused(world, "private deploy step did not pass")
        stream["steps"][0]["conclusion"] = "success"
        world.jobs[-1]["steps"].pop(1)
        self.assertWorldRefused(world, "must succeed in order")

    def test_private_receipt_rejects_other_root_attempt_or_deployment(self):
        for field, value, reason in (("run_id", "99", "run or attempt mismatch"),
                                     ("run_attempt", "2", "run or attempt mismatch"),
                                     ("build_sha", OTHER_SHA, "invalid private deployment identity"),
                                     ("version_id", "30000000-0000-4000-8000-000000000001", "different deployment"),
                                     ("deployment_id", "40000000-0000-4000-8000-000000000001", "different deployment")):
            with self.subTest(field=field):
                world = PrivateWorld()
                world.receipt["deployments"]["staging-stream"][field] = value
                world.set_archive()
                self.assertWorldRefused(world, reason)
        world = PrivateWorld()
        world.artifact["name"] = sa.artifact_name(SHA, RUN_ID, 1)
        self.assertWorldRefused(world, "missing or ambiguous attempt-bound receipt")

    def test_private_producer_refuses_a_different_closing_identity(self):
        pins = private_pins()
        suite = private_report(pins)
        prop = suite.find("testcase[@name='build-identity-final']/properties/property")
        closing = json.loads(prop.get("value"))
        closing["deployments"]["staging-web"]["deployment_id"] = pins["staging-api"]["deployment_id"]
        prop.set("value", json.dumps(closing))
        receipt = sa.produce(make_env(), ET.tostring(suite), make_policy(PRIVATE_CONTRACT_BYTES),
                             PRIVATE_CONTRACT_BYTES, MANIFEST, private_pins=pins,
                             private_track_sha256=PRIVATE_AUDIO_SHA)
        self.assertEqual(receipt["result"], "incomplete")
        self.assertEqual(receipt["reasons"], ["private journey observed different deployment identities"])

    def test_intervening_stream_mutation_from_another_same_sha_run_refuses(self):
        world = PrivateWorld()
        world.add_other("skipped")
        other_id = RUN_ID - 1
        world.runs[-1].update(id=other_id, head_sha=SHA)
        jobs = world.others[other_id] = world.others.pop(101)
        stream = dict(jobs[0], name="staging-stream")
        jobs.append(stream)
        self.assertEqual(world.verify()["result"], "satisfied")
        stream["conclusion"] = "success"
        self.assertWorldRefused(world, "intervening staging deployment")

    def test_same_run_private_rerun_selects_only_the_current_attempt_artifact(self):
        world = PrivateWorld()
        prior_artifact = world.artifact
        routes = world.routes()
        world.run["run_attempt"] = 2
        rerun_jobs = [dict(job, run_attempt=2) for job in world.jobs]
        pins = {lane: dict(pin, run_attempt="2") for lane, pin in private_pins().items()}
        report = ET.tostring(private_report(pins, run_attempt="2"))
        world.receipt = sa.produce(make_env(GITHUB_RUN_ATTEMPT="2"), report, world.policy,
                                   world.contract_bytes, world.manifest, private_pins=pins,
                                   private_track_sha256=PRIVATE_AUDIO_SHA)
        world.set_archive()
        current_artifact = dict(world.artifact, id=ARTIFACT_ID + 1,
                                name=f"staging-admission-{SHA}-{RUN_ID}-2-private-music")
        routes[f"runs/{RUN_ID}/attempts/2/jobs?per_page=100&page=1"] = page("jobs", rerun_jobs)
        routes[f"artifacts/{ARTIFACT_ID + 1}/zip"] = world.archive
        artifact_listing = f"runs/{RUN_ID}/artifacts?per_page=100&page=1"

        routes[artifact_listing] = page("artifacts", [prior_artifact, current_artifact])
        actions = FakeActions(routes)
        verified = world.verify_with_actions(actions)
        self.assertEqual(verified["result"], "satisfied")
        self.assertEqual(verified["run_attempt"], "2")
        self.assertNotIn(f"artifacts/{ARTIFACT_ID}/zip", actions.calls)

        routes[artifact_listing] = page("artifacts", [prior_artifact])
        actions = FakeActions(routes)
        self.refused("missing or ambiguous attempt-bound receipt", world.verify_with_actions, actions)
        self.assertFalse(any(call.endswith("/zip") for call in actions.calls))


class PolicyTests(Base):
    def assert_installed_policy(self, root):
        policy = sa.parse_json((root / sa.POLICY).read_bytes())
        try:
            contract_bytes = (root / sa.CONTRACT).read_bytes()
        except FileNotFoundError:
            self.assertEqual(policy["approvals"], [])
            self.refused("unavailable or ambiguous", sa.approved_contract, policy, b"")
        else:
            sa.approved_contract(policy, contract_bytes)

    def test_checked_in_policy_matches_contract(self):
        self.assert_installed_policy(HERE.parent)

    def test_installed_policy_with_fixture_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            policy_path = root / sa.POLICY
            contract_path = root / sa.CONTRACT
            policy_path.parent.mkdir()
            policy_path.write_text(json.dumps(dict(make_policy(), approvals=[])), encoding="utf-8")
            self.assert_installed_policy(root)

            contract_bytes = CONTRACT_BYTES + b"\r\n"
            policy_path.write_text(json.dumps(make_policy(contract_bytes)), encoding="utf-8")
            contract_path.write_bytes(contract_bytes)
            self.assert_installed_policy(root)

            # The JSON is equivalent, but the approved bytes have changed.
            contract_path.write_bytes(CONTRACT_BYTES + b"\n")
            self.refused("unapproved product contract hash", self.assert_installed_policy, root)

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


class DraftPrivateContractTests(Base):
    CASES = ("invite-acceptance", "upload-to-library", "play", "seek", "queue",
             "anonymous-refused", "unlisted-refused", "revoked-or-expired-refused", "video-off")

    def setUp(self):
        self.contract_bytes = (HERE / "private-music-journey-contract.draft.json").read_bytes()
        self.policy = make_policy(self.contract_bytes)
        self.policy["approvals"][0]["spec_revision"] = sa.parse_json(self.contract_bytes)["spec_revision"]
        self.contract = sa.approved_contract(self.policy, self.contract_bytes)

    def private_suite(self):
        suite = private_report(private_pins())
        ET.SubElement(suite, "testcase", classname="private-music", name="invite-acceptance", retry="0")
        return suite

    def coverage(self, report):
        rows = sa.junit_evidence(ET.tostring(report), self.contract)[0]
        return {row["id"]: row["outcome"] for row in rows}

    def produce_receipt(self, report):
        return sa.produce(make_env(JOURNEY_GATE=""), ET.tostring(report), self.policy,
                          self.contract_bytes, MANIFEST, private_pins=private_pins(),
                          private_track_sha256=PRIVATE_AUDIO_SHA)

    def raw_playwright_report(self):
        # Playwright 1.65.1 writes the spec filename and outer test title.
        # Nested test.step() names do not become cases.
        spec = HERE.parent / "e2e/private-staging/journey.spec.ts"
        title = "private staging invite serves curated audio and refuses access at cutoff"
        self.assertIn(f"test('{title}',", spec.read_text(encoding="utf-8"))
        root = ET.Element("testsuites", tests="1", failures="0", skipped="0", errors="0")
        suite = ET.SubElement(root, "testsuite", name=spec.name, tests="1")
        ET.SubElement(suite, "testcase", name=title, classname=spec.name, time="0.001")
        return root

    def test_draft_matches_the_fixed_private_report_cases(self):
        self.assertEqual(self.contract["journey"], "private-music")
        self.assertEqual([row["id"] for row in self.contract["criteria"] if not row["applicable"]],
                         ["paid-signup"])
        root = self.raw_playwright_report()
        suite = self.private_suite()
        suite.set("name", "private candidate assertions")
        root.insert(0, suite)
        root.set("tests", str(len(root.findall(".//testcase"))))
        self.assertEqual(self.coverage(root), dict.fromkeys(self.CASES, "passed"))

    def test_each_selector_needs_one_first_attempt_pass(self):
        faults = ("missing", "duplicate", "wrong-class", "wrong-name", "no-retry-field",
                  "retried", "skipped", "failure", "expected-failure", "flakyFailure")
        for name in self.CASES:
            for fault in faults:
                with self.subTest(case=name, fault=fault):
                    suite = self.private_suite()
                    case = suite.find(f"./testcase[@name='{name}']")
                    if fault == "missing":
                        suite.remove(case)
                    elif fault == "duplicate":
                        suite.append(copy.deepcopy(case))
                    elif fault == "wrong-class":
                        case.set("classname", "journey.spec.ts")
                    elif fault == "wrong-name":
                        case.set("name", f"[staging] {name}")
                    elif fault == "no-retry-field":
                        del case.attrib["retry"]
                    elif fault == "retried":
                        case.set("retry", "1")
                    elif fault == "expected-failure":
                        ET.SubElement(ET.SubElement(case, "properties"), "property", name="fail")
                    else:
                        ET.SubElement(case, fault)
                    expected = dict.fromkeys(self.CASES, "passed")
                    expected[name] = "incomplete"
                    self.assertEqual(self.coverage(suite), expected)

    def test_repeated_case_cannot_replace_a_required_case_in_receipt(self):
        suite = self.private_suite()
        expected = dict.fromkeys(self.CASES, "passed")
        receipt = self.produce_receipt(suite)
        self.assertEqual(receipt["result"], "satisfied")
        self.assertEqual(receipt["reasons"], [])
        self.assertEqual({row["id"]: row["outcome"] for row in receipt["coverage"]}, expected)

        # Neither case carries track properties, so only the selector changes.
        suite.find("testcase[@name='queue']").set("name", "seek")
        receipt = self.produce_receipt(suite)
        self.assertEqual(receipt["result"], "incomplete")
        self.assertEqual(receipt["reasons"], ["applicable product evidence incomplete, skipped or failed"])
        expected.update(seek="incomplete", queue="incomplete")
        self.assertEqual({row["id"]: row["outcome"] for row in receipt["coverage"]}, expected)

    def test_deliberate_skip_names_the_incomplete_case_without_its_reason(self):
        suite = self.private_suite()
        seek = suite.find("testcase[@name='seek']")
        reason = "The manual playback check ran out of time."
        ET.SubElement(seek, "skipped", message=reason)

        receipt = self.produce_receipt(suite)
        self.assertEqual(receipt["result"], "incomplete")
        coverage = {row["id"]: row["outcome"] for row in receipt["coverage"]}
        self.assertEqual(coverage, dict.fromkeys(self.CASES, "passed") | {"seek": "incomplete"})
        self.assertNotIn(reason, json.dumps(receipt))

    def test_raw_playwright_pass_cannot_supply_private_cases(self):
        root = self.raw_playwright_report()
        self.refused("private identity cases did not pass exactly once",
                     self.coverage, root)
        suite = ET.SubElement(root, "testsuite", name="private build identities")
        private = self.private_suite()
        for phase in sa.IDENTITY_PHASES:
            suite.append(private.find(f"./testcase[@name='{phase}']"))
        root.set("tests", "3")
        self.assertEqual(self.coverage(root), dict.fromkeys(self.CASES, "incomplete"))


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

    def test_relevant_history_above_search_ceiling(self):
        w = World()
        # Model the observed 1,113 results: 647 in a bounded slice, then 466
        # in the open tail (including the candidate), plus irrelevant lifetime history.
        for start, count, age in ((200, 647, 35), (900, 465, 10), (1400, 125, 100)):
            w.runs.extend(dict(w.run, id=rid, head_sha=OTHER_SHA, created_at=iso(-age * 24 * 60),
                               updated_at=iso(-30)) for rid in range(start, start + count))
        base = f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push"
        cutoff = (sa.timestamp(iso(-3)) - timedelta(days=66)).date()
        old_query = f"{base}&created=%3E%3D{cutoff}&per_page=100&page=1"
        routes = w.routes()
        routes[old_query] = page("workflow_runs", w.runs[:100], total=1113)
        actions = FakeActions(routes)
        self.assertEqual(w.verify_with_actions(actions)["result"], "satisfied")
        self.assertTrue(any("&created=" in route and route.endswith("page=7") for route in actions.calls))
        self.assertTrue(any("&created=%3E%3D" in route and route.endswith("page=5") for route in actions.calls))
        self.assertNotIn(old_query, actions.calls)
        self.assertNotIn(f"{base}&per_page=100&page=1", actions.calls)

    def test_incomplete_relevant_discovery_refuses_without_partial_proof(self):
        w = World()
        w.add_other("success")
        w.runs[1]["created_at"] = iso(-60 * 24 * 60)
        for filter_ in ("&created=%3E%3D", "&status=requested"):
            with self.subTest(filter_=filter_):
                routes = w.routes()
                route = next(r for r in routes if filter_ in r)
                routes[route]["total_count"] = 1000
                actions = FakeActions(routes)
                self.refused("incomplete or exhausted workflow_runs", w.verify_with_actions, actions)
                self.assertFalse(any("jobs?filter=all" in call for call in actions.calls))

    def test_conflict_search_covers_utc_boundaries_and_all_age_pending(self):
        # The local calendar date is Nov 19, but the opening UTC date is Nov 18.
        opening = sa.timestamp("2026-11-19T00:30:00+02:00")
        base = f"workflows/{WORKFLOW_ID}/runs?branch=main&event=push"
        dates = {
            "created=2026-09-13..2026-10-04": ("2026-09-13T00:00:00Z", "2026-10-04T23:59:59Z"),
            "created=2026-10-05..2026-10-26": ("2026-10-05T00:00:00Z", "2026-10-26T23:59:59Z"),
            "created=%3E%3D2026-10-27": ("2026-10-27T00:00:00Z", "2026-11-20T00:00:00Z"),
        }
        row = World().run
        rows = {query: [dict(row, id=2 * ix + offset, created_at=stamp) for offset, stamp in enumerate(stamps)]
                for ix, (query, stamps) in enumerate(dates.items(), 50)}
        statuses = ("queued", "in_progress", "waiting", "pending", "requested")
        rows.update({f"status={status}": [dict(row, id=200 + ix, status=status,
                                              conclusion=None, created_at="2026-01-01T00:00:00Z")]
                     for ix, status in enumerate(statuses)})
        recent = dict(rows["created=%3E%3D2026-10-27"][0], status="queued", conclusion=None)
        rows["created=%3E%3D2026-10-27"][0] = recent
        rows["status=queued"].append(recent)
        routes = {f"{base}&{query}&per_page=100&page=1": page("workflow_runs", batch)
                  for query, batch in rows.items()}
        actions = FakeActions(routes)
        self.assertCountEqual(actions.conflicts(str(WORKFLOW_ID), opening),
                              [run for batch in rows.values() for run in batch if run is not recent] + [recent])
        self.assertCountEqual(actions.calls, routes)

        # Valid overlap is deduped, changed attempt metadata and out-of-scope rows are not.
        queued = f"{base}&status=queued&per_page=100&page=1"
        first = f"{base}&created=2026-09-13..2026-10-04&per_page=100&page=1"
        oldest = rows["created=2026-09-13..2026-10-04"][0]
        for route, wrong, reason in (
            (queued, dict(recent, run_attempt=2), "changed during conflict discovery"),
            (queued, dict(recent, status="completed"), "outside requested status"),
            (first, recent, "outside requested creation dates"),
            (first, dict(oldest, created_at="2026-09-12T23:59:59Z"), "outside requested creation dates"),
            (first, dict(oldest, event="workflow_dispatch"), "outside requested workflow scope"),
            (first, dict(oldest, head_branch="other"), "outside requested workflow scope"),
            (first, dict(oldest, workflow_id=WORKFLOW_ID + 1), "outside requested workflow scope"),
        ):
            with self.subTest(reason=reason, wrong=wrong):
                invalid = FakeActions({**routes, route: page("workflow_runs", [wrong])})
                self.refused(reason, invalid.conflicts, str(WORKFLOW_ID), opening)

    def test_conflict_partitions_share_the_actions_network_deadline(self):
        empty = io.BytesIO(json.dumps(page("workflow_runs", [])).encode())
        with patch.object(sa.time, "monotonic", side_effect=[100, 100, 219, 220]):
            actions = sa.Actions(REPO, "synthetic-token")
            with patch.object(actions.opener, "open", return_value=empty) as open_:
                self.refused("time budget exhausted", actions.conflicts, str(WORKFLOW_ID), NOW)
                self.assertEqual(open_.call_count, 1)

    def test_final_actions_response_must_finish_before_deadline(self):
        w = World()
        routes = w.routes()
        now = 100
        candidate_reads = 0

        class FinalResponse(io.BytesIO):
            def read(self, size=-1):
                nonlocal now
                now = 220
                return super().read(size)

        def open_(request, timeout):
            nonlocal candidate_reads
            route = request.full_url.removeprefix(actions.base)
            data = routes[route]
            body = data if isinstance(data, bytes) else json.dumps(data).encode()
            if route == f"runs/{RUN_ID}":
                candidate_reads += 1
                if candidate_reads == 2:
                    return FinalResponse(body)
            return io.BytesIO(body)

        with patch.object(sa.time, "monotonic", side_effect=lambda: now):
            actions = sa.Actions(REPO, "synthetic-token")
            with patch.object(actions.opener, "open", side_effect=open_):
                self.refused("time budget exhausted", w.verify_with_actions, actions)
        self.assertEqual(candidate_reads, 2)

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
        self.refused("invalid JSON object", FakeActions({first: []}).runs, route)
        self.refused("invalid workflow_runs row", FakeActions({first: page("workflow_runs", [None])}).runs, route)
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
        self.init_repo()

    def init_repo(self, initial_branch="fixture"):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.objects = {}
        self.git("init", "--bare", "--quiet", f"--initial-branch={initial_branch}")
        self.assertEqual(self.git("symbolic-ref", "HEAD").decode().strip(),
                         f"refs/heads/{initial_branch}")
        self.contract = json.dumps(dict(CONTRACT, criteria=[{
            "id": "signup", "applicable": True, "evidence": [{
                "classname": "signup.spec.ts", "name": "signup", "outcome": "passed"}]}])).encode()
        self.policy = make_policy(self.contract)
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
        })
        self.install_contract(self.contract, self.policy)

    def install_contract(self, contract, policy):
        self.contract, self.policy = contract, policy
        for name, data in ((sa.POLICY, json.dumps(policy).encode()), (sa.CONTRACT, contract)):
            self.files[name] = ("100644", data)
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

    def install_private_contract(self):
        self.install_contract(PRIVATE_CONTRACT_BYTES, make_policy(PRIVATE_CONTRACT_BYTES))
        fixture = sa.PRIVATE_TRACK_FILE
        self.files[fixture] = ("100644", PRIVATE_AUDIO)
        return fixture

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

    def run_main(self, candidate_files=None, trusted_files=None, current_files=None, private=False,
                 mode="bootstrap-maintenance", world_factory=None, live_error=None):
        candidate = self.commit(candidate_files or self.files)
        trusted = self.commit(trusted_files or self.files, candidate)
        current = self.commit(current_files or trusted_files or self.files, trusted)
        # Model a detached workflow checkout, independent of init.defaultBranch.
        self.git("update-ref", "--no-deref", "HEAD", trusted)
        # Model checkout's full-history fallback: origin/main names the captured
        # dispatch commit, even though the remote branch has already advanced.
        self.git("update-ref", "refs/remotes/origin/main", trusted)
        self.git("update-ref", "refs/heads/main", current)
        self.git("config", "remote.origin.url", str(self.root))
        with patch.dict(globals(), SHA=candidate):
            if world_factory is not None:
                world = world_factory()
            elif private:
                world = PrivateWorld()
            else:
                world = World(policy=self.policy, contract_bytes=self.contract,
                              xml=make_junit(final_sha=candidate).replace(
                                  b'classname="journey"', b'classname="signup.spec.ts"'))
            actions = FakeActions(world.routes())
        env = make_env(GITHUB_SHA=trusted, DEPLOY_SHA=candidate, GITHUB_EVENT_NAME="workflow_dispatch",
                       GITHUB_REPOSITORY_ID=str(REPO_ID), GITHUB_TOKEN="synthetic-token", DEPLOY_MODE=mode)
        with patch.object(sa, "ROOT", self.root), patch.dict(sa.os.environ, env, clear=True), \
                patch.object(sa.sys, "argv", ["staging_admission.py", "verify"]), \
                patch.object(sa, "Actions", return_value=actions), \
                patch.object(sa, "current_identity", side_effect=live_error) as live, \
                patch("sys.stdout", new=io.StringIO()) as output:
            with patch.object(sa, "git", wraps=sa.git) as source_git:
                try:
                    sa.main()
                except sa.Refused:
                    self.assertEqual(output.getvalue(), "")
                    raise
            live.assert_called_once_with(sa.os.environ, candidate)
            source_git.assert_any_call("merge-base", "--is-ancestor", candidate, current)
            trees = (*sa.TRUSTED_TREES, "fixtures/synthetic-flac/private-staging") if private else sa.TRUSTED_TREES
            source_git.assert_any_call("ls-tree", "-z", current, "--", *trees, *sa.TRUSTED_FILES)
            source_git.assert_any_call("show", f"{current}:migrations/manifest.toml")
        self.assertEqual(self.git("rev-parse", "HEAD").decode().strip(), trusted)
        self.assertEqual(self.git("rev-parse", "origin/main").decode().strip(), trusted)
        self.assertFalse((self.root / "FETCH_HEAD").exists())
        self.assertIn(f"artifacts/{ARTIFACT_ID}/zip", actions.calls)
        return world, output.getvalue().splitlines()

    def test_main_accepts_matching_closure_despite_unrelated_main_movement(self):
        current = dict(self.files, **{"docs/unrelated.md": ("100644", b"unrelated main change")})
        _, lines = self.run_main(current_files=current)
        self.assertEqual(len(lines), 1)
        self.assertIn("not operational acceptance", lines[0])

    def test_private_main_prints_only_verified_candidate_pins(self):
        self.install_private_contract()

        def receipt_with_extra_fields():
            world = PrivateWorld()
            world.receipt["private_diagnostics"] = "synthetic-private-value"
            world.receipt["identities"]["build-identity"]["operator_note"] = "synthetic-private-value"
            world.set_archive()
            return world

        world, lines = self.run_main(private=True, mode="promote", world_factory=receipt_with_extra_fields)
        self.assertEqual(len(lines), 2)
        self.assertIn("not operational acceptance", lines[1])
        self.assertEqual(json.loads(lines[0]), {"verified_private_music_staging": {
            "schema": 1, "sha": world.receipt["sha"], "run_id": str(RUN_ID), "run_attempt": "1",
            "artifact_name": world.artifact["name"], "deployments": world.receipt["deployments"],
            "migration_manifest_sha256": sa.digest(MANIFEST),
            "contract_sha256": sa.digest(PRIVATE_CONTRACT_BYTES),
            "policy_sha256": sa.digest(json.dumps(world.policy, sort_keys=True).encode()),
            "observed_at": {phase: world.receipt["identities"][phase]["observed_at"]
                            for phase in sa.IDENTITY_PHASES},
        }})

    def test_private_main_prints_no_pins_on_live_identity_refusal(self):
        self.install_private_contract()
        self.refused("current staging identity drift", self.run_main, private=True, mode="promote",
                     live_error=sa.Refused("current staging identity drift"))

    def test_private_producer_cli_binds_candidate_audio(self):
        fixture = self.install_private_contract()
        report = self.root / "e2e/private-staging-results/admission.xml"
        report.parent.mkdir(parents=True)
        manifest = self.root / "migrations/manifest.toml"
        manifest.parent.mkdir()
        manifest.write_bytes(MANIFEST)
        for audio, result in ((PRIVATE_AUDIO, "satisfied"), (b"different synthetic input", "incomplete")):
            with self.subTest(result=result):
                candidate = self.commit(dict(self.files, **{fixture: ("100644", audio)}))
                with patch.dict(globals(), SHA=candidate):
                    pins = private_pins()
                    report.write_bytes(ET.tostring(private_report(pins)))
                    env = make_env(PRIVATE_STAGING_DEPLOYMENTS=json.dumps(pins), JOURNEY_GATE="")
                with patch.object(sa, "ROOT", self.root), patch.dict(sa.os.environ, env, clear=True), \
                        patch.object(sa.sys, "argv", ["staging_admission.py", "produce-private"]), \
                        patch("sys.stdout", new=io.StringIO()):
                    sa.main()
                receipt = json.loads((self.root / "staging-admission.json").read_bytes())
                self.assertEqual(receipt["result"], result)
                if result == "satisfied":
                    self.assertEqual(receipt["private_track"]["sha256"], PRIVATE_AUDIO_SHA)
                else:
                    self.assertIn("private track evidence", receipt["reasons"][0])

    def test_private_main_binds_synthetic_audio_tree(self):
        fixture = self.install_private_contract()
        self.run_main(private=True, mode="promote")
        corrected = dict(self.files, **{fixture: ("100644", b"different synthetic input")})
        self.refused("producer.*drift", self.run_main, current_files=corrected, private=True, mode="promote")
        self.refused("private track evidence", self.run_main, candidate_files=corrected,
                     trusted_files=corrected, current_files=corrected, private=True, mode="promote")

    def test_promotion_refuses_missing_approval(self):
        self.install_private_contract()
        self.install_contract(self.contract, dict(self.policy, approvals=[]))
        self.refused("approved product contract unavailable", self.run_main, private=True, mode="promote")

    def test_promotion_refuses_ordinary_and_billing_admission(self):
        # Each substitute independently verifies on its approved ordinary route.
        self.run_main()
        self.refused("requires the approved private-music journey", self.run_main, mode="promote")

        billing = json.dumps(dict(CONTRACT, criteria=[{
            "id": "billing", "applicable": True, "evidence": [{
                "classname": "billing.spec.ts", "name": "checkout", "outcome": "passed"}]}])).encode()
        self.install_contract(billing, make_policy(billing))

        def billing_world():
            xml = make_junit().replace(b'classname="journey" name="signup"',
                                       b'classname="billing.spec.ts" name="checkout"')
            return World(policy=self.policy, contract_bytes=self.contract, xml=xml)

        self.run_main(world_factory=billing_world)
        self.refused("requires the approved private-music journey", self.run_main,
                     mode="promote", world_factory=billing_world)

    def test_promotion_refuses_missing_skipped_or_malformed_private_evidence(self):
        self.install_private_contract()
        _, lines = self.run_main(private=True, mode="promote")
        self.assertIn("verified_private_music_staging", json.loads(lines[0]))

        def without_journey():
            world = PrivateWorld()
            world.jobs.pop()
            return world

        def skipped_journey():
            world = PrivateWorld()
            world.jobs[-1]["conclusion"] = "skipped"
            return world

        def without_private_artifact():
            world = PrivateWorld()
            world.artifact = None
            return world

        def malformed_private_artifact():
            world = PrivateWorld()
            # Keep the successful jobs and a matching archive digest. Only the receipt is invalid.
            world.set_archive(make_zip([("staging-admission.json", '{"schema":')]))
            return world

        for factory, reason in ((without_journey, "missing or ambiguous required staging job"),
                                (skipped_journey, "required job did not pass in selected attempt"),
                                (without_private_artifact, "missing or ambiguous attempt-bound receipt"),
                                (malformed_private_artifact, "malformed JSON")):
            with self.subTest(fixture=factory.__name__):
                self.refused(reason, self.run_main, private=True, mode="promote", world_factory=factory)

    def test_promotion_refuses_receipt_for_other_build_attempt_or_journey(self):
        self.install_private_contract()
        for field, value, reason in (("sha", OTHER_SHA, "receipt source, producer or attempt mismatch"),
                                     ("run_attempt", "2", "receipt source, producer or attempt mismatch"),
                                     ("journey", "billing", "receipt journey mismatch")):
            with self.subTest(field=field):
                def wrong_receipt():
                    world = PrivateWorld()
                    name = world.artifact["name"]
                    world.receipt[field] = value
                    world.set_archive()
                    world.artifact["name"] = name
                    return world

                self.refused(reason, self.run_main, private=True, mode="promote", world_factory=wrong_receipt)

    def test_main_source_binding_is_independent_of_initial_branch(self):
        for initial_branch in ("main", "master"):
            with self.subTest(initial_branch=initial_branch):
                self.init_repo(initial_branch)
                unrelated = dict(self.files, **{"docs/unrelated.md": ("100644", b"unrelated main change")})
                self.run_main(current_files=unrelated)
                changed = dict(self.files, **{"e2e/helpers/accounts.ts": ("100644", b"corrected input")})
                self.refused("producer.*drift", self.run_main, trusted_files=changed)
                self.refused("producer.*drift", self.run_main, current_files=changed)
                changed = dict(self.files, **{"migrations/manifest.toml": ("100644", b"new manifest")})
                self.refused("current migration manifest drift", self.run_main, current_files=changed)

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

    def test_main_refuses_current_main_only_policy_revocation(self):
        revoked = dict(self.files, **{sa.POLICY: ("100644", json.dumps(
            dict(self.policy, approvals=[])).encode())})
        self.refused("producer.*drift", self.run_main, current_files=revoked)

    def test_main_refuses_current_main_only_manifest_drift(self):
        changed = dict(self.files, **{"migrations/manifest.toml": ("100644", b"new manifest")})
        self.refused("current migration manifest drift", self.run_main, current_files=changed)

    def test_main_refuses_unavailable_remote_main(self):
        real_git = sa.git
        def unavailable(*args):
            if args[0] == "ls-remote":
                raise sa.Refused("current main unavailable")
            return real_git(*args)
        with patch.object(sa, "git", side_effect=unavailable):
            self.refused("current main unavailable", self.run_main)

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


class CurrentMainTests(Base):
    def resolve(self, responses=None, commit=SHA, fail=None):
        responses = iter(responses if responses is not None else [
            f"{SHA}\trefs/heads/main\n".encode()] * 2)
        def command(*args):
            if fail == args[0]:
                raise sa.Refused("current main object unavailable")
            if args[0] == "ls-remote":
                return next(responses)
            if args[0] == "rev-parse":
                return (commit + "\n").encode()
            return b""
        with patch.object(sa, "git", side_effect=command) as git:
            result = sa.current_main(OTHER_SHA)
            git.assert_any_call("fetch", "--no-tags", "--no-write-fetch-head", "--refmap=", "origin", SHA)
            git.assert_any_call("merge-base", "--is-ancestor", OTHER_SHA, SHA)
            return result

    def test_pins_remote_commit(self):
        self.assertEqual(self.resolve(), SHA)

    def test_refuses_missing_malformed_ambiguous_or_foreign_ref(self):
        for result in (b"", b"not-a-sha\trefs/heads/main\n", f"{SHA}\trefs/heads/other\n".encode(),
                       f"{SHA}\trefs/heads/main\n{OTHER_SHA}\trefs/heads/main\n".encode()):
            with self.subTest(response=result):
                self.refused("incoherent current main", self.resolve, [result])

    def test_refuses_main_movement_during_fetch(self):
        self.refused("main moved", self.resolve, [f"{sha}\trefs/heads/main\n".encode()
                                                  for sha in (SHA, OTHER_SHA)])

    def test_refuses_noncommit_or_mismatched_object(self):
        self.refused("not the resolved commit", self.resolve, commit=OTHER_SHA)
        self.refused("object unavailable", self.resolve, fail="rev-parse")

    def test_refuses_network_or_fetch_failure(self):
        for command in ("ls-remote", "fetch"):
            with self.subTest(command=command):
                self.refused("object unavailable", self.resolve, fail=command)

    def test_refuses_dispatch_not_ancestor_of_current_main(self):
        self.refused("object unavailable", self.resolve, fail="merge-base")

    def test_fetches_missing_remote_commit_without_changing_local_refs(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            remote, local = root / "remote.git", root / "local.git"
            def git_at(path, *args, data=None):
                env = dict(os.environ, GIT_AUTHOR_NAME="Fixture", GIT_COMMITTER_NAME="Fixture",
                           GIT_AUTHOR_EMAIL="fixture@example.invalid", GIT_COMMITTER_EMAIL="fixture@example.invalid")
                return subprocess.check_output(["git", "-C", str(path), *args], input=data,
                                               stderr=subprocess.PIPE, env=env, timeout=15)
            for path in (remote, local):
                path.mkdir()
                git_at(path, "init", "--bare", "--quiet", "--initial-branch=fixture")
            tree = git_at(remote, "mktree", data=b"").decode().strip()
            trusted = git_at(remote, "commit-tree", tree, "-m", "dispatch").decode().strip()
            git_at(remote, "update-ref", "refs/heads/main", trusted)
            git_at(local, "config", "remote.origin.url", str(remote))
            git_at(local, "config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")
            git_at(local, "fetch", "--quiet", "origin")
            git_at(local, "update-ref", "HEAD", trusted)
            before = git_at(local, "show-ref")
            fetch_head = (local / "FETCH_HEAD").read_bytes()
            current = git_at(remote, "commit-tree", tree, "-p", trusted, "-m", "main advanced").decode().strip()
            git_at(remote, "update-ref", "refs/heads/main", current)
            with self.assertRaises(subprocess.CalledProcessError):
                git_at(local, "cat-file", "-e", current)
            with patch.object(sa, "ROOT", local):
                self.assertEqual(sa.current_main(trusted), current)
            self.assertEqual(git_at(local, "cat-file", "-t", current), b"commit\n")
            self.assertEqual(git_at(local, "show-ref"), before)
            self.assertEqual((local / "FETCH_HEAD").read_bytes(), fetch_head)
            self.assertEqual(git_at(local, "rev-parse", "HEAD").decode().strip(), trusted)


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
        verification = evidence.split("      - name: Verify candidate deployment and accepted journey receipt\n")[1]
        self.assertIn("if: inputs.mode != 'recover-worker'", verification)
        self.assertIn("DEPLOY_SHA: ${{ needs.preflight.outputs.sha }}", verification)
        self.assertIn("DEPLOY_MODE: ${{ inputs.mode }}", verification)
        self.assertIn("run: python3 -I ci/staging_admission.py verify", verification)
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


class StagingPushReadbackTests(unittest.TestCase):
    SQUASH = "c" * 40
    MANUAL = "e" * 40
    OTHER_WORKFLOW = "f" * 40

    def setUp(self):
        self.world = World()

    def comparison(self, head, status="ahead", *, merge_base=None, ahead=1, behind=0):
        return {"base_commit": {"sha": self.SQUASH},
                "merge_base_commit": {"sha": merge_base or self.SQUASH},
                "status": status, "ahead_by": ahead, "behind_by": behind, "total_commits": ahead,
                "commits": [{"sha": head}] if ahead else []}

    def compare_route(self, head):
        return f"compare/{self.SQUASH}...{head}"

    def snapshot(self):
        run = self.world.run
        queued = dict(run, id=96, run_number=10, head_sha=OTHER_SHA, status="queued", conclusion=None)
        manual = dict(run, id=97, run_number=20, head_sha=self.MANUAL, event="workflow_dispatch")
        other = dict(run, id=98, run_number=21, head_sha=self.OTHER_WORKFLOW,
                     workflow_id=6, path=".github/workflows/check.yml")
        admitted = dict(run, run_number=30)
        # Queue rejection appears as a failed run with no attempt job rows.
        rejected = dict(run, id=99, run_number=25, head_sha=self.SQUASH, conclusion="failure")
        jobs = {}
        for entry in (admitted, manual, other):
            jobs[f"runs/{entry['id']}/attempts/1/jobs"] = page("jobs", [
                dict(job, run_id=entry["id"], head_sha=entry["head_sha"]) for job in self.world.jobs
            ])
        jobs["runs/99/attempts/1/jobs"] = page("jobs", [])
        jobs["runs/96/attempts/1/jobs"] = page("jobs", [dict(
            self.world.jobs[-1], run_id=96, head_sha=OTHER_SHA, status="queued", conclusion=None,
            runner_id=0, completed_at=None, steps=[],
        )])
        return {
            "workflow": self.world.workflow,
            "runs": page("workflow_runs", [rejected, other, admitted, queued, manual]),
            "jobs": jobs,
            "comparisons": {self.compare_route(head): self.comparison(head)
                            for head in (SHA, self.MANUAL, self.OTHER_WORKFLOW)}
                           | {self.compare_route(self.SQUASH): self.comparison(self.SQUASH, "identical", ahead=0),
                              self.compare_route(OTHER_SHA): self.comparison(
                                  OTHER_SHA, "behind", merge_base=OTHER_SHA, ahead=0, behind=2)},
        }

    def readback(self, evidence):
        raw = evidence if isinstance(evidence, bytes) else json.dumps(evidence).encode()
        return subprocess.run(
            [sys.executable, "-E", "-s", "-B", str(HERE / "staging_push_readback.py"),
             self.SQUASH, REPO, str(REPO_ID)], input=raw, capture_output=True, check=False,
        )

    def assert_pending(self, evidence):
        result = self.readback(evidence)
        self.assertEqual((result.returncode, result.stderr, json.loads(result.stdout)),
                         (1, b"", {"status": "pending"}))

    def add_completed_push(self, snapshot, *, run_id, run_number):
        head = "d" * 40
        runs = snapshot["runs"]["workflow_runs"]
        snapshot["runs"] = page("workflow_runs", [*runs, dict(
            self.world.run, id=run_id, run_number=run_number, head_sha=head,
        )])
        snapshot["comparisons"][self.compare_route(head)] = self.comparison(head)
        jobs = [dict(job, run_id=run_id, head_sha=head, started_at=iso(6), completed_at=iso(7))
                for job in self.world.jobs]
        jobs[-1].update(started_at=iso(8), completed_at=iso(10))
        snapshot["jobs"][f"runs/{run_id}/attempts/1/jobs"] = page("jobs", jobs)
        return head, jobs

    def test_nominates_ordinary_descendant_for_exact_attempt_verification(self):
        snapshot = self.snapshot()
        result = self.readback(snapshot)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        nomination = json.loads(result.stdout)
        self.assertEqual(nomination, {"status": "nominated", "sha": SHA,
                                     "run_id": str(RUN_ID), "run_attempt": "1"})
        receipt = self.world.verify(sha=nomination["sha"])
        self.assertEqual(receipt["result"], "satisfied")
        for key in ("sha", "run_id", "run_attempt"):
            self.assertEqual(receipt[key], nomination[key])

        other_runs = [run for run in snapshot["runs"]["workflow_runs"] if run["id"] != RUN_ID]
        snapshot["runs"] = page("workflow_runs", other_runs)
        self.assert_pending(snapshot)

    def test_most_recent_completed_journey_wins_even_when_creation_order_differs(self):
        for run_id, run_number in ((102, 31), (94, 8)):
            with self.subTest(run_number=run_number):
                snapshot = self.snapshot()
                head, jobs = self.add_completed_push(snapshot, run_id=run_id, run_number=run_number)
                result = self.readback(snapshot)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout), {"status": "nominated", "sha": head,
                                                             "run_id": str(run_id), "run_attempt": "1"})
                for job, original in zip(jobs, self.world.jobs):
                    job.update(started_at=original["started_at"], completed_at=original["completed_at"])
                self.assert_pending(snapshot)

    def test_completed_skipped_attempt_does_not_mask_qualifying_push(self):
        snapshot = self.snapshot()
        _, jobs = self.add_completed_push(snapshot, run_id=95, run_number=9)
        for job in jobs:
            job.update(conclusion="skipped", started_at=None, completed_at=None, steps=[])
        result = self.readback(snapshot)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), {"status": "nominated", "sha": SHA,
                                                     "run_id": str(RUN_ID), "run_attempt": "1"})

        jobs[-1]["run_attempt"] = 2
        self.assert_pending(snapshot)

    def test_ancestry_must_be_bound_to_the_selected_push_head(self):
        head_route = self.compare_route(SHA)
        for outcome, comparison in (
            ("behind", self.comparison(SHA, "behind", merge_base=SHA, ahead=0, behind=2)),
            ("diverged", self.comparison(SHA, "diverged", merge_base=OTHER_SHA, behind=2)),
            ("wrong head", self.comparison(OTHER_SHA)),
            ("wrong base", self.comparison(SHA) | {"base_commit": {"sha": OTHER_SHA}}),
            ("wrong merge base", self.comparison(SHA, merge_base=OTHER_SHA)),
            ("partial comparison", self.comparison(SHA, ahead=2)),
        ):
            with self.subTest(outcome=outcome):
                snapshot = self.snapshot()
                snapshot["comparisons"][head_route] = comparison
                self.assert_pending(snapshot)

        snapshot = self.snapshot()
        del snapshot["comparisons"][head_route]
        self.assert_pending(snapshot)

        snapshot = self.snapshot()
        admitted = snapshot["runs"]["workflow_runs"][2]
        admitted["head_sha"] = self.SQUASH
        for job in snapshot["jobs"][f"runs/{RUN_ID}/attempts/1/jobs"]["jobs"]:
            job["head_sha"] = self.SQUASH
        result = self.readback(snapshot)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["sha"], self.SQUASH)

    def test_incomplete_or_changed_attempt_evidence_stays_pending(self):
        jobs_route = f"runs/{RUN_ID}/attempts/1/jobs"
        for fault in ("partial runs", "partial jobs", "zero jobs", "other attempt", "other run",
                      "other job SHA", "rerun", "superseded run", "other repository"):
            with self.subTest(fault=fault):
                snapshot = self.snapshot()
                runs, jobs = snapshot["runs"], snapshot["jobs"][jobs_route]
                admitted = runs["workflow_runs"][2]
                if fault == "partial runs":
                    runs["total_count"] += 1
                elif fault == "partial jobs":
                    jobs["total_count"] += 1
                elif fault == "zero jobs":
                    snapshot["jobs"][jobs_route] = page("jobs", [])
                elif fault == "other attempt":
                    jobs["jobs"][-1]["run_attempt"] = 2
                elif fault == "other run":
                    jobs["jobs"][-1]["run_id"] = 101
                elif fault == "other job SHA":
                    jobs["jobs"][-1]["head_sha"] = OTHER_SHA
                elif fault == "rerun":
                    admitted["run_attempt"] = 2
                elif fault == "superseded run":
                    runs["workflow_runs"][0].update(id=101, run_number=31, head_sha=SHA)
                else:
                    admitted["head_repository"] = {"full_name": REPO, "id": REPO_ID + 1}
                self.assert_pending(snapshot)

        self.assert_pending(b'{"runs":{},"runs":{}}')


if __name__ == "__main__":
    unittest.main()
