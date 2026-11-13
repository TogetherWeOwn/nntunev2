"""Fail closed unless a checkout and the requested history are locally complete.

This is an inert building block: callers must supply trusted object IDs and paths.
It does not fetch, construct a graph, or authorize qualification.
"""

import hashlib
import os
from pathlib import Path
import re
import stat
import subprocess


SHA = re.compile(r"[0-9a-f]{40}\Z")


def git_environment():
    # Git's environment can redirect repository discovery, objects and index.
    # Keep tracing for the no-fetch assertions, but not repository overrides.
    return {key: value for key, value in os.environ.items()
            if not key.startswith("GIT_") or key == "GIT_TRACE2_EVENT"}


def guard(root, commits, trusted_paths):
    if not commits or any(not SHA.fullmatch(oid) for oid in commits):
        raise ValueError("expected full commit object IDs")
    if not trusted_paths or any(not p or p.startswith("-") for p in trusted_paths):
        raise ValueError("expected trusted input paths")

    root = Path(root).resolve()

    def git(*args):
        result = subprocess.run(
            ["git", "--no-lazy-fetch", "--no-replace-objects",
             "-c", "core.fsmonitor=false", "-c", "core.ignoreCase=false", "-C", str(root),
             "--work-tree", str(root), *args],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
            env=git_environment(),
        )
        if result.returncode:
            raise ValueError("Git object guard failed: " + result.stderr.decode(errors="replace"))
        return result.stdout

    if git("rev-parse", "--is-shallow-repository").strip() != b"false":
        raise ValueError("shallow repository")
    graft_path = Path(os.fsdecode(git("rev-parse", "--git-path", "info/grafts").removesuffix(b"\n")))
    if not graft_path.is_absolute():
        graft_path = Path(root) / graft_path
    if os.path.lexists(graft_path):
        raise ValueError("Git grafts can hide history")
    for path in trusted_paths:
        entries = git("ls-files", "-v", "-z", "--", path)
        if not entries:
            raise ValueError("trusted input path has no tracked files")
        if any(entry[:2] != b"H " for entry in entries.split(b"\0") if entry):
            raise ValueError("suppressed or unmerged trusted inputs")
    # Inspect the index and untracked paths without status refreshing the index:
    # status can execute repository-configured clean filters.
    if (git("diff", "--cached", "--name-only", "--no-ext-diff", "--no-textconv",
            "HEAD", "--", *trusted_paths)
            or git("ls-files", "--others", "--exclude-standard", "-z", "--", *trusted_paths)
            or git("ls-files", "--others", "--ignored", "--exclude-standard", "-z",
                   "--", *trusted_paths)):
        raise ValueError("dirty trusted inputs")
    for oid in commits:
        if git("cat-file", "-t", oid).strip() != b"commit":
            raise ValueError("object is not a commit")
    objects = git("rev-list", "--objects", "--missing=print", *commits)
    if any(line.startswith(b"?") for line in objects.splitlines()):
        raise ValueError("missing reachable object")
    git("fsck", "--connectivity-only", "--no-reflogs", *commits)

    # Compare raw bytes and executable identity to the index without filters.
    for entry in git("ls-files", "--stage", "-z", "--", *trusted_paths).split(b"\0"):
        if not entry:
            continue
        metadata, name = entry.split(b"\t", 1)
        mode, oid, stage = metadata.split()
        if mode not in (b"100644", b"100755") or stage != b"0":
            raise ValueError("unsupported trusted input type")
        path = Path(root) / os.fsdecode(name)
        try:
            for parent in path.relative_to(root).parents:
                if parent == Path("."):
                    continue
                if not stat.S_ISDIR((root / parent).lstat().st_mode):
                    raise ValueError("unsupported trusted input type")
            actual_mode = path.lstat().st_mode
            if not stat.S_ISREG(actual_mode):
                raise ValueError("unsupported trusted input type")
            if bool(actual_mode & stat.S_IXUSR) != (mode == b"100755"):
                raise ValueError("dirty trusted inputs")
            content = path.read_bytes()
        except OSError as error:
            raise ValueError("cannot read trusted input") from error
        digest = hashlib.sha1(b"blob " + str(len(content)).encode() + b"\0" + content).hexdigest().encode()
        if digest != oid:
            raise ValueError("dirty trusted inputs")
