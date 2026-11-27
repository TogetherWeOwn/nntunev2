#!/usr/bin/env python3
"""Nominate an ordinary staging attempt from saved GitHub REST evidence."""

import argparse
import json
import sys

from staging_admission import (
    DEPLOY_STEPS, JOURNEY, MAX_RUN_PAGE, PENDING_RUN_STATES, SHA,
    TERMINAL_CONCLUSIONS, WORKFLOW, Refused, decimal, parse_json, require, timestamp,
)


def complete_page(page, key):
    require(isinstance(page, dict), "missing listing")
    count, rows = page.get("total_count"), page.get(key)
    require(isinstance(rows, list) and type(count) is int and count == len(rows) <= 100
            and all(isinstance(row, dict) for row in rows), "incomplete listing")
    return rows


def contains_squash(comparisons, squash, head):
    # Use the unpaginated Compare response. GitHub includes the head as its
    # last commit even when it limits the response to 250 commits.
    comparison = comparisons[f"compare/{squash}...{head}"]
    base = comparison["base_commit"]["sha"]
    merge_base = comparison["merge_base_commit"]["sha"]
    ahead, behind = comparison["ahead_by"], comparison["behind_by"]
    total, commits = comparison["total_commits"], comparison["commits"]
    require(base == squash and SHA.fullmatch(merge_base)
            and type(ahead) is int and type(behind) is int and type(total) is int
            and behind >= 0 and ahead == total >= 0
            and isinstance(commits, list) and len(commits) == min(ahead, 250),
            "incomplete comparison")
    commit_shas = [row["sha"] for row in commits]
    require(all(isinstance(sha, str) and SHA.fullmatch(sha) and sha != merge_base for sha in commit_shas)
            and len(set(commit_shas)) == len(commit_shas), "invalid comparison commit inventory")
    require((commit_shas[-1] if commit_shas else merge_base) == head,
            "comparison belongs to a different head")
    # REST status describes HEAD relative to BASE.
    status = {(False, False): "identical", (True, False): "ahead",
              (False, True): "behind", (True, True): "diverged"}[ahead > 0, behind > 0]
    require(comparison["status"] == status and (merge_base == squash) == (behind == 0)
            and (head == squash) == (status == "identical"), "incoherent comparison")
    return behind == 0


def nominate(evidence, *, squash, repo, repository_id):
    require(SHA.fullmatch(squash), "invalid prerequisite SHA")
    workflow = evidence["workflow"]
    require(workflow["path"] == WORKFLOW and workflow["state"] == "active", "wrong staging workflow")
    workflow_id = decimal(workflow["id"])
    rows = complete_page(evidence.get("runs"), "workflow_runs")
    jobs, comparisons = evidence["jobs"], evidence["comparisons"]
    require(isinstance(jobs, dict) and isinstance(comparisons, dict), "missing readbacks")

    seen_ids, seen_numbers, latest_by_sha = set(), set(), {}
    for run in rows:
        run_id = int(decimal(run["id"]))
        require(run_id not in seen_ids, "duplicate run")
        seen_ids.add(run_id)
        event = run["event"]
        require(isinstance(event, str) and event, "missing run event")
        if event != "push" or decimal(run["workflow_id"]) != workflow_id:
            continue
        branch = run["head_branch"]
        require(isinstance(branch, str) and branch, "missing push branch")
        if branch != "main":
            continue
        require(run["path"] == WORKFLOW, "unexpected staging workflow path")
        for key in ("repository", "head_repository"):
            require(run[key]["full_name"] == repo and decimal(run[key]["id"]) == repository_id,
                    "foreign staging run")
        number = int(decimal(run["run_number"]))
        attempt = decimal(run["run_attempt"])
        head, status, conclusion = run["head_sha"], run["status"], run["conclusion"]
        require(SHA.fullmatch(head) and number not in seen_numbers
                and ((status in PENDING_RUN_STATES and conclusion is None)
                     or (status == "completed" and conclusion in TERMINAL_CONCLUSIONS)),
                "incoherent staging run")
        seen_numbers.add(number)
        # The existing verifier resolves the highest run ID for a SHA.
        current = latest_by_sha.get(head)
        if current is None or run_id > current[0]:
            latest_by_sha[head] = (run_id, attempt, status, conclusion)

    candidates = []
    for head, (run_id, attempt, status, conclusion) in latest_by_sha.items():
        # Only a completed first attempt can be handed to the verifier.
        if (attempt, status, conclusion) != ("1", "completed", "success"):
            continue
        if not contains_squash(comparisons, squash, head):
            continue
        attempt_jobs = complete_page(jobs.get(f"runs/{run_id}/attempts/{attempt}/jobs"), "jobs")
        required_jobs = {}
        for name in (*DEPLOY_STEPS, JOURNEY):
            matches = [job for job in attempt_jobs if job["name"] == name]
            require(len(matches) == 1, "missing or ambiguous staging job")
            job = matches[0]
            require(decimal(job["run_id"]) == str(run_id) and decimal(job["run_attempt"]) == attempt
                    and job["head_sha"] == head and job["status"] == "completed"
                    and job["conclusion"] in TERMINAL_CONCLUSIONS, "incoherent staging job")
            required_jobs[name] = job
        if any(job["conclusion"] != "success" for job in required_jobs.values()):
            continue
        require(all(timestamp(job["started_at"]) <= timestamp(job["completed_at"])
                    for job in required_jobs.values()), "job completed before it started")
        # The ordinary journey follows both deploy jobs in the shared writer
        # group. Queued runs can complete in a different order from creation.
        journey = required_jobs[JOURNEY]
        started = timestamp(journey["started_at"])
        require(all(timestamp(required_jobs[name]["completed_at"]) <= started for name in DEPLOY_STEPS),
                "journey started before deployment finished")
        completed = timestamp(journey["completed_at"])
        candidates.append((completed, head, str(run_id), attempt))

    require(candidates, "no eligible staging attempt")
    candidates.sort(reverse=True)
    completed, head, run_id, attempt = candidates[0]
    require(len(candidates) == 1 or completed != candidates[1][0], "ambiguous deployment order")
    return {"status": "nominated", "sha": head, "run_id": run_id, "run_attempt": attempt}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("prerequisite_sha")
    parser.add_argument("repository", help="owner/name of the staging repository")
    parser.add_argument("repository_id", help="GitHub ID of the staging repository")
    args = parser.parse_args()
    try:
        result = nominate(parse_json(sys.stdin.buffer.read(MAX_RUN_PAGE + 1), MAX_RUN_PAGE),
                          squash=args.prerequisite_sha, repo=args.repository,
                          repository_id=decimal(args.repository_id))
    except (Refused, OSError, KeyError, TypeError, ValueError, RecursionError):
        print(json.dumps({"status": "pending"}))
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
