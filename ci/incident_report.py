#!/usr/bin/env python3
"""Report trusted completed gates using the first-party keyed incident pattern.

Only API job/step conclusions are public evidence here: never logs, artifacts,
secrets, triggering-run source, or exception response bodies. Stdlib only.
"""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime

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


def timestamp(value):
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return result if result.tzinfo else None
    except (AttributeError, TypeError, ValueError):
        return None


def report(api, run_id, dry_run=False):
    run = api.request("GET", f"/actions/runs/{run_id}")
    if not trusted(run):
        raise ReportError("Untrusted or incomplete gate run")
    workflow = run["path"].rsplit("/", 1)[-1]
    attempt = run["run_attempt"]
    # A delayed green/re-run must not close a newer failure. Serialize writers
    # in the workflow, then ignore superseded runs, including newer attempts.
    # Runs are newest-created first: a newer run, if any, is in this page.
    # Do not walk all history (a busy repository can exceed the search bound).
    latest = api.request("GET", f"/actions/workflows/{workflow}/runs?branch=main&per_page=100")["workflow_runs"]
    if any(trusted(r) and (r["id"], r["run_attempt"]) > (run_id, attempt) for r in latest):
        return ["superseded"]
    current = api.request("GET", f"/actions/runs/{run_id}")
    if current["run_attempt"] != attempt or current["status"] != "completed":
        return ["superseded"]
    jobs = api.pages(f"/actions/runs/{run_id}/attempts/{attempt}/jobs", "jobs")
    issues = api.pages("/issues?state=open&labels=incident")
    actions = []
    for name, evidence in GATES[workflow].items():
        selected = [j for j in jobs if j.get("name") == name]
        if len(selected) != 1:
            continue
        job = selected[0]
        conclusion = job.get("conclusion")
        steps = job.get("steps", [])
        failed = [str(s["number"]) for s in steps if s.get("conclusion") in BAD]
        recovered = any(s.get("name") == evidence and s.get("conclusion") == "success"
                        for s in steps) if evidence is not None else any(
                            j.get("name") == "staging-web" and j.get("conclusion") == "success"
                            and any(s.get("name") == "Deploy to staging"
                                    and s.get("conclusion") == "success" for s in j.get("steps", []))
                            for j in jobs)
        if conclusion not in BAD and not (conclusion == "success" and recovered):
            continue
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
        if conclusion in BAD:
            if matches:
                for issue in matches[:1]:
                    actions.append(f"commented #{issue['number']}: {name}")
                    if not dry_run:
                        api.request("POST", f"/issues/{issue['number']}/comments",
                                    {"body": "Still failing.\n\n" + body})
            else:
                actions.append(f"opened: {name}")
                if not dry_run:
                    api.request("POST", "/issues", {"title": f"incident: {workflow}: {name}",
                                "body": body + "\n" + marker, "labels": ["incident"]})
        else:
            started = timestamp(job.get("started_at"))
            for issue in matches:
                updated = timestamp(issue.get("updated_at"))
                if started is None or updated is None or updated > started:
                    actions.append(f"held #{issue['number']}: newer activity")
                    continue
                actions.append(f"recovered #{issue['number']}: {name}")
                if not dry_run:
                    api.request("POST", f"/issues/{issue['number']}/comments",
                                {"body": "Recovered.\n\n" + body})
                    api.request("PATCH", f"/issues/{issue['number']}",
                                {"state": "closed", "state_reason": "completed"})
    return actions


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", type=int, required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.run_id <= 0 or os.environ.get("GITHUB_REPOSITORY") != REPO:
        raise ReportError("Invalid incident reporting context")
    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        raise ReportError("Missing GitHub token")
    for action in report(API(token), args.run_id, args.dry_run):
        print(("DRY RUN: " if args.dry_run else "") + action)


if __name__ == "__main__":
    try:
        main()
    except ReportError as error:
        print(f"incident reporting failed: {error}", file=sys.stderr)
        sys.exit(1)
