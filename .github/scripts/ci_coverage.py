"""Plan CI coverage once from the actual Actions checkout comparison."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


WEB = {"web-checks", "web-build", "web-e2e", "web-a11y", "history-fixture"}
BROWSER = {"web-build", "web-e2e"}
RELEASE = {"worker-wasm-budget", "worker-release-builds"}
WORKERS = {"api", "ingest", "mb", "repair", "stream", "transcode", "youtube"}
# Worker suites share Cargo path dependencies, source/SQL fixtures, and wire
# contracts. Keep that entire set together until those dependencies can be isolated.
BACKEND = {
    "api", "api-migrations", "mb-import", "mb-import-etl", "repair-container",
    "rust-ingest", "rust-mb", "rust-repair", "rust-test", "rust-transcode",
    "rust-wasm", "test-nntp", "transcode-container",
} | BROWSER | RELEASE
SELECTABLE = WEB | BACKEND
COMPONENT = r"[A-Za-z0-9][A-Za-z0-9_.-]*"
PATH = rf"(?:{COMPONENT}/)*{COMPONENT}"
SHA = re.compile(r"(?!0{40}$)[0-9a-f]{40}")


def consumers(path):
    if path in {"LICENSE", "README.md", "ROADMAP.md"} or re.fullmatch(
        rf"docs/{PATH}\.(md|rst)", path
    ):
        return "documentation/metadata", set()
    worker = re.fullmatch(rf"workers/([a-z]+)/(?:src|tests)/{PATH}\.rs", path)
    if worker and worker[1] in WORKERS:
        return f"workers/{worker[1]}", BACKEND
    if re.fullmatch(rf"apps/web/(src|tests|static)/{PATH}\.(ts|js|svelte|css|svg|png|webp)", path):
        return "apps/web", WEB
    if path == "migrations/manifest.toml" or re.fullmatch(r"migrations/[0-9]+_[a-z0-9_]+\.sql", path):
        return "migrations", BACKEND
    # Includes shared sources, tool/config files, package manifests and locks.
    return "full", SELECTABLE


def changed_paths(base, tested, root):
    if not SHA.fullmatch(base) or not SHA.fullmatch(tested):
        raise ValueError("missing or invalid comparison commits")
    raw = subprocess.run(
        ["git", "--no-replace-objects", "diff", "--raw", "-z", "--no-renames", "--no-ext-diff",
         "--no-textconv", "--ignore-submodules=none", base, tested, "--"],
        check=True, capture_output=True, timeout=30, cwd=root,
    ).stdout
    if not raw:
        return []
    *fields, terminator = raw.split(b"\0")
    if terminator or len(fields) % 2:
        raise ValueError("incomplete Git diff")

    paths = []
    for header, path in zip(fields[::2], fields[1::2]):
        modes = re.fullmatch(rb":([0-7]{6}) ([0-7]{6}) [0-9a-f]+ [0-9a-f]+ ([ADMT])", header)
        if not modes or not path:
            raise ValueError("invalid Git diff record")
        if not set(modes.groups()[:2]) <= {b"000000", b"100644"}:
            return [None]  # Executable files, symlinks and submodules require full CI.
        paths.append(path.decode("utf-8"))
    return paths


def referenced_prose(paths, base, tested, root):
    # Search names to catch relative paths as well as repository-root references.
    # A reference in either tree is sufficient, even if the prose was deleted.
    matches = subprocess.run(
        ["git", "--no-replace-objects", "grep", "-qF", "--no-textconv", "-f", "-", base, tested,
         "--", ":(exclude)*.md", ":(exclude)*.rst", ":(exclude,top)LICENSE"],
        input="\n".join(sorted({Path(path).name for path in paths})) + "\n",
        text=True, capture_output=True, timeout=30, cwd=root,
    )
    if matches.returncode not in (0, 1):
        matches.check_returncode()
    return matches.returncode == 0


def plan(event, base, tested, root=None):
    # Push, queue, dispatch and scheduled main runs always cover the full tree.
    if event != "pull_request":
        return True, {"full"}, []
    try:
        paths = changed_paths(base, tested, root)
        if not paths or None in paths:
            return True, {"full"}, []
        areas, jobs, prose = set(), set(), []
        for path in paths:
            area, applies = consumers(path)
            areas.add(area)
            jobs.update(applies)
            if area == "documentation/metadata":
                prose.append(path)
        if jobs != SELECTABLE and prose and referenced_prose(prose, base, tested, root):
            return True, areas | {"referenced documentation (full)"}, []
    except (OSError, ValueError, subprocess.SubprocessError):
        return False, {"incomplete (full coverage required)"}, []
    return True, areas, sorted(SELECTABLE - jobs)


def main():
    event, base, tested, head = sys.argv[1:]
    complete, areas, skip = plan(event, base, tested)
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as summary:
        print("### CI coverage", file=summary)
        print(f"- Head `{head}`, tested `{tested}`, base `{base}`", file=summary)
        print(f"- Areas {', '.join(sorted(areas))}", file=summary)
        print(f"- Approved skips {', '.join(skip) or 'none'}", file=summary)
    print(f"complete={str(complete).lower()}")
    print(f"skip={json.dumps(skip, separators=(',', ':'))}")


if __name__ == "__main__":
    main()
