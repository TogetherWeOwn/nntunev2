#!/usr/bin/env python3
"""Reject added direct PR SHA reads in workflow YAML against the resolved base."""
import argparse
from collections import Counter
from itertools import count
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


def mapping_fields(node):
    fields = {}
    for key, value in node.value:
        if not isinstance(key, yaml.ScalarNode) or key.value in fields:
            raise yaml.YAMLError("workflow keys must be unique scalars")
        fields[key.value] = value
    return fields


def step_scalar(fields, key):
    node = fields.get(key)
    if node is None:
        return ""
    if not isinstance(node, yaml.ScalarNode):
        raise yaml.YAMLError("workflow step identity must use scalar fields")
    return node.value


def scalar_fields(node, location, ancestors, visits):
    # An alias can add a consumer at a new location. Bound the expanded
    # traversal and reject cycles before counting references.
    if next(visits) >= 100_000 or id(node) in ancestors:
        raise yaml.YAMLError("workflow aliases exceed traversal bounds")
    ancestors = ancestors | {id(node)}
    if isinstance(node, yaml.ScalarNode):
        yield location, node.value
    elif isinstance(node, yaml.MappingNode):
        for key, child in mapping_fields(node).items():
            yield from scalar_fields(child, (*location, key), ancestors, visits)
    elif isinstance(node, yaml.SequenceNode):
        for child in node.value:
            fields = mapping_fields(child) if isinstance(child, yaml.MappingNode) else {}
            inputs = fields.get("with")
            destination = step_scalar(mapping_fields(inputs), "path") if isinstance(inputs, yaml.MappingNode) else ""
            step_id = step_scalar(fields, "id")
            action = step_scalar(fields, "uses").partition("@")[0]
            directory = step_scalar(fields, "working-directory")
            role = step_id if "id" in fields else action if "uses" in fields else directory
            step = repr((role, destination))
            yield from scalar_fields(child, (*location, step), ancestors, visits)


def workflow_reads(root, commit):
    reads = Counter()
    entries = git(root, "ls-tree", "-rz", commit, "--", ".github/workflows").split("\0")
    for entry in filter(None, entries):
        header, path = entry.split("\t", 1)
        if not path.endswith((".yml", ".yaml")):
            continue
        mode, kind, oid = header.split()
        if mode not in ("100644", "100755") or kind != "blob":
            raise yaml.YAMLError("workflow path must contain a regular file")
        tree = yaml.compose(git(root, "cat-file", "blob", oid), Loader=yaml.SafeLoader)
        if not isinstance(tree, yaml.MappingNode):
            raise yaml.YAMLError("workflow must contain a mapping")
        for location, scalar in scalar_fields(tree, (), set(), count()):
            reads.update((path, location, match) for match in RAW_SHA.findall(scalar))
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
    except (BaseError, RecursionError, ValueError, yaml.YAMLError) as exc:
        print(f"workflow-pr-shas: unable to compare pinned workflow trees ({type(exc).__name__})", file=sys.stderr)
        return 2
    for (path, _, match), count in sorted(added.items()):
        print(f"workflow-pr-shas: {path!r} adds {count} direct PR SHA read(s): {match!r}. "
              "Use the ci-commits outputs.", file=sys.stderr)
    return 1 if added else 0


if __name__ == "__main__":
    sys.exit(main())
