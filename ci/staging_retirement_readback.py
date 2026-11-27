#!/usr/bin/env python3
"""Read saved retirement evidence for an already selected staging attempt."""

import argparse
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from staging_admission import (
    DEPLOY_STEPS, HASH, MAX_RUN_PAGE, PRIVATE_JOURNEY, SHA, TERMINAL_CONCLUSIONS, WORKFLOW,
    Refused, decimal, parse_json, require, require_steps, timestamp,
)

REPO = "TogetherWeOwn/nntunev2"
RETIREMENT = "Retire the selected private staging fixture"
CONTAINMENT = "Verify the pinned staging disable deployment"
RETIRE = "Retire the selected tone A versions once"
CONFIRMED = "Private staging fixture cleanup: confirmed"


def attempt(records, run_id, number, sha, event):
    route = f"runs/{run_id}/attempts/{number}"
    run = records[route]
    require(decimal(run.get("id")) == run_id and decimal(run.get("run_attempt")) == number
            and run.get("path") in (WORKFLOW, f"{WORKFLOW}@main", f"{WORKFLOW}@refs/heads/main")
            and run.get("head_sha") == sha and run.get("head_branch") == "main"
            and run.get("event") == event and run.get("status") == "completed"
            and run.get("conclusion") in TERMINAL_CONCLUSIONS, "selected attempt does not match")
    repository = run["repository"]
    head_repository = run["head_repository"]
    require(repository.get("full_name") == head_repository.get("full_name") == REPO
            and decimal(repository.get("id")) == decimal(head_repository.get("id")),
            "foreign staging attempt")
    page = records[f"{route}/jobs"]
    rows, count = page.get("jobs"), page.get("total_count")
    require(isinstance(rows, list) and type(count) is int and count == len(rows) <= 100,
            "incomplete attempt jobs")
    ids = set()
    for row in rows:
        job_id = decimal(row.get("id"))
        require(job_id not in ids and decimal(row.get("run_id")) == run_id
                and ("run_attempt" not in row or decimal(row["run_attempt"]) == number)
                and row.get("head_sha") == sha, "job does not match the selected attempt")
        ids.add(job_id)
    return run, rows


def job(rows, name, conclusion):
    matches = [row for row in rows if row.get("name") == name]
    require(len(matches) == 1, "missing or ambiguous staging job")
    (row,) = matches
    require(row.get("status") == "completed" and row.get("conclusion") == conclusion,
            "required job outcome is missing in this attempt")
    return row


def require_already_cleared(evidence, sha, run_id, number, scope_sha256):
    require(SHA.fullmatch(sha) and HASH.fullmatch(scope_sha256), "invalid candidate or scope pin")
    records = evidence["actions"]
    run_id = decimal(run_id)
    candidate, jobs = attempt(records, run_id, decimal(number), sha, "push")
    job(jobs, RETIREMENT, "skipped")
    job(jobs, PRIVATE_JOURNEY, "skipped")
    deploy_starts = []
    for name, step_name in DEPLOY_STEPS.items():
        deploy = job(jobs, name, "success")
        require_steps(deploy, (step_name,), "selected attempt did not deploy")
        step = next(step for step in deploy["steps"] if step["name"] == step_name)
        deploy_starts.append(timestamp(step.get("started_at")))

    receipt = evidence["retirement"]
    prior_sha = receipt.get("sha")
    prior_run_id = decimal(receipt.get("run_id"))
    require(isinstance(prior_sha, str) and SHA.fullmatch(prior_sha)
            and prior_run_id != run_id and receipt.get("scope_sha256") == scope_sha256,
            "prior dispatch does not match the approved fixture scope")
    prior, jobs = attempt(records, prior_run_id, "1", prior_sha, "workflow_dispatch")
    require(prior["conclusion"] == "success"
            and decimal(prior.get("workflow_id")) == decimal(candidate.get("workflow_id"))
            and decimal(prior["repository"]["id"]) == decimal(candidate["repository"]["id"]),
            "prior retirement dispatch is unverified")
    retired = job(jobs, RETIREMENT, "success")
    require_steps(retired, (CONTAINMENT, RETIRE), "protected retirement steps did not pass")
    times = {step["name"]: (timestamp(step.get("started_at")), timestamp(step.get("completed_at")))
             for step in retired["steps"] if step["name"] in (CONTAINMENT, RETIRE)}
    containment_start, containment_end = times[CONTAINMENT]
    retire_start, retire_end = times[RETIRE]
    require(containment_start <= containment_end <= retire_start <= retire_end
            <= timestamp(retired.get("completed_at")) <= min(deploy_starts), "invalid retirement timing")
    require(decimal(retired.get("id")) == decimal(receipt.get("job_id"))
            and receipt.get("verdict") == CONFIRMED, "prior confirmation is missing or mismatched")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sha")
    parser.add_argument("run_id")
    parser.add_argument("attempt")
    parser.add_argument("scope_sha256")
    arguments = parser.parse_args()
    try:
        evidence = parse_json(sys.stdin.buffer.read(MAX_RUN_PAGE + 1), MAX_RUN_PAGE)
        require_already_cleared(evidence, arguments.sha, arguments.run_id,
                                arguments.attempt, arguments.scope_sha256)
    except Refused as error:
        print(f"Private staging fixture retirement: pending ({error})")
        return 1
    except (AttributeError, KeyError, OSError, RecursionError, TypeError, ValueError):
        print("Private staging fixture retirement: pending (incomplete or invalid readback)")
        return 1
    print("Private staging fixture retirement: already cleared")
    return 0


if __name__ == "__main__":
    sys.exit(main())
