#!/usr/bin/env python3
"""Attempt-bound staging receipts. No approval enrollment or operational PASS.

The production CLI reads only reviewed policy from its main workflow checkout,
Git objects and authenticated Actions metadata. Offline fixtures call the pure
functions; there is deliberately no production policy/API/local-receipt override.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zipfile
from datetime import datetime, timedelta, timezone

ROOT = Path(__file__).resolve().parent.parent
POLICY = "ci/staging-admission-policy.json"
CONTRACT = "ci/product-journey-contract.json"
WORKFLOW = ".github/workflows/deploy.yml"
JOURNEY = "post-deploy-e2e / full journey (signup → library → meter → limits)"
RECORD = "Record candidate admission receipt"
UPLOAD = "Upload candidate admission receipt"
# Bind conservative dependency roots, not a hand-picked set of test helpers.
# The journey loads every e2e spec, imports helpers and reads/spawns root inputs.
# Tree object IDs include all committed descendants, names, types and modes;
# package/lock/config changes and new/deleted dependencies cannot be missed.
# Changes outside these roots still do not expire evidence just by moving main.
TRUSTED_TREES = (
    "e2e", ".github", "fixtures/cc0", "config", "workers", "crates/nntune-config",
)
TRUSTED_FILES = (
    "ci/staging_admission.py", POLICY, CONTRACT, "bin/smoke.mjs", "Cargo.toml", "README.md",
    "apps/web/wrangler.toml", "crates/nntune-core/src/transcode.rs",
    "containers/mb-import/Dockerfile", "docs/api-errors.md", "docs/cost-basis.md",
)
SHA = re.compile(r"[0-9a-f]{40}")
HASH = re.compile(r"[0-9a-f]{64}")
LABEL = re.compile(r"[A-Za-z0-9_.:/@ -]{1,200}")
MAX_JSON = 256 * 1024
MAX_ARCHIVE = 512 * 1024
MAX_XML = 2 * 1024 * 1024
MAX_RUN_PAGE = 4 * 1024 * 1024
PENDING_RUN_STATES = ("queued", "in_progress", "waiting", "pending", "requested")
TERMINAL_CONCLUSIONS = ("success", "failure", "cancelled", "skipped", "timed_out",
                        "action_required", "neutral", "stale", "startup_failure")
DEPLOY_STEPS = {"staging-web": "Deploy to staging", "staging-api": "Deploy api to staging"}


class Refused(Exception):
    pass


def require(condition, reason):
    if not condition:
        raise Refused(reason)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def decimal(value):
    require(type(value) in (int, str) and re.fullmatch(r"[1-9][0-9]{0,15}", str(value)),
            "invalid run, attempt or repository identity")
    return str(value)


def timestamp(value):
    require(isinstance(value, str), "missing evidence time")
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
        require(result.tzinfo is not None, "evidence time lacks timezone")
        return result
    except ValueError:
        raise Refused("invalid evidence time") from None


def parse_json(data, limit=MAX_JSON):
    require(len(data) <= limit, "oversized JSON")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON field")
            result[key] = value
        return result
    try:
        result = json.loads(data, object_pairs_hook=unique)
        require(isinstance(result, dict), "invalid JSON object")
        return result
    except (ValueError, UnicodeError):
        raise Refused("malformed JSON") from None


def approved_contract(policy, contract_bytes):
    require(policy.get("schema") == 1 and policy.get("evaluator") == "junit-map-v1",
            "unsupported trusted policy")
    approvals = policy.get("approvals")
    require(isinstance(approvals, list) and len(approvals) == 1,
            "approved product contract unavailable or ambiguous")
    approval = approvals[0]
    require(approval.get("sha256") == digest(contract_bytes), "unapproved product contract hash")
    for key in ("spec_revision", "product_approval", "qa_approval"):
        require(isinstance(approval.get(key), str) and LABEL.fullmatch(approval[key]),
                "missing authenticated approval/spec reference in trusted policy")
    contract = parse_json(contract_bytes)
    require(contract.get("schema") == 1 and contract.get("evaluator") == policy["evaluator"]
            and contract.get("spec_revision") == approval["spec_revision"], "contract policy mismatch")
    criteria = contract.get("criteria")
    require(isinstance(criteria, list) and 0 < len(criteria) <= 200, "empty or invalid applicability")
    seen = set()
    selectors = set()
    applicable = 0
    for criterion in criteria:
        key = criterion.get("id")
        require(isinstance(key, str) and LABEL.fullmatch(key) and key not in seen,
                "invalid or duplicate criterion")
        seen.add(key)
        require(type(criterion.get("applicable")) is bool, "unknown applicability")
        if not criterion["applicable"]:
            require(isinstance(criterion.get("rationale"), str) and criterion["rationale"].strip(),
                    "exclusion lacks approved rationale")
            continue
        applicable += 1
        evidence = criterion.get("evidence")
        require(isinstance(evidence, list) and 0 < len(evidence) <= 100,
                "applicable criterion has no executable evidence")
        for item in evidence:
            require(set(item) == {"classname", "name", "outcome"} and item["outcome"] == "passed"
                    and all(isinstance(item[k], str) and 0 < len(item[k]) <= 500
                            for k in ("classname", "name")), "unsupported evidence mapping")
            selector = (item["classname"], item["name"])
            require(selector not in selectors, "ambiguous evidence mapping")
            selectors.add(selector)
    require(applicable > 0, "no applicable product criteria")
    return contract


def junit_evidence(xml, contract):
    require(0 < len(xml) <= MAX_XML and b"<!DOCTYPE" not in xml and b"<!ENTITY" not in xml,
            "missing or unsafe JUnit evidence")
    try:
        root = ET.fromstring(xml)
    except ET.ParseError:
        raise Refused("malformed JUnit evidence") from None
    cases = list(root.iter("testcase"))
    require(0 < len(cases) <= 10000, "empty or oversized journey")
    rows = {}
    identities = {}
    for case in cases:
        selector = (case.get("classname"), case.get("name"))
        expected_failure = any(p.get("name") == "fail" for p in case.findall("properties/property"))
        failure = any(c.tag in ("failure", "error", "flakyFailure", "flakyError", "rerunFailure", "rerunError")
                      for c in case)
        outcome = "skipped" if case.find("skipped") is not None else (
            "failed" if failure or expected_failure else "passed")
        rows.setdefault(selector, []).append(outcome)
        for prop in case.findall("properties/property"):
            if prop.get("name") == "candidate-identity":
                require(outcome == "passed", "identity test did not pass")
                value = parse_json(prop.get("value", "").encode())
                phase = value.get("phase")
                require(phase in ("build-identity", "build-identity-final") and phase not in identities,
                        "missing or ambiguous live identities")
                require(all(isinstance(value.get(k), str) and SHA.fullmatch(value[k])
                            for k in ("web", "api", "expected")), "malformed live identities")
                timestamp(value.get("observed_at"))
                identities[phase] = {k: value[k] for k in
                                     ("phase", "web", "api", "expected", "observed_at")}
    coverage = []
    for criterion in contract["criteria"]:
        if not criterion["applicable"]:
            continue
        outcomes = [rows.get((e["classname"], e["name"]), []) for e in criterion["evidence"]]
        coverage.append({"id": criterion["id"], "outcome": "passed" if
                         all(value == ["passed"] for value in outcomes) else "incomplete"})
    return coverage, identities


def validate_receipt(receipt, *, repo, sha, run_id, attempt, workflow_sha,
                     manifest_hash, policy, contract_bytes):
    contract = approved_contract(policy, contract_bytes)
    require(receipt.get("schema") == 1 and receipt.get("repository") == repo
            and receipt.get("environment") == "staging" and receipt.get("workflow") == WORKFLOW
            and receipt.get("event") == "push" and receipt.get("ref") == "refs/heads/main"
            and receipt.get("sha") == sha and receipt.get("workflow_sha") == workflow_sha
            and receipt.get("run_id") == run_id and receipt.get("run_attempt") == attempt,
            "receipt source, producer or attempt mismatch")
    require(receipt.get("migration_manifest_sha256") == manifest_hash,
            "candidate migration manifest mismatch")
    require(receipt.get("contract_sha256") == digest(contract_bytes)
            and receipt.get("policy_sha256") == digest(json.dumps(policy, sort_keys=True).encode())
            and receipt.get("evaluator") == policy["evaluator"], "receipt approval/evaluator mismatch")
    require(receipt.get("api") == {"job": "staging-api", "deployed": True,
                                  "outcome": "deployed", "source_sha": sha},
            "candidate API not actually deployed from the same source")
    require(receipt.get("legacy_gate") == "success", "legacy journey gate did not pass")
    expected = [{"id": c["id"], "outcome": "passed"} for c in contract["criteria"] if c["applicable"]]
    require(receipt.get("coverage") == expected, "applicable product evidence incomplete, skipped or failed")
    require(receipt.get("result") == "satisfied" and receipt.get("reasons") == [],
            "receipt is nonqualifying")
    require(isinstance(receipt.get("junit_sha256"), str) and HASH.fullmatch(receipt["junit_sha256"]),
            "missing evidence content digest")
    identities = receipt.get("identities", {})
    times = []
    for phase in ("build-identity", "build-identity-final"):
        item = identities.get(phase, {})
        require(item.get("phase") == phase and item.get("web") == sha and item.get("api") == sha
                and item.get("expected") == sha, "opening/closing live identities disagree")
        times.append(timestamp(item.get("observed_at")))
    require(times[0] < times[1] <= timestamp(receipt.get("recorded_at")), "incoherent evidence ordering")
    return receipt


def git(*args):
    try:
        return subprocess.check_output(["git", "-C", str(ROOT), *args], stderr=subprocess.PIPE,
                                       timeout=15)
    except (OSError, subprocess.SubprocessError):
        raise Refused("trusted source object unavailable or not on main") from None


def git_file(sha, name):
    return git("show", f"{sha}:{name}")


def current_main(trusted):
    # Checkout may reset origin/main to the dispatch SHA after fetching history.
    # Resolve the remote independently, then pin objects without changing any ref.
    def resolve():
        result = git("ls-remote", "--exit-code", "origin", "refs/heads/main")
        match = re.fullmatch(rb"([0-9a-f]{40})\trefs/heads/main\n", result)
        require(match is not None, "missing or incoherent current main resolution")
        return match[1].decode("ascii")
    tip = resolve()
    git("fetch", "--no-tags", "--no-write-fetch-head", "--refmap=", "origin", tip)
    require(git("rev-parse", "--verify", f"{tip}^{{commit}}").decode().strip() == tip,
            "current main is not the resolved commit")
    require(resolve() == tip, "current main moved during source resolution; retry required")
    git("merge-base", "--is-ancestor", trusted, tip)
    return tip


def produce(env, xml, policy, contract_bytes, manifest_bytes):
    sha = env["GITHUB_SHA"]
    require(SHA.fullmatch(sha) and SHA.fullmatch(env["GITHUB_WORKFLOW_SHA"])
            and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", env["GITHUB_REPOSITORY"]),
            "invalid candidate source identity")
    outcome = env.get("API_DEPLOY_OUTCOME", "")
    require(outcome in ("", "deployed", "skipped", "unconfigured", "failed", "cancelled"),
            "invalid API outcome label")
    require(env.get("API_SOURCE_SHA", "") == "" or SHA.fullmatch(env["API_SOURCE_SHA"]),
            "invalid API source identity")
    require(env.get("JOURNEY_GATE", "") in ("", "success", "failure", "skipped", "cancelled"),
            "invalid legacy gate label")
    receipt = {
        "schema": 1, "repository": env["GITHUB_REPOSITORY"], "environment": "staging",
        "workflow": WORKFLOW, "workflow_sha": env["GITHUB_WORKFLOW_SHA"],
        "event": env["GITHUB_EVENT_NAME"], "ref": env["GITHUB_REF"], "sha": sha,
        "run_id": decimal(env["GITHUB_RUN_ID"]), "run_attempt": decimal(env["GITHUB_RUN_ATTEMPT"]),
        "api": {"job": "staging-api", "deployed": env.get("API_DEPLOYED") == "true",
                "outcome": env.get("API_DEPLOY_OUTCOME", ""), "source_sha": env.get("API_SOURCE_SHA", "")},
        "migration_manifest_sha256": digest(manifest_bytes), "contract_sha256": digest(contract_bytes),
        "policy_sha256": digest(json.dumps(policy, sort_keys=True).encode()),
        "evaluator": "junit-map-v1", "legacy_gate": env.get("JOURNEY_GATE", ""),
        "junit_sha256": digest(xml), "coverage": [], "identities": {},
        "recorded_at": datetime.now(timezone.utc).isoformat(), "result": "incomplete", "reasons": [],
    }
    try:
        contract = approved_contract(policy, contract_bytes)
        receipt["coverage"], receipt["identities"] = junit_evidence(xml, contract)
        receipt["result"] = "satisfied"
        validate_receipt(receipt, repo=receipt["repository"], sha=sha, run_id=receipt["run_id"],
                         attempt=receipt["run_attempt"], workflow_sha=receipt["workflow_sha"],
                         manifest_hash=digest(manifest_bytes), policy=policy, contract_bytes=contract_bytes)
    except Refused as error:
        receipt["result"] = "incomplete"
        receipt["reasons"] = [str(error)]
    return receipt


def artifact_name(sha, run_id, attempt):
    return f"staging-admission-{sha}-{run_id}-{attempt}"


def read_archive(archive):
    require(0 < len(archive) <= MAX_ARCHIVE, "oversized or absent receipt archive")
    try:
        with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
            infos = bundle.infolist()
            require(len(infos) == 1 and infos[0].filename == "staging-admission.json"
                    and 0 < infos[0].file_size <= MAX_JSON and not infos[0].is_dir()
                    and not infos[0].flag_bits & 1, "invalid receipt archive member")
            return parse_json(bundle.read(infos[0]))
    except (zipfile.BadZipFile, RuntimeError, OSError):
        raise Refused("unreadable receipt archive") from None


class ArtifactRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        require(urllib.parse.urlsplit(newurl).scheme == "https", "insecure artifact redirect")
        redirected = super().redirect_request(request, fp, code, msg, headers, newurl)
        # GitHub redirects archive downloads to signed storage URLs. Never send
        # the repository token to the storage origin (or an arbitrary redirect).
        if urllib.parse.urlsplit(request.full_url).netloc != urllib.parse.urlsplit(newurl).netloc:
            redirected.remove_header("Authorization")
        return redirected


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        raise Refused("staging identity redirect refused")


def current_identity(env, sha):
    # Same two public read-only identity endpoints as the journey helper. No
    # redirects: the Access pair must never cross to a login/foreign origin.
    raw = env.get("STAGING_BASE_URL", "")
    url = urllib.parse.urlsplit(raw)
    require(url.scheme == "https" and url.hostname and not url.username and not url.password
            and not url.query and not url.fragment and url.path in ("", "/"),
            "missing or invalid staging identity origin")
    require(env.get("CF_ACCESS_CLIENT_ID") and env.get("CF_ACCESS_CLIENT_SECRET"),
            "missing staging identity Access pair")
    opener = urllib.request.build_opener(NoRedirect())
    for path, field in (("/api/build", "sha"), ("/healthz", "build_sha")):
        request = urllib.request.Request(raw.rstrip("/") + path, headers={
            "CF-Access-Client-Id": env["CF_ACCESS_CLIENT_ID"],
            "CF-Access-Client-Secret": env["CF_ACCESS_CLIENT_SECRET"],
        })
        try:
            with opener.open(request, timeout=15) as response:
                require(response.status == 200, "staging identity HTTP failure")
                data = parse_json(response.read(MAX_JSON + 1))
            require(isinstance(data, dict) and data.get(field) == sha, "current staging identity drift")
            if field == "sha":
                require(set(data) == {"sha"}, "invalid web identity shape")
        except (urllib.error.URLError, TimeoutError):
            raise Refused("staging identity evidence unavailable") from None


class Actions:
    def __init__(self, repo, token):
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) and token,
                "missing authenticated repository context")
        self.base = f"https://api.github.com/repos/{repo}/actions/"
        self.token = token
        self.deadline = time.monotonic() + 120
        self.opener = urllib.request.build_opener(ArtifactRedirect())

    def read(self, route, limit=MAX_JSON):
        request = urllib.request.Request(self.base + route, headers={
            "Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        })
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, "Actions discovery time budget exhausted")
        try:
            with self.opener.open(request, timeout=min(15, remaining)) as response:
                data = response.read(limit + 1)
            require(len(data) <= limit, "Actions response exceeds bounds")
            return data
        except (urllib.error.URLError, TimeoutError):
            raise Refused("authenticated Actions evidence unavailable") from None

    def api(self, route):
        data = parse_json(self.read(route))
        require(isinstance(data, dict), "invalid Actions object")
        return data

    def listing(self, route, key):
        data = self.api(route + ("&" if "?" in route else "?") + "per_page=100&page=1")
        rows = data.get(key)
        require(isinstance(rows, list) and type(data.get("total_count")) is int
                and 0 <= data["total_count"] <= 100 and len(rows) == data["total_count"],
                f"incomplete or exhausted {key} listing")
        return rows

    def runs(self, route):
        # GitHub search-filtered run listings expose at most 1,000 results.
        # Refuse at that ceiling, on page drift, duplicates or a short page.
        rows, seen, total = [], set(), None
        for number in range(1, 11):
            data = parse_json(self.read(f"{route}&per_page=100&page={number}", MAX_RUN_PAGE), MAX_RUN_PAGE)
            count, batch = data.get("total_count"), data.get("workflow_runs")
            require(type(count) is int and 0 <= count < 1000 and isinstance(batch, list)
                    and (total is None or total == count)
                    and len(batch) == min(100, count - len(rows)),
                    "incomplete or exhausted workflow_runs listing")
            total = count
            for run in batch:
                rid = decimal(run.get("id"))
                require(rid not in seen, "incomplete or exhausted workflow_runs listing")
                seen.add(rid)
            rows.extend(batch)
            if len(rows) == total:
                return rows
        raise Refused("incomplete or exhausted workflow_runs listing")

    def conflicts(self, workflow_id, opening):
        # A creation-only recent window would miss old reruns/queued deploys.
        # GitHub permits reruns within 30 days of the INITIAL run, then bounds
        # execution (including waiting/approval) to 35 days. Include 66 days
        # before the receipt opening, rounded down, plus ALL unresolved runs
        # independently of creation time. See staging-admission.md for sources.
        cutoff = (opening - timedelta(days=66)).astimezone(timezone.utc).date().isoformat()
        base = f"workflows/{workflow_id}/runs?branch=main&event=push"
        queries = [f"{base}&created=%3E%3D{cutoff}"]
        queries.extend(f"{base}&status={status}" for status in PENDING_RUN_STATES)
        found = {}
        for query in queries:
            for run in self.runs(query):
                rid = decimal(run.get("id"))
                require(rid not in found or found[rid] == run,
                        "staging run changed during conflict discovery")
                found[rid] = run
        return list(found.values())


def unchanged_by_job(job, opening, source, producer_source_ok):
    reason = "known intervening staging deployment; fresh journey required"
    require(job.get("status") == "completed" and job.get("conclusion") in TERMINAL_CONCLUSIONS, reason)
    if job["conclusion"] == "skipped":
        return
    if timestamp(job.get("completed_at")) <= opening:
        return
    # A failed install/build/schema prerequisite is not a mutation, but only
    # the reviewed producer plus an explicit skipped deploy step proves that.
    steps = job.get("steps", [])
    deploy = [step for step in steps if step.get("name") == DEPLOY_STEPS[job["name"]]]
    require(job["conclusion"] == "failure" and len(deploy) == 1
            and deploy[0].get("status") == "completed" and deploy[0].get("conclusion") == "skipped"
            and type(deploy[0].get("number")) is int, reason)
    require(any(step.get("status") == "completed" and step.get("conclusion") == "failure"
                and type(step.get("number")) is int and 0 < step["number"] < deploy[0]["number"]
                for step in steps), reason)
    producer_source_ok(source)


def verify(actions, *, repo, repository_id, sha, policy, contract_bytes, manifest_hash,
           producer_source_ok):
    # Approval absence is a refusal even when every legacy job is green.
    approved_contract(policy, contract_bytes)
    workflow = actions.api("workflows/deploy.yml")
    require(workflow.get("path") == WORKFLOW and workflow.get("state") == "active",
            "wrong staging workflow")
    workflow_id = decimal(workflow.get("id"))
    runs = actions.runs(f"workflows/{workflow_id}/runs?branch=main&event=push&head_sha={sha}")
    same = [r for r in runs if r.get("head_sha") == sha and r.get("event") == "push"]
    require(same, "no candidate main-push staging run")
    selected = max(same, key=lambda r: int(decimal(r.get("id"))))
    run_id = decimal(selected.get("id"))
    attempt = decimal(selected.get("run_attempt"))

    def run_identity(run):
        require(decimal(run.get("id")) == run_id and decimal(run.get("run_attempt")) == attempt
                and decimal(run.get("workflow_id")) == workflow_id and run.get("path") == WORKFLOW
                and run.get("head_sha") == sha and run.get("head_branch") == "main"
                and run.get("event") == "push" and run.get("status") == "completed"
                and run.get("conclusion") == "success", "wrong, superseded or unsuccessful staging attempt")
        for key in ("repository", "head_repository"):
            require(run.get(key, {}).get("full_name") == repo
                    and decimal(run.get(key, {}).get("id")) == repository_id, "foreign repository producer")
    run = actions.api(f"runs/{run_id}")
    run_identity(run)
    producer_source_ok(run["head_sha"])
    jobs = actions.listing(f"runs/{run_id}/attempts/{attempt}/jobs", "jobs")
    selected_jobs = {}
    for name in ("staging-web", "staging-api", JOURNEY):
        matches = [j for j in jobs if j.get("name") == name]
        require(len(matches) == 1, "missing or ambiguous required staging job")
        job = matches[0]
        require(decimal(job.get("run_id")) == run_id and decimal(job.get("run_attempt")) == attempt
                and job.get("head_sha") == sha and job.get("status") == "completed"
                and job.get("conclusion") == "success", "required job did not pass in selected attempt")
        selected_jobs[name] = job
    journey = selected_jobs[JOURNEY]
    previous = 0
    for name in ("Run full journey", "Enforce journey evidence gate (all-skipped must fail)", RECORD, UPLOAD):
        steps = [s for s in journey.get("steps", []) if s.get("name") == name]
        require(len(steps) == 1 and steps[0].get("status") == "completed"
                and steps[0].get("conclusion") == "success" and type(steps[0].get("number")) is int
                and steps[0]["number"] > previous, "journey, evaluator and receipt steps must succeed in order")
        previous = steps[0]["number"]
    artifacts = actions.listing(f"runs/{run_id}/artifacts", "artifacts")
    name = artifact_name(sha, run_id, attempt)
    matches = [a for a in artifacts if a.get("name") == name]
    require(len(matches) == 1, "missing or ambiguous attempt-bound receipt")
    artifact = matches[0]
    producer = artifact.get("workflow_run", {})
    require(artifact.get("expired") is False and type(artifact.get("size_in_bytes")) is int
            and 0 < artifact["size_in_bytes"] <= MAX_ARCHIVE
            and re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest", ""))
            and decimal(producer.get("id")) == run_id
            and decimal(producer.get("repository_id")) == repository_id
            and decimal(producer.get("head_repository_id")) == repository_id
            and producer.get("head_branch") == "main" and producer.get("head_sha") == sha,
            "invalid artifact provenance")
    require(timestamp(artifact.get("expires_at")) > datetime.now(timezone.utc), "expired receipt")
    archive = actions.read(f"artifacts/{decimal(artifact.get('id'))}/zip", MAX_ARCHIVE)
    require("sha256:" + digest(archive) == artifact["digest"], "artifact content digest mismatch")
    receipt = validate_receipt(read_archive(archive), repo=repo, sha=sha, run_id=run_id, attempt=attempt,
                               workflow_sha=sha, manifest_hash=manifest_hash,
                               policy=policy, contract_bytes=contract_bytes)
    opening = timestamp(receipt["identities"]["build-identity"]["observed_at"])
    closing = timestamp(receipt["identities"]["build-identity-final"]["observed_at"])
    require(timestamp(journey.get("started_at")) <= opening < closing
            <= timestamp(receipt["recorded_at"]) <= timestamp(journey.get("completed_at")),
            "journey timing mismatch")
    for name in ("staging-web", "staging-api"):
        require(timestamp(selected_jobs[name].get("completed_at")) <= opening,
                "journey preceded candidate deployment")
    # Per-lane locks are not a reservation. Discover recent/rerunnable runs
    # and every unresolved run, not the workflow's entire lifetime history.
    for other in actions.conflicts(workflow_id, opening):
        if other.get("event") != "push" or other.get("head_sha") == sha:
            continue
        require(other.get("status") in (*PENDING_RUN_STATES, "completed"),
                "unclassified conflicting staging run")
        if other["status"] == "completed":
            require(other.get("conclusion") in TERMINAL_CONCLUSIONS,
                    "unclassified conflicting staging run conclusion")
            # Completed run metadata is updated after its jobs. A completed
            # pre-opening run cannot conceal a later job in that snapshot.
            if timestamp(other.get("updated_at")) <= opening:
                continue
        other_id = decimal(other.get("id"))
        # Include ALL attempts: a failed latest rerun must not hide a mutation
        # from an earlier attempt after the candidate journey started.
        other_jobs = actions.listing(f"runs/{other_id}/jobs?filter=all", "jobs")
        require(all(any(job.get("name") == lane for job in other_jobs) for lane in DEPLOY_STEPS),
                "missing conflicting staging jobs; fresh journey required")
        if other["status"] != "completed":
            other_attempt = int(decimal(other.get("run_attempt")))
            require(all(any(job.get("name") == lane and job.get("run_attempt") == other_attempt
                            for job in other_jobs) for lane in DEPLOY_STEPS),
                    "missing current-attempt conflicting staging jobs; fresh journey required")
        for job in other_jobs:
            if job.get("name") in DEPLOY_STEPS:
                unchanged_by_job(job, opening, other.get("head_sha"), producer_source_ok)
    # Re-resolve after download and drift discovery: a rerun cannot mix attempts.
    run_identity(actions.api(f"runs/{run_id}"))
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("produce", "verify"))
    args = parser.parse_args()
    env = os.environ
    policy = parse_json((ROOT / POLICY).read_bytes())
    contract_bytes = (ROOT / CONTRACT).read_bytes() if (ROOT / CONTRACT).is_file() else b""
    if args.mode == "produce":
        xml_path = ROOT / "e2e/test-results/junit.xml"
        xml = xml_path.read_bytes() if xml_path.is_file() else b""
        require(len(xml) <= MAX_XML, "oversized JUnit")
        receipt = produce(env, xml, policy, contract_bytes, (ROOT / "migrations/manifest.toml").read_bytes())
        (ROOT / "staging-admission.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print("candidate admission receipt: " + receipt["result"])
        return
    sha = env.get("DEPLOY_SHA", "")
    require(SHA.fullmatch(sha), "invalid selected candidate")
    require(env.get("GITHUB_REF") == "refs/heads/main"
            and env.get("GITHUB_EVENT_NAME") == "workflow_dispatch", "verifier must run from trusted main")
    trusted = env.get("GITHUB_SHA", "")
    require(SHA.fullmatch(trusted) and git("rev-parse", "HEAD").decode().strip() == trusted,
            "verifier checkout is not workflow source")
    main_tip = current_main(trusted)
    git("merge-base", "--is-ancestor", sha, main_tip)
    def source_binding(source):
        records = git("ls-tree", "-z", source, "--", *TRUSTED_TREES, *TRUSTED_FILES)
        entries = dict(row.split(b"\t", 1)[::-1] for row in records.split(b"\0") if row)
        require(set(entries) == {path.encode() for path in (*TRUSTED_TREES, *TRUSTED_FILES)}
                and all(entries[path.encode()].startswith(b"040000 tree ") for path in TRUSTED_TREES)
                and all(entries[path.encode()].startswith(b"100644 blob ")
                        or entries[path.encode()].startswith(b"100755 blob ") for path in TRUSTED_FILES),
                "missing or unsupported producer dependency source")
        return records
    def producer_source_ok(source):
        require(source_binding(source) == source_binding(trusted) == source_binding(main_tip),
                "relevant producer or trusted policy drift; new evidence required")
    manifest = git_file(sha, "migrations/manifest.toml")
    require(manifest == git_file(main_tip, "migrations/manifest.toml"), "current migration manifest drift")
    verify(Actions(env["GITHUB_REPOSITORY"], env["GITHUB_TOKEN"]), repo=env["GITHUB_REPOSITORY"],
           repository_id=decimal(env["GITHUB_REPOSITORY_ID"]), sha=sha, policy=policy,
           contract_bytes=contract_bytes, manifest_hash=digest(manifest), producer_source_ok=producer_source_ok)
    current_identity(env, sha)
    print("candidate deployment and applicable journey receipt verified (not operational acceptance)")


if __name__ == "__main__":
    try:
        main()
    except (Refused, OSError, KeyError, TypeError, ValueError):
        # Do not echo arbitrary API, XML, URLs or parser content into logs.
        print("::error::Candidate admission refused: missing, unapproved, stale or incoherent evidence", file=sys.stderr)
        sys.exit(1)
