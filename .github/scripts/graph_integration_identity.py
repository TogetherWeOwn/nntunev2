"""Inert local integration identity primitive; not a qualification or fetcher.

The caller owns trusted inputs and must use a disposable checkout. This helper
neither validates pull request metadata nor establishes control provenance.
"""

from pathlib import Path
import subprocess

from graph_object_guard import git_environment, guard
from graph_qualification_context import require_sha


def _git(root, *args, input_data=None):
    result = subprocess.run(
        ["git", "--no-lazy-fetch", "--no-replace-objects", "-C", str(root),
         "--work-tree", str(root), "-c", "core.hooksPath=/dev/null",
         "-c", "core.fsmonitor=false", *args],
        input=input_data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=git_environment(), check=False,
    )
    if result.returncode:
        raise ValueError("integration Git command failed: " + result.stderr.decode(errors="replace"))
    return result.stdout.decode("ascii").strip()


def verify(root, base, head, tree, merge):
    """Read back the exact graph and detached checkout; reject any mismatch."""
    for oid in (base, head, tree, merge):
        require_sha(oid)
    if _git(root, "cat-file", "-t", merge) != "commit":
        raise ValueError("integration object is not a commit")
    headers = _git(root, "cat-file", "-p", merge).split("\n\n", 1)[0].splitlines()
    trees = [line[5:] for line in headers if line.startswith("tree ")]
    parents = [line[7:] for line in headers if line.startswith("parent ")]
    if trees != [tree] or parents != [base, head]:
        raise ValueError("integration tree or ordered parents mismatch")
    if _git(root, "rev-parse", "--verify", "HEAD") != merge:
        raise ValueError("checkout is not the integration commit")
    if _git(root, "rev-parse", "--abbrev-ref", "HEAD") != "HEAD":
        raise ValueError("checkout is not detached")
    guard(root, [merge], ["."])


def construct(root, base, head, trusted_paths):
    """Build and check out a local-only commit in a disposable repository."""
    root = Path(root).resolve()
    require_sha(base)
    require_sha(head)
    if not trusted_paths:
        raise ValueError("expected trusted input paths")
    guard(root, [base, head], [*trusted_paths, "."])
    tree = _git(root, "merge-tree", "--write-tree", base, head)
    require_sha(tree)
    merge = _git(root, "-c", "user.name=Graph integration",
                 "-c", "user.email=graph-integration@example.invalid",
                 "commit-tree", tree, "-p", base, "-p", head,
                 input_data=b"Ephemeral integration\n")
    require_sha(merge)
    _git(root, "checkout", "--detach", merge)
    verify(root, base, head, tree, merge)
    return tree, merge
