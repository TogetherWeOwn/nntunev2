#!/usr/bin/env python3
"""Read exact GitHub source evidence and certify executed web acceptance only."""

import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import os
from pathlib import Path
import re
import urllib.parse
import zipfile

from web_release_common import (
    JOBS, MAX_BYTES, Refusal, cli_result, positive, read_json, require,
    source_identity, validate_manifest, validate_plan,
)


def timestamp(value):
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(result.tzinfo is not None, "timestamp-zone")
    return result


def certify(args):
    source = source_identity(args.repository, args.sha, args.run_id, args.attempt)
    fixture, manifest = read_json(args.input), read_json(args.manifest)
    validate_manifest(manifest, source=source)
    require(fixture.get("schema") == "nntune-web-evidence-v1", "evidence-schema")
    run = fixture.get("run", {})
    require(run.get("id") == args.run_id and run.get("run_attempt") == args.attempt and run.get("head_sha") == args.sha, "run-identity")
    require(run.get("head_branch") == "main" and run.get("event") == "push" and run.get("path") == args.workflow == ".github/workflows/deploy.yml", "run-policy")
    repository, head_repository = run.get("repository", {}), run.get("head_repository", {})
    require(repository.get("full_name") == args.repository and head_repository.get("full_name") == args.repository, "run-repository")
    require(positive(repository.get("id")) and repository.get("id") == head_repository.get("id"), "repository-id")
    active = run.get("status") == "in_progress" and run.get("conclusion") is None and not args.require_completed
    require(active or (run.get("status") == "completed" and run.get("conclusion") == "success"), "run-not-successful")
    pages = fixture.get("jobPages")
    require(isinstance(pages, list) and 0 < len(pages) <= 200, "jobs-pages")
    total = pages[0].get("total_count")
    require(positive(total), "jobs-total")
    jobs = []
    for number, page in enumerate(pages, 1):
        require(page.get("page") == number and page.get("total_count") == total, "jobs-pagination")
        require(page.get("nextPage") == (number + 1 if number < len(pages) else None), "jobs-pagination")
        require(isinstance(page.get("jobs"), list) and len(page["jobs"]) > 0, "jobs-empty-page")
        jobs.extend(page["jobs"])
    require(len(jobs) == total and len({j.get("id") for j in jobs}) == total and all(positive(j.get("id")) for j in jobs), "jobs-incomplete-or-duplicate")
    for name, steps in JOBS.items():
        matches = [job for job in jobs if job.get("name") == name]
        require(len(matches) == 1, "required-job-unique")
        job = matches[0]
        require(job.get("run_id") == args.run_id and job.get("run_attempt") == args.attempt and job.get("head_sha") == args.sha, "job-source")
        require(job.get("status") == "completed" and job.get("conclusion") == "success", "job-not-executed")
        for step_name in steps:
            matches = [step for step in job.get("steps", []) if step.get("name") == step_name]
            require(len(matches) == 1, "required-step-unique")
            step = matches[0]
            require(step.get("conclusion") != "skipped", "acceptance-skipped")
            require(step.get("status") == "completed" and step.get("conclusion") == "success", "step-not-executed")
    artifact = fixture.get("artifact", {})
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", args.artifact_digest) is not None, "artifact-digest")
    require(artifact.get("id") == args.artifact_id and artifact.get("name") == args.artifact_name and artifact.get("digest") == args.artifact_digest, "artifact-identity")
    require(artifact.get("expired") is False and positive(artifact.get("size_in_bytes")), "artifact-expired-or-empty")
    require(timestamp(artifact["created_at"]) >= timestamp(run["run_started_at"]), "artifact-attempt")
    require(timestamp(artifact["expires_at"]) > datetime.now(timezone.utc), "artifact-expired")
    origin = artifact.get("workflow_run", {})
    require(origin.get("id") == args.run_id and origin.get("head_sha") == args.sha and origin.get("head_branch") == "main", "artifact-run")
    require(origin.get("repository_id") == repository["id"] and origin.get("head_repository_id") == repository["id"], "artifact-repository")
    evidence = {
        "runId": args.run_id, "attempt": args.attempt, "workflow": args.workflow, "jobs": list(JOBS),
        "artifactId": args.artifact_id, "artifactName": args.artifact_name, "artifactDigest": args.artifact_digest,
    }
    certificate = {"schema": "nntune-web-certificate-v1", "components": ["web"], "source": source, "evidence": evidence}
    write_new(args.output, certificate)
    return {"certificate": certificate}


def write_new(path, value):
    with Path(path).open("x", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False)


def api(repository, path, raw=False):
    # Pure certification must not initialize SSL/socket machinery. The offline
    # bank replaces socket creation, and no network dependency belongs there.
    import urllib.error
    import urllib.request

    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None

    source_identity(repository, "0" * 40, 1, 1)
    token = os.environ.get("GITHUB_TOKEN", "")
    require(bool(token), "github-token-missing")
    request = urllib.request.Request(
        f"https://api.github.com/repos/{repository}{path}",
        headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"},
    )
    try:
        with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
            require(response.status == 200, "github-status")
            data = response.read(MAX_BYTES + 1)
    except urllib.error.HTTPError as error:
        if not raw or error.code != 302:
            raise Refusal("github-read-refused") from error
        # Signed artifact URLs receive no GitHub Authorization header.
        location = error.headers.get("Location", "")
        target = urllib.parse.urlsplit(location)
        require(target.scheme == "https" and target.hostname and not target.username and not target.password, "artifact-redirect")
        require(target.hostname.endswith((".blob.core.windows.net", ".githubusercontent.com", ".actions.githubusercontent.com")), "artifact-redirect-host")
        try:
            with urllib.request.build_opener(NoRedirect()).open(location, timeout=60) as response:
                require(response.status == 200, "artifact-download-status")
                data = response.read(MAX_BYTES + 1)
        except urllib.error.HTTPError as error:
            raise Refusal("artifact-download-refused") from error
    require(len(data) <= MAX_BYTES, "github-response-budget")
    return data if raw else json.loads(data)


def fetch(args):
    run = api(args.repository, f"/actions/runs/{args.run_id}/attempts/{args.attempt}")
    artifact = api(args.repository, f"/actions/artifacts/{args.artifact_id}")
    require(artifact.get("id") == args.artifact_id and artifact.get("name") == args.artifact_name and artifact.get("digest") == args.artifact_digest, "artifact-selection")
    pages, count = [], 0
    for number in range(1, 201):
        page = api(args.repository, f"/actions/runs/{args.run_id}/attempts/{args.attempt}/jobs?per_page=100&page={number}")
        require(isinstance(page.get("jobs"), list) and type(page.get("total_count")) is int, "jobs-response")
        count += len(page["jobs"])
        more = count < page["total_count"]
        pages.append({"page": number, "total_count": page["total_count"], "jobs": page["jobs"], "nextPage": number + 1 if more else None})
        require(not more or len(page["jobs"]) == 100, "jobs-truncated")
        if not more:
            break
    require(pages[-1]["nextPage"] is None and count == pages[0]["total_count"], "jobs-truncated")
    archive = api(args.repository, f"/actions/artifacts/{args.artifact_id}/zip", raw=True)
    require("sha256:" + hashlib.sha256(archive).hexdigest() == args.artifact_digest, "archive-digest")
    try:
        with zipfile.ZipFile(io.BytesIO(archive)) as zipped:
            require(zipped.namelist() == ["web-release.tar.gz"], "artifact-zip-members")
            item = zipped.getinfo("web-release.tar.gz")
            require(0 < item.file_size <= MAX_BYTES, "artifact-zip-budget")
            payload = zipped.read(item)
    except zipfile.BadZipFile as error:
        raise Refusal("artifact-zip-invalid") from error
    with Path(args.archive_output).open("xb") as handle:
        handle.write(payload)
    write_new(args.output, {"schema": "nntune-web-evidence-v1", "run": run, "artifact": artifact, "jobPages": pages})
    return {}


def resolve(args):
    require(re.fullmatch(r"canary/v[0-9.]+-canary\.[0-9]+", args.canary) is not None, "source-canary")
    ref = api(args.repository, "/git/ref/tags/" + urllib.parse.quote(args.canary, safe=""))
    require(ref.get("ref") == "refs/tags/" + args.canary and ref.get("object", {}).get("type") == "tag", "canary-ref")
    annotation = api(args.repository, "/git/tags/" + ref["object"]["sha"])
    require(annotation.get("object", {}).get("type") == "commit" and annotation["object"].get("sha") == args.sha, "canary-target")
    message = annotation.get("message", "")
    require(message.startswith("calver-plan-v1\n"), "canary-provenance")
    record = json.loads(message.split("\n", 1)[1])
    evidence = record.pop("publicationEvidence", None)
    require(isinstance(evidence, dict), "canary-evidence")
    source = source_identity(args.repository, args.sha, evidence.get("runId"), evidence.get("attempt"))
    validate_plan(record, source)
    require(record["tag"] == args.canary, "canary-tag")
    require({key: evidence.get(key) for key in ("runId", "attempt", "workflow", "jobs")} == record["source"], "canary-selector")
    require(positive(evidence.get("artifactId")) and isinstance(evidence.get("artifactName"), str) and re.fullmatch(r"sha256:[0-9a-f]{64}", evidence.get("artifactDigest", "")), "canary-artifact")
    write_new(args.output, {"plan": record, "evidence": evidence})
    return {"selection": {"plan": record, "evidence": evidence}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    certification = commands.add_parser("certify")
    certification.add_argument("--input", required=True)
    certification.add_argument("--manifest", required=True)
    certification.add_argument("--sha", required=True)
    certification.add_argument("--workflow", required=True)
    certification.add_argument("--require-completed", action="store_true")
    fetching = commands.add_parser("fetch")
    fetching.add_argument("--archive-output", required=True)
    for command in (certification, fetching):
        command.add_argument("--run-id", type=int, required=True)
        command.add_argument("--attempt", type=int, required=True)
        command.add_argument("--artifact-id", type=int, required=True)
        command.add_argument("--artifact-name", required=True)
        command.add_argument("--artifact-digest", required=True)
    resolving = commands.add_parser("resolve")
    resolving.add_argument("--canary", required=True)
    resolving.add_argument("--sha", required=True)
    for command in (certification, fetching, resolving):
        command.add_argument("--repository", required=True)
        command.add_argument("--output", required=True)
    args = parser.parse_args()
    cli_result(lambda: {"certify": certify, "fetch": fetch, "resolve": resolve}[args.command](args))


if __name__ == "__main__":
    main()
