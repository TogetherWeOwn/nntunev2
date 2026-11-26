#!/usr/bin/env python3
"""Report superseded automatic staging pushes from saved Actions REST pages."""

import argparse
import json
import re
import sys


REPO = "TogetherWeOwn/nntunev2"
WORKFLOW = ".github/workflows/deploy.yml"
RUN_STATES = ("queued", "pending", "requested", "waiting", "in_progress")
UNSTARTED_JOBS = ("queued", "pending", "requested")
PAGE_SIZE = 100
MAX_ROWS = 999  # GitHub search-filtered run listings are capped at 1,000.
MAX_INPUT = 16 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}")
RUN_ID = re.compile(r"[1-9][0-9]{0,15}")


class Indeterminate(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise Indeterminate(reason)


def unique_fields(pairs):
    result = dict(pairs)
    require(len(result) == len(pairs), "duplicate JSON field")
    return result


def positive_id(value):
    return type(value) is int and 0 < value < 10**16


def pages(inventory, key):
    reason = f"incomplete or inconsistent {key} pages"
    require(isinstance(inventory, list) and inventory and isinstance(inventory[0], dict), reason)
    total = inventory[0].get("total_count")
    require(type(total) is int and 0 <= total <= MAX_ROWS, reason)
    require(len(inventory) == max(1, (total + PAGE_SIZE - 1) // PAGE_SIZE), reason)
    rows = {}
    for index, page in enumerate(inventory):
        require(isinstance(page, dict) and type(page.get("total_count")) is int
                and page["total_count"] == total, reason)
        batch = page.get(key)
        require(isinstance(batch, list) and len(batch) == min(PAGE_SIZE, total - index * PAGE_SIZE), reason)
        for row in batch:
            require(isinstance(row, dict) and positive_id(row.get("id")) and row["id"] not in rows, reason)
            rows[row["id"]] = row
    return rows


def automatic_pending(run):
    return (run["status"] in ("pending", "queued") and run.get("event") == "push"
            and run.get("head_branch") == "main" and run["run_attempt"] == 1
            and "conclusion" in run and run["conclusion"] is None)


def staging_repository(repo):
    return isinstance(repo, dict) and repo.get("full_name") == REPO and positive_id(repo.get("id"))


def no_jobs_started(run, inventory):
    jobs = pages(inventory, "jobs")
    for job in jobs.values():
        require(type(job.get("run_id")) is int and job["run_id"] == run["id"]
                and type(job.get("run_attempt")) is int and job["run_attempt"] == run["run_attempt"]
                and job.get("head_sha") == run["head_sha"], "job evidence belongs to a different run attempt")
        require({"status", "started_at", "completed_at", "conclusion"} <= job.keys(),
                "missing job state evidence")
    return all(job["status"] in UNSTARTED_JOBS and job["started_at"] is None
               and job["completed_at"] is None and job["conclusion"] is None
               and job.get("runner_id") in (None, 0) and job.get("steps", []) == []
               for job in jobs.values())


def select(evidence, pinned_runs, pinned_shas):
    require(isinstance(evidence, dict), "missing input object")
    workflow, inventory, jobs = (evidence.get(key) for key in ("workflow", "runs", "jobs"))
    require(isinstance(workflow, dict) and workflow.get("path") == WORKFLOW
            and workflow.get("state") == "active" and positive_id(workflow.get("id")),
            "missing active staging workflow identity")
    require(isinstance(inventory, dict) and inventory.keys() == set(RUN_STATES)
            and isinstance(jobs, dict), "missing unresolved run or job inventory")

    runs, numbers, eligible, kept = {}, set(), {}, {}
    for state in RUN_STATES:
        for rid, run in pages(inventory[state], "workflow_runs").items():
            number, sha = run.get("run_number"), run.get("head_sha")
            repo = run.get("repository")
            require(rid not in runs and positive_id(number) and number not in numbers
                    and positive_id(run.get("run_attempt")) and run.get("status") == state
                    and run.get("path") == WORKFLOW and type(run.get("workflow_id")) is int
                    and run["workflow_id"] == workflow["id"] and staging_repository(repo)
                    and isinstance(sha, str) and SHA.fullmatch(sha)
                    and isinstance(run.get("event"), str) and isinstance(run.get("head_branch"), str),
                    "duplicate or out-of-scope workflow run")
            runs[rid] = sha
            numbers.add(number)
            head_repo = run.get("head_repository")
            if not automatic_pending(run) or not staging_repository(head_repo) or head_repo["id"] != repo["id"]:
                kept[rid] = "keep_outside_selection"
            elif no_jobs_started(run, jobs.get(str(rid))):
                eligible[rid] = number
            else:
                kept[rid] = "keep_job_activity"

    # Run numbers increase within the workflow. A rerun keeps its number.
    newest = max(eligible, key=eligible.get, default=None)
    rows = []
    for rid, sha in sorted(runs.items()):
        if rid in pinned_runs or sha in pinned_shas:
            disposition = "keep_pinned"
        elif rid == newest:
            disposition = "keep_newest_eligible"
        else:
            disposition = kept.get(rid, "superseded")
        rows.append({"run_id": rid, "head_sha": sha, "disposition": disposition})
    return {"status": "complete", "newest_eligible_run_id": newest, "runs": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pin-run", action="append", default=[], metavar="RUN_ID")
    parser.add_argument("--pin-sha", action="append", default=[], metavar="SHA")
    args = parser.parse_args()
    try:
        require(all(RUN_ID.fullmatch(pin) for pin in args.pin_run)
                and all(SHA.fullmatch(pin) for pin in args.pin_sha), "invalid candidate pin")
        data = sys.stdin.buffer.read(MAX_INPUT + 1)
        require(len(data) <= MAX_INPUT, "oversized input")
        evidence = json.loads(data, object_pairs_hook=unique_fields)
        report = select(evidence, set(map(int, args.pin_run)), set(args.pin_sha))
    except Indeterminate as error:
        print(json.dumps({"status": "indeterminate", "reason": str(error)}))
        return 1
    except (OSError, ValueError, RecursionError):
        print(json.dumps({"status": "indeterminate", "reason": "unreadable or malformed JSON"}))
        return 1
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
