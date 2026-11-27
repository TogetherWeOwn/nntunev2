#!/usr/bin/env python3
"""Prepare and verify the ordinary staging API build without deploying it."""

from datetime import datetime, timezone
import io
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import zipfile
import zlib

from staging_admission import (
    ROOT, SHA, TERMINAL_CONCLUSIONS, Refused, decimal, digest, require, require_steps, timestamp,
)

REPOSITORY = "TogetherWeOwn/nntunev2"
CI_WORKFLOW = ".github/workflows/ci.yml"
JOB = "worker release builds"
DRY_RUN = "Dry-run generated staging upload configs without rebuilding"
PREPARE = "Prepare ordinary staging API candidate"
UPLOAD = "Upload ordinary staging API candidate"
MAX_ARCHIVE = 16 * 1024 * 1024
# worker-build 0.9 bundles the JS snippets into index.js. Keep the generated
# package metadata used for bundling alongside the three runtime modules.
FILES = {
    "index.js": 2 * 1024 * 1024,
    "index_bg.wasm": 12 * 1024 * 1024,
    "package.json": 16 * 1024,
    "worker/shim.mjs": 4096,
}
ARTIFACT_DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def artifact_name(sha, run_id):
    return f"staging-api-ordinary-v1-{sha}-{run_id}-1"


def prepare(source, destination):
    require(not source.is_symlink() and not (source / "worker").is_symlink(),
            "unexpected build output directory")
    files = {}
    for name, limit in FILES.items():
        path = source / name
        require(stat.S_ISREG(path.lstat().st_mode), "expected a regular build output")
        with path.open("rb") as member:
            data = member.read(limit + 1)
        require(0 < len(data) <= limit, "build output size outside bounds")
        files[name] = data
    destination.mkdir()
    (destination / "worker").mkdir()
    for name, data in files.items():
        (destination / name).write_bytes(data)


def read_bundle(data):
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            entries = archive.infolist()
            require(len(entries) == len(FILES) and {e.orig_filename for e in entries} == FILES.keys(),
                    "unexpected build archive inventory")
            files = {}
            for entry in entries:
                name = entry.orig_filename
                limit = FILES[name]
                require(entry.filename == name and stat.S_IFMT(entry.external_attr >> 16) in (0, stat.S_IFREG)
                        and not entry.flag_bits & 1 and entry.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)
                        and 0 < entry.file_size <= limit and 0 < entry.compress_size <= MAX_ARCHIVE,
                        "invalid build archive member")
                with archive.open(entry) as member:
                    files[name] = member.read(limit + 1)
                require(len(files[name]) == entry.file_size, "build archive member length mismatch")
    except (OSError, RuntimeError, ValueError, EOFError, zipfile.BadZipFile, zlib.error):
        raise Refused("unreadable build archive") from None
    return files


def matching(rows, field, value):
    require(all(isinstance(row, dict) for row in rows), "invalid candidate CI metadata")
    return [row for row in rows if row.get(field) == value]


def verified_candidate(actions, *, repository_id, sha, private_trial_upload):
    """Return verified bytes, or refuse without writing any files.

    Use the repository-scoped authenticated Actions client and the writer's
    own push SHA and gate output. The commit pins both tool manifests and
    lockfiles. The producer supplies the ordinary build only.
    """
    require(private_trial_upload == "0", "ordinary API build is unavailable for this mode")
    require(isinstance(sha, str) and SHA.fullmatch(sha), "invalid candidate SHA")
    repo_id = decimal(repository_id)
    workflow = actions.api("workflows/ci.yml")
    require(workflow.get("path") == CI_WORKFLOW, "wrong CI workflow")
    workflow_id = decimal(workflow.get("id"))
    runs = actions.listing(f"workflows/{workflow_id}/runs?branch=main&event=push&head_sha={sha}", "workflow_runs")
    require(len(runs) == 1 and isinstance(runs[0], dict), "missing or ambiguous candidate CI run")
    run_id = decimal(runs[0].get("id"))

    def check_run():
        run = actions.api(f"runs/{run_id}")
        status, conclusion = run.get("status"), run.get("conclusion")
        require(decimal(run.get("id")) == run_id and decimal(run.get("workflow_id")) == workflow_id
                and run.get("path") == CI_WORKFLOW and run.get("event") == "push"
                and run.get("head_branch") == "main" and run.get("head_sha") == sha
                and decimal(run.get("run_attempt")) == "1"
                and ((status == "in_progress" and "conclusion" in run and conclusion is None)
                     or (status == "completed" and conclusion in TERMINAL_CONCLUSIONS)),
                "wrong or superseded CI attempt")
        for field in ("repository", "head_repository"):
            repo = run.get(field)
            require(isinstance(repo, dict) and repo.get("full_name") == REPOSITORY
                    and decimal(repo.get("id")) == repo_id, "foreign CI producer")

    check_run()
    jobs = actions.listing(f"runs/{run_id}/attempts/1/jobs", "jobs")
    matches = matching(jobs, "name", JOB)
    require(len(matches) == 1, "missing or ambiguous Worker release build")
    job = matches[0]
    require(decimal(job.get("run_id")) == run_id and decimal(job.get("run_attempt")) == "1"
            and job.get("head_sha") == sha and job.get("status") == "completed"
            and job.get("conclusion") == "success", "Worker release build did not pass")
    require(isinstance(job.get("steps"), list) and all(isinstance(step, dict) for step in job["steps"]),
            "invalid candidate CI step metadata")
    require_steps(job, ("Build ordinary Worker release bundles", DRY_RUN, PREPARE, UPLOAD),
                  "required candidate build steps did not pass in order")
    artifacts = actions.listing(f"runs/{run_id}/artifacts", "artifacts")
    matches = matching(artifacts, "name", artifact_name(sha, run_id))
    require(len(matches) == 1, "missing or ambiguous API build artifact")
    artifact = matches[0]
    producer = artifact.get("workflow_run")
    size = artifact.get("size_in_bytes")
    fingerprint = artifact.get("digest")
    require(artifact.get("expired") is False and timestamp(artifact.get("expires_at")) > datetime.now(timezone.utc)
            and type(size) is int and 0 < size <= MAX_ARCHIVE
            and isinstance(fingerprint, str) and ARTIFACT_DIGEST.fullmatch(fingerprint)
            and isinstance(producer, dict) and decimal(producer.get("id")) == run_id
            and decimal(producer.get("repository_id")) == repo_id and decimal(producer.get("head_repository_id")) == repo_id
            and producer.get("head_branch") == "main" and producer.get("head_sha") == sha,
            "invalid or expired API build artifact")
    data = actions.read(f"artifacts/{decimal(artifact.get('id'))}/zip", MAX_ARCHIVE)
    require(len(data) == size and "sha256:" + digest(data) == fingerprint, "API build artifact digest mismatch")
    files = read_bundle(data)
    check_run()
    return files


def prepare_ci(root, env):
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True, timeout=10).strip()
    require(SHA.fullmatch(head) and head == env["GITHUB_SHA"], "checkout disagrees with CI SHA")
    require(decimal(env["GITHUB_RUN_ATTEMPT"]) == "1", "unexpected CI attempt")
    name = artifact_name(head, decimal(env["GITHUB_RUN_ID"]))
    path = Path(env["RUNNER_TEMP"]) / "staging-api-candidate"
    prepare(root / "workers/api/build", path)
    with Path(env["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        print(f"name={name}\npath={path}", file=output)


if __name__ == "__main__":
    try:
        prepare_ci(ROOT, os.environ)
    except (Refused, OSError, KeyError, subprocess.SubprocessError):
        sys.exit("ordinary staging API candidate preparation failed")
