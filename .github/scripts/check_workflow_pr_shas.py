#!/usr/bin/env python3
"""Reject added direct PR SHA reads in workflow YAML against the resolved base."""
import argparse
from collections import Counter
import re
import sys

import yaml

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
        for token in yaml.scan(source, Loader=yaml.SafeLoader):
            if isinstance(token, yaml.ScalarToken) and (count := len(RAW_SHA.findall(token.value))):
                reads[path, token.value] += count
    return reads


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--tested", required=True)
    args = parser.parse_args()
    try:
        base = require_commit(".", event_sha(args.base, "comparison base"))
        tested = require_commit(".", event_sha(args.tested, "tested commit"))
        added = workflow_reads(".", tested) - workflow_reads(".", base)
    except (IdentityError, UnicodeError, yaml.YAMLError) as exc:
        print(f"workflow-pr-shas: unable to compare pinned workflow trees ({type(exc).__name__})", file=sys.stderr)
        return 2
    for (path, scalar), count in sorted(added.items()):
        print(f"workflow-pr-shas: {path!r} adds {count} direct PR SHA read(s): {RAW_SHA.findall(scalar)!r}. "
              "Use the ci-commits outputs.", file=sys.stderr)
    return 1 if added else 0


if __name__ == "__main__":
    sys.exit(main())
