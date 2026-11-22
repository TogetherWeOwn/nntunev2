#!/usr/bin/env python3
"""Reject added direct PR SHA reads in workflow YAML against the resolved base."""
import argparse
from collections import Counter
from pathlib import Path
import re
import sys

from migrations_base import IdentityError, event_sha, git, require_commit


def property_read(name):
    return rf"""\s*(?:\.\s*{name}\b|\[\s*(?:'{name}'|"{name}")\s*\])"""


# GitHub accepts case-insensitive property names and mixed dot/index syntax.
RAW_SHA = re.compile(r"\bgithub" + "".join(
    property_read(name) for name in ("event", "pull_request", "(?:base|head)", "sha")
), re.IGNORECASE)


def workflow_reads(root, commit):
    reads = Counter()
    for entry in git(root, "ls-tree", "-rz", commit, "--", ".github/workflows").split("\0"):
        if not entry:
            continue
        header, path = entry.split("\t", 1)
        if not path.endswith((".yml", ".yaml")):
            continue
        _, _, oid = header.split()
        source = git(root, "cat-file", "blob", oid)
        for match in RAW_SHA.finditer(source):
            start = source.rfind("\n", 0, match.start()) + 1
            end = source.find("\n", match.end())
            line = source[start:end if end >= 0 else len(source)].strip()
            if not line.startswith("#") or "${{" in line:
                reads[path, line] += 1
    return reads


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--tested", required=True)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        base = require_commit(args.root, event_sha(args.base, "comparison base"))
        tested = require_commit(args.root, event_sha(args.tested, "tested commit"))
        added = workflow_reads(args.root, tested) - workflow_reads(args.root, base)
    except (IdentityError, UnicodeError) as exc:
        print(f"workflow-pr-shas: unable to compare pinned workflow trees: {exc}", file=sys.stderr)
        return 2
    for (path, line), count in sorted(added.items()):
        print(f"workflow-pr-shas: {path!r} adds {count} direct PR SHA read(s): {line!r}",
              file=sys.stderr)
    if added:
        print("workflow-pr-shas: use the pinned ci-commits outputs.", file=sys.stderr)
        return 1
    print(f"workflow-pr-shas: no added direct PR SHA reads ({base}..{tested})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
