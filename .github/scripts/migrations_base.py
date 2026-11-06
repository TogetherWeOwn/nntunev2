#!/usr/bin/env python3
"""Pair the migration gate's comparison base with the GitHub Actions checkout.

PRs test the integration tree, so its first parent is the only paired base.
Pushes retain event.before, even when the pushed tip is itself a merge.
Dispatch, merge_group and zero/absent-before pushes retain the historical
main (then origin/main) comparison; on main this may be a self-comparison,
but full manifest/checksum validation still runs. This is not a PR fallback.
"""

import argparse
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
            ["git", "-C", str(root), *args], check=True,
            capture_output=True, text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else str(exc)
        raise BaseError(f"git {' '.join(args)} failed: {detail}") from exc


def event_sha(value, name):
    if not re.fullmatch(r"[0-9a-fA-F]{40}", value) or value == "0" * 40:
        raise BaseError(f"{name} must be a nonzero full 40-hex commit SHA")
    return value.lower()


def require_commit(root, sha):
    if git(root, "cat-file", "-t", sha) != "commit":
        raise BaseError(f"{sha} is not a commit object")
    return sha


def select_base(root, env):
    head = git(root, "rev-parse", "--verify", "HEAD^{commit}")
    if git(root, "rev-parse", "--is-shallow-repository") != "false":
        raise BaseError("shallow checkout: migration comparisons require full history")
    event = env.get("GITHUB_EVENT_NAME", "")
    if not event:
        raise BaseError("GITHUB_EVENT_NAME is required; cannot infer comparison semantics")
    if event == "pull_request":
        ref = env.get("GITHUB_REF", "")
        if not re.fullmatch(r"refs/pull/[1-9][0-9]*/merge", ref):
            raise BaseError("pull_request requires a refs/pull/<number>/merge checkout context")
        expected = event_sha(env.get("GITHUB_SHA", ""), "GITHUB_SHA")
        if head != expected:
            raise BaseError(f"checkout HEAD {head} does not match event merge SHA {expected}")
        parents = git(root, "show", "-s", "--format=%P", head).split()
        if len(parents) != 2:
            raise BaseError(f"PR merge {head} must have exactly two parents; found {len(parents)}")
        for parent in parents:
            require_commit(root, parent)
        pr_head = env.get("MIGRATIONS_PR_HEAD_SHA", "")
        if pr_head and parents[1] != event_sha(pr_head, "MIGRATIONS_PR_HEAD_SHA"):
            raise BaseError(f"merge second parent {parents[1]} does not match event PR head {pr_head}")
        base, reason = parents[0], "PR integration first parent"
    elif event == "push" and env.get("MIGRATIONS_PUSH_BEFORE", "") not in ("", "0" * 40):
        base = require_commit(root, event_sha(env["MIGRATIONS_PUSH_BEFORE"], "MIGRATIONS_PUSH_BEFORE"))
        reason = "push event.before"
    else:
        # Preserve the existing non-PR/zero-before policy, resolving it once
        # to an immutable SHA. Never reach this branch from a malformed PR.
        try:
            base = git(root, "rev-parse", "--verify", "main^{commit}")
        except BaseError:
            base = git(root, "rev-parse", "--verify", "origin/main^{commit}")
        require_commit(root, base)
        reason = f"{event} legacy main comparison (no nonzero push-before)"
    print(f"migrations-base: checkout={head} base={base} ({reason})", file=sys.stderr)
    return base


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    try:
        base = select_base(args.root, os.environ)
        print(f"base_ref={base}")
        return 0
    except BaseError as exc:
        print(
            f"migrations-base: {exc}. Use actions/checkout with fetch-depth: 0 "
            "and the event's default checkout; verify the event SHA/head inputs. "
            "No comparison base was selected.", file=sys.stderr,
        )
        return 2


if __name__ == "__main__":
    sys.exit(main())
