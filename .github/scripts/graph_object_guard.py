"""Fail closed unless a checkout and the requested history are locally complete.

This is an inert building block: callers must supply trusted object IDs and paths.
It does not fetch, construct a graph, or authorize qualification.
"""

import hashlib
import os
from pathlib import Path
import re
import subprocess


SHA = re.compile(r"[0-9a-f]{40}\Z")


def guard(root, commits, trusted_paths):
    if not commits or any(not SHA.fullmatch(oid) for oid in commits):
        raise ValueError("expected full commit object IDs")
    if not trusted_paths or any(not p or p.startswith("-") for p in trusted_paths):
        raise ValueError("expected trusted input paths")

    def git(*args):
        result = subprocess.run(
            ["git", "--no-lazy-fetch", "--no-replace-objects",
             "-c", "core.fsmonitor=false", "-C", str(root), *args],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
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
    if git("status", "--porcelain=v1", "--untracked-files=all", "--ignored=matching",
           "--", *trusted_paths):
        raise ValueError("dirty trusted inputs")
    for oid in commits:
        if git("cat-file", "-t", oid).strip() != b"commit":
            raise ValueError("object is not a commit")
    objects = git("rev-list", "--objects", "--missing=print", *commits)
    if any(line.startswith(b"?") for line in objects.splitlines()):
        raise ValueError("missing reachable object")
    git("fsck", "--connectivity-only", "--no-reflogs", *commits)

    # Git status applies clean filters; compare the actual bytes to the index.
    for entry in git("ls-files", "--stage", "-z", "--", *trusted_paths).split(b"\0"):
        if not entry:
            continue
        metadata, name = entry.split(b"\t", 1)
        mode, oid, stage = metadata.split()
        if mode not in (b"100644", b"100755") or stage != b"0":
            raise ValueError("unsupported trusted input type")
        path = Path(root) / os.fsdecode(name)
        try:
            if path.is_symlink():
                raise ValueError("unsupported trusted input type")
            content = path.read_bytes()
        except OSError as error:
            raise ValueError("cannot read trusted input") from error
        digest = hashlib.sha1(b"blob " + str(len(content)).encode() + b"\0" + content).hexdigest().encode()
        if digest != oid:
            raise ValueError("dirty trusted inputs")
