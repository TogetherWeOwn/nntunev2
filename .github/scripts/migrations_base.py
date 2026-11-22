#!/usr/bin/env python3
"""Resolve the tested Actions commit, its comparison base and the source head.

PR bases are the actual first parent. Pushes retain event.before even for a
merge tip. Queue runs use the pinned queue base. Dispatch and branch creation
have no prior tip and retain the local main (then origin/main) comparison.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path


class BaseError(Exception):
    """An event/checkout mismatch or unavailable comparison history."""


def git(root, *args):
    try:
        return subprocess.run(
            ["git", "--no-replace-objects", "-C", str(root), *args], check=True,
            capture_output=True, text=True, timeout=20,
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError) as exc:
        detail = exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else str(exc)
        raise BaseError(f"git {' '.join(args)} failed: {detail}") from exc


def event_sha(value, name):
    if not isinstance(value, str) or not re.fullmatch(r"(?!0{40})[0-9a-fA-F]{40}", value):
        raise BaseError(f"{name} must be a nonzero full 40-hex commit SHA")
    return value.lower()


def require_commit(root, sha):
    if git(root, "cat-file", "-t", sha) != "commit":
        raise BaseError(f"{sha} is not a commit object")
    return sha


def event_payload(env):
    try:
        with open(env["GITHUB_EVENT_PATH"], "rb") as stream:
            raw = stream.read(32 * 1024 * 1024 + 1)
        if len(raw) > 32 * 1024 * 1024:
            raise BaseError("Actions event payload exceeds the byte bound")
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise BaseError("Actions event payload must be a JSON object")
        return payload
    except (OSError, KeyError, ValueError, RecursionError) as exc:
        raise BaseError("missing or invalid Actions event payload") from exc


def resolve(root, env):
    event = env.get("GITHUB_EVENT_NAME", "")
    if not event:
        raise BaseError("GITHUB_EVENT_NAME is required; cannot infer comparison semantics")
    tested = event_sha(env.get("GITHUB_SHA"), "GITHUB_SHA")
    if git(root, "rev-parse", "--verify", "HEAD^{commit}") != tested:
        raise BaseError("checkout HEAD does not match GITHUB_SHA")
    if git(root, "rev-parse", "--is-shallow-repository") != "false":
        raise BaseError("shallow checkout: CI comparisons require full history")

    head = tested
    payload = event_payload(env) if event in {"pull_request", "push", "merge_group"} else {}
    if event == "pull_request":
        ref = env.get("GITHUB_REF", "")
        if not re.fullmatch(r"refs/pull/[1-9][0-9]*/merge", ref):
            raise BaseError("pull_request requires a refs/pull/<number>/merge checkout context")
        parents = git(root, "show", "-s", "--format=%P", tested).split()
        if len(parents) != 2:
            raise BaseError(f"PR merge {tested} must have exactly two parents; found {len(parents)}")
        base, head = (require_commit(root, parent) for parent in parents)
        if head != event_sha(payload["pull_request"]["head"]["sha"], "event PR head"):
            raise BaseError("merge second parent does not match event PR head")
    elif event == "merge_group":
        group = payload["merge_group"]
        if tested != event_sha(group["head_sha"], "merge group head"):
            raise BaseError("queue checkout does not match event merge group head")
        base = require_commit(root, event_sha(group["base_sha"], "merge group base"))
        git(root, "merge-base", "--is-ancestor", base, tested)
    elif event == "push" and payload.get("before", "") not in ("", "0" * 40):
        base = require_commit(root, event_sha(payload["before"], "push before"))
    else:
        try:
            base = git(root, "rev-parse", "--verify", "refs/heads/main^{commit}")
        except BaseError:
            base = git(root, "rev-parse", "--verify", "refs/remotes/origin/main^{commit}")
        require_commit(root, base)
    return {"merge_sha": tested, "base_sha": base, "head_sha": head}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    try:
        commits = resolve(args.root, os.environ)
    except (BaseError, KeyError, TypeError) as exc:
        print(f"ci-commits: invalid checkout or event evidence ({exc}). "
              "Use the event checkout with fetch-depth: 0. No commit identities emitted.",
              file=sys.stderr)
        return 2
    print(f"ci-commits: checkout={commits['merge_sha']} base={commits['base_sha']} "
          f"head={commits['head_sha']}", file=sys.stderr)
    for key, sha in commits.items():
        print(f"{key}={sha}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
