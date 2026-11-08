#!/usr/bin/env python3
"""Report trusted completed gates using the first-party keyed incident pattern.

Only API job/step conclusions are public evidence here: never logs, artifacts,
secrets, triggering-run source, or exception response bodies. Stdlib only.
"""

import argparse
from datetime import datetime, timedelta, timezone
import json
import os
import re
import sys
import urllib.error
import urllib.request

REPO = "TogetherWeOwn/nntunev2"
WORKFLOWS = {"deploy.yml": "push", "e2e-staging.yml": None,
             "deploy-production.yml": "workflow_dispatch"}
BAD = {"failure", "cancelled", "timed_out", "action_required", "startup_failure"}
# The successful evidence step prevents skipped/unconfigured jobs from closing
# an incident. Keys are job names from the reviewed default-branch workflows.
GATES = {
    "deploy.yml": {
        "gate": None,
        "staging-web": "Deploy to staging",
        "staging-api": "Deploy api to staging",
        "staging-mb": "Deploy mb to staging",
        "staging-ingest": "Deploy ingest to staging",
        "post-deploy-e2e / synthetic smoke (health)": "Run synthetic smoke",
        "post-deploy-e2e / full journey (signup → library → meter → limits)":
            "Enforce journey evidence gate (all-skipped must fail)",
    },
    "e2e-staging.yml": {"synthetic smoke (health)": "Run synthetic smoke"},
    "deploy-production.yml": {
        "production-preflight": "Resolve and validate the promotion target",
        "staging-evidence": "Verify candidate deployment and accepted journey receipt",
        "deploy-production": "Verify production serves the promoted build",
        "release / tag-release": "Tag the deployed commit and publish its release",
    },
}


class ReportError(Exception):
    """Fixed-vocabulary errors only; never expose remote response bodies."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


class API:
    def __init__(self, token):
        self.token = token

    def request(self, method, path, body=None):
        req = urllib.request.Request(
            "https://api.github.com/repos/" + REPO + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": "Bearer " + self.token,
                     "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28",
                     "Content-Type": "application/json",
                     "User-Agent": "nntune-incident-report/1"})
        try:
            with urllib.request.build_opener(NoRedirect()).open(req, timeout=15) as res:
                raw = res.read(2 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as exc:
            raise ReportError(f"GitHub {method} refused (HTTP {exc.code})") from None
        except (OSError, ValueError):
            raise ReportError("GitHub transport failed") from None
        if len(raw) > 2 * 1024 * 1024:
            raise ReportError("GitHub response exceeded bound")
        try:
            return json.loads(raw) if raw else None
        except (ValueError, RecursionError):
            raise ReportError("GitHub response malformed") from None

    def pages(self, path, key=None):
        result = []
        for page in range(1, 11):
            data = self.request("GET", path + ("&" if "?" in path else "?") +
                                f"per_page=100&page={page}")
            rows = data[key] if key else data
            if not isinstance(rows, list) or len(rows) > 100:
                raise ReportError("GitHub list malformed")
            result.extend(rows)
            if len(rows) < 100:
                return result
        raise ReportError("GitHub pagination exceeded bound")


def trusted(run):
    workflow = run.get("path", "").removeprefix(".github/workflows/")
    return (workflow in WORKFLOWS and run.get("head_branch") == "main"
            and run.get("head_repository", {}).get("full_name") == REPO
            and run.get("status") == "completed"
            and run.get("event") in {"push", "schedule", "workflow_dispatch"}
            and (WORKFLOWS[workflow] is None or run["event"] == WORKFLOWS[workflow])
            and re.fullmatch(r"[0-9a-f]{40}", run.get("head_sha", "")) is not None)


def gate_result(jobs, name, evidence):
    selected = [j for j in jobs if j.get("name") == name]
    if len(selected) != 1:
        return None
    job = selected[0]
    recovered = any(s.get("name") == evidence and s.get("conclusion") == "success"
                    for s in job.get("steps", [])) if evidence is not None else any(
                        j.get("name") == "staging-web" and j.get("conclusion") == "success"
                        and any(s.get("name") == "Deploy to staging"
                                and s.get("conclusion") == "success" for s in j.get("steps", []))
                        for j in jobs)
    return job if (job.get("conclusion") in BAD or
                   (job.get("conclusion") == "success" and recovered)) else None


def source(issue):
    match = re.search(r"<!-- incident-source: ([1-9][0-9]*):([1-9][0-9]*) -->",
                      issue.get("body") or "")
    return tuple(map(int, match.groups())) if match else None


def ensure_comment(api, number, key, occurrence, kind, body, dry_run):
    marker = f"<!-- incident-occurrence: {key} {occurrence[0]}:{occurrence[1]} {kind} -->"
    comments = api.pages(f"/issues/{number}/comments")
    if any(marker in (comment.get("body") or "") for comment in comments):
        return False
    if not dry_run:
        api.request("POST", f"/issues/{number}/comments", {"body": body + "\n" + marker})
    return True


def recent(api, dry_run=False):
    # Overlap scans for delayed schedules; source markers make replay idempotent.
    # Read the whole bounded snapshot before writes, then process in source order
    # so even a transient failure followed by recovery leaves an incident trail.
    cutoff = datetime.now(timezone.utc) - timedelta(hours=1)
    runs, history = [], {}
    for workflow in WORKFLOWS:
        rows = api.request("GET", f"/actions/workflows/{workflow}/runs?branch=main&per_page=100")["workflow_runs"]
        if not isinstance(rows, list) or len(rows) > 100:
            raise ReportError("Gate history malformed")
        if len(rows) == 100 and datetime.strptime(
                rows[-1]["created_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc) >= cutoff:
            raise ReportError("Gate history window saturated")
        history[workflow] = [r for r in rows if trusted(r)]
        for run in history[workflow]:
            if datetime.strptime(
                    run["updated_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc) >= cutoff:
                runs.append(run)
    if len(runs) > 10:
        raise ReportError("Recent gate history exceeds bound")
    candidates = {(r["id"], r["run_attempt"]) for r in runs}
    outside = []
    for workflow, rows in history.items():
        selected = [(r["id"], r["run_attempt"]) for r in runs
                    if r["path"].rsplit("/", 1)[-1] == workflow]
        outside.extend(r for r in rows if selected
                       and (r["id"], r["run_attempt"]) > min(selected)
                       and (r["id"], r["run_attempt"]) not in candidates)
    if len(outside) > 10:
        raise ReportError("Gate history replay exceeds bound")
    # Healthy gates need not have an issue. Consult newer out-of-window sources
    # before any writes, but keep ordered transitions among this scan's candidates.
    evidence = {workflow: {} for workflow in WORKFLOWS}
    for run in outside:
        workflow = run["path"].rsplit("/", 1)[-1]
        evidence[workflow][(run["id"], run["run_attempt"])] = api.pages(
            f"/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs")
    actions = []
    for run in sorted(runs, key=lambda r: (r["id"], r["run_attempt"])):
        workflow = run["path"].rsplit("/", 1)[-1]
        actions.extend(report(api, run["id"], dry_run, newer_evidence=evidence[workflow]))
    return actions


def report(api, run_id, dry_run=False, newer_evidence=None):
    run = api.request("GET", f"/actions/runs/{run_id}")
    if not trusted(run):
        raise ReportError("Untrusted or incomplete gate run")
    workflow = run["path"].rsplit("/", 1)[-1]
    attempt = run["run_attempt"]
    # A delayed green/re-run must not close a newer failure. Serialize writers
    # in the workflow, then ignore superseded runs, including newer attempts.
    # Runs are newest-created first: a newer run, if any, is in this page.
    # Do not walk all history (a busy repository can exceed the search bound).
    if newer_evidence is None:
        latest = api.request(
            "GET", f"/actions/workflows/{workflow}/runs?branch=main&per_page=100")["workflow_runs"]
        newer = [r for r in latest if trusted(r) and (r["id"], r["run_attempt"]) > (run_id, attempt)]
        if len(newer) > 10:
            raise ReportError("Gate history replay exceeds bound")
        newer_evidence = {(r["id"], r["run_attempt"]): api.pages(
            f"/actions/runs/{r['id']}/attempts/{r['run_attempt']}/jobs", "jobs") for r in newer}
    current = api.request("GET", f"/actions/runs/{run_id}")
    if current["run_attempt"] != attempt or current["status"] != "completed":
        return ["superseded"]
    jobs = api.pages(f"/actions/runs/{run_id}/attempts/{attempt}/jobs", "jobs")
    newer_jobs = [rows for occurrence, rows in newer_evidence.items()
                  if occurrence > (run_id, attempt)]
    # Retain source ordering even after recovery, so a delayed older failure
    # cannot reopen the same episode. Reuse a closed keyed issue on recurrence.
    issues = api.pages("/issues?state=all&labels=incident")
    actions = []
    for name, evidence in GATES[workflow].items():
        job = gate_result(jobs, name, evidence)
        if job is None:
            continue
        if any(gate_result(rows, name, evidence) is not None for rows in newer_jobs):
            actions.append("superseded")
            continue
        conclusion = job.get("conclusion")
        failed = [str(s["number"]) for s in job.get("steps", []) if s.get("conclusion") in BAD]
        # Stable workflow/job key, independent of mapping order. Names are our
        # constants, not user-controlled API strings. Never rename a live key.
        key = workflow + ":" + re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
        marker = f"<!-- incident-key: {key} -->"
        matches = sorted([i for i in issues if "pull_request" not in i
                          and marker in (i.get("body") or "")], key=lambda i: i["number"])
        url = f"https://github.com/{REPO}/actions/runs/{run_id}/attempts/{attempt}"
        body = (f"Run: {url}\n\nWorkflow source SHA: `{run['head_sha']}`\n\n"
                f"Gate: {name}\n\nThreshold: job and required evidence step success; "
                f"measured job conclusion: {conclusion}; failing step numbers: "
                f"{', '.join(failed) or 'none reported'}.\n\n"
                "Numerical test measurements are not available in the jobs API; "
                "inspect the linked gate summary for its skip/count budget. "
                "No logs or artifacts are copied into this issue.\n")
        if workflow == "deploy-production.yml":
            body += ("\nThis is the workflow source SHA, not necessarily the selected "
                     "promotion target. The dispatch input is not exposed by the runs API; "
                     "consult the linked preflight. No production change is authorized here.\n")
        occurrence = (run_id, attempt)
        if any(source(i) is not None and source(i) > occurrence for i in matches):
            actions.append("superseded")
            continue
        recorded_matches = [i for i in matches if source(i) == occurrence]
        if recorded_matches:
            issue = recorded_matches[0]
            if conclusion in BAD and "<!-- incident-notification: failure -->" in issue["body"]:
                delivered = ensure_comment(api, issue["number"], key, occurrence, "failure",
                                           "Still failing.\n\n" + body, dry_run)
                actions.append(f"commented #{issue['number']}: {name}" if delivered else "superseded")
            else:
                actions.append("superseded")
            continue
        recorded = body + f"\n{marker}\n<!-- incident-source: {run_id}:{attempt} -->\n"
        if conclusion in BAD:
            if matches:
                issue = matches[0]
                actions.append(f"commented #{issue['number']}: {name}")
                if not dry_run:
                    api.request("PATCH", f"/issues/{issue['number']}",
                                {"body": recorded + "<!-- incident-notification: failure -->\n",
                                 "state": "open"})
                ensure_comment(api, issue["number"], key, occurrence, "failure",
                               "Still failing.\n\n" + body, dry_run)
            else:
                actions.append(f"opened: {name}")
                if not dry_run:
                    created = api.request("POST", "/issues", {
                        "title": f"incident: {workflow}: {name}",
                        "body": recorded, "labels": ["incident"]})
                    if not isinstance(created, dict) or type(created.get("number")) is not int:
                        raise ReportError("Issue creation returned no number")
        else:
            for issue in matches:
                if issue.get("state") == "closed":
                    continue
                if source(issue) is None:
                    actions.append(f"held #{issue['number']}: unknown source")
                    continue
                actions.append(f"recovered #{issue['number']}: {name}")
                ensure_comment(api, issue["number"], key, occurrence, "recovery",
                               "Recovered.\n\n" + body, dry_run)
                if not dry_run:
                    api.request("PATCH", f"/issues/{issue['number']}",
                                {"body": recorded, "state": "closed", "state_reason": "completed"})
    return actions


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--run-id", type=int)
    mode.add_argument("--recent", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if (args.run_id is not None and args.run_id <= 0) or os.environ.get("GITHUB_REPOSITORY") != REPO:
        raise ReportError("Invalid incident reporting context")
    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        raise ReportError("Missing GitHub token")
    api = API(token)
    actions = recent(api, args.dry_run) if args.recent else report(api, args.run_id, args.dry_run)
    for action in actions:
        print(("DRY RUN: " if args.dry_run else "") + action)


if __name__ == "__main__":
    try:
        main()
    except ReportError as error:
        print(f"incident reporting failed: {error}", file=sys.stderr)
        sys.exit(1)
