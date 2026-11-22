#!/usr/bin/env python3
"""Reject added direct PR SHA reads in workflow YAML against the resolved base."""
import argparse
from collections import Counter
import re
import sys

import yaml

from migrations_base import BaseError, event_sha, git, require_commit


def property_read(name):
    return rf"""\s*(?:\.\s*{name}\b|\[\s*(?:'{name}'|"{name}")\s*\])"""


# GitHub accepts case-insensitive property names and mixed dot/index syntax.
RAW_SHA = re.compile(r"\bgithub" + "".join(
    property_read(name) for name in ("event", "pull_request", "(?:base|head)", "sha")
), re.IGNORECASE)


def scalar_fields(node, location, seen):
    if id(node) in seen:  # A YAML alias is a reference to an already visited node.
        return
    seen.add(id(node))
    if isinstance(node, yaml.ScalarNode):
        yield location, node.value
    elif isinstance(node, yaml.MappingNode):
        for key, child in node.value:
            if not isinstance(key, yaml.ScalarNode):
                raise yaml.YAMLError("workflow keys must be scalars")
            yield from scalar_fields(child, (*location, key.value), seen)
    elif isinstance(node, yaml.SequenceNode):
        for index, child in enumerate(node.value):
            label = next((value.value for key, value in child.value
                          if key.value in ("id", "name") and isinstance(value, yaml.ScalarNode)),
                         str(index)) if isinstance(child, yaml.MappingNode) else str(index)
            yield from scalar_fields(child, (*location, f"[{label}]"), seen)


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
        tree = yaml.compose(source, Loader=yaml.SafeLoader)
        for location, scalar in scalar_fields(tree, (), set()):
            if count := len(RAW_SHA.findall(scalar)):
                reads[path, location, scalar] += count
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
    except (BaseError, RecursionError, UnicodeError, yaml.YAMLError) as exc:
        print(f"workflow-pr-shas: unable to compare pinned workflow trees ({type(exc).__name__})", file=sys.stderr)
        return 2
    for (path, _, scalar), count in sorted(added.items()):
        print(f"workflow-pr-shas: {path!r} adds {count} direct PR SHA read(s): {RAW_SHA.findall(scalar)!r}. "
              "Use the ci-commits outputs.", file=sys.stderr)
    return 1 if added else 0


if __name__ == "__main__":
    sys.exit(main())
