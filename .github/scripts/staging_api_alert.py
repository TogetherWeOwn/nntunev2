#!/usr/bin/env python3
"""Keep one open GitHub issue while the `staging-api` deploy job is red on main.

`deploy-alerts-staging.yml` only deploys the Tail consumer, which watches the
*serving* Worker; a failed deploy keeps the old Worker serving, so nothing
alerted when `staging-api` stayed red. `deploy.yml` calls this after the job:

- `failed`: open the tracking issue (or comment on the open one) with the run
  link, so repo watchers are notified on every red main push.
- `recovered`: comment and close the open tracking issue after a deploy.

Only fixed text plus the run URL and SHA from the Actions environment are
written; no step output or log content is copied into the issue.
"""

import json
import os
import sys
import urllib.request

TITLE = "staging-api deploy is red on main"
API = "https://api.github.com"


class AlertError(Exception):
    pass


def github_request(method, path, token, payload=None):
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        API + path,
        data=data,
        method=method,
        headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.loads(response.read() or b"null")


def find_open_issue(repo, request):
    # Exact-title match among open issues (not PRs) opened by this workflow.
    for page in range(1, 6):
        issues = request("GET", f"/repos/{repo}/issues?state=open&per_page=100&page={page}")
        for issue in issues:
            if issue.get("title") == TITLE and "pull_request" not in issue:
                return issue["number"]
        if len(issues) < 100:
            return None
    return None


def run(mode, env, request):
    repo = env.get("GITHUB_REPOSITORY", "")
    sha = env.get("GITHUB_SHA", "")
    run_url = f"{env.get('GITHUB_SERVER_URL', 'https://github.com')}/{repo}/actions/runs/{env.get('GITHUB_RUN_ID', '')}"
    if not repo or not sha:
        raise AlertError("GITHUB_REPOSITORY and GITHUB_SHA are required")
    number = find_open_issue(repo, request)
    if mode == "failed":
        body = (
            f"`staging-api` failed on main at `{sha}`: {run_url}\n\n"
            "Check the job summary first. A red read-only session schema gate "
            "usually means an `app` migration was merged without being applied "
            "to `nntunev2_staging` (docs/ops.md, \"Staging migration apply "
            "declaration\"). This issue closes itself on the next successful "
            "staging-api deploy."
        )
        if number is None:
            created = request("POST", f"/repos/{repo}/issues", {"title": TITLE, "body": body})
            print(f"staging-api-alert: opened issue #{created['number']}")
        else:
            request("POST", f"/repos/{repo}/issues/{number}/comments", {"body": body})
            print(f"staging-api-alert: still red, commented on issue #{number}")
    elif mode == "recovered":
        if number is None:
            return
        request(
            "POST",
            f"/repos/{repo}/issues/{number}/comments",
            {"body": f"`staging-api` deployed `{sha}` successfully: {run_url}. Closing."},
        )
        request("PATCH", f"/repos/{repo}/issues/{number}", {"state": "closed", "state_reason": "completed"})
        print(f"staging-api-alert: recovered, closed issue #{number}")
    else:
        raise AlertError(f"unknown mode {mode!r} (expected failed or recovered)")


def main(argv=None, env=None, request=None):
    argv = sys.argv[1:] if argv is None else argv
    env = os.environ if env is None else env
    if len(argv) != 1:
        print("usage: staging_api_alert.py failed|recovered", file=sys.stderr)
        return 2
    if request is None:
        token = env.get("GH_TOKEN", "")
        if not token:
            print("staging-api-alert: GH_TOKEN is required", file=sys.stderr)
            return 2
        request = lambda method, path, payload=None: github_request(method, path, token, payload)  # noqa: E731
    try:
        run(argv[0], env, request)
    except (AlertError, OSError, ValueError, KeyError) as exc:
        print(f"staging-api-alert: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
