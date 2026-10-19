#!/usr/bin/env python3
"""Dependabot coverage check: every manifest directory must have an entry.

Dependabot takes no globs and each ecosystem looks only in its configured
directory, so a manifest without a matching entry silently gets no
automated security updates (workers/repair shipped exactly this way).
This script derives the expected (ecosystem, directory) set from the repo
itself and fails when `.github/dependabot.yml` does not cover it:

  cargo:  "/" plus every Cargo.toml directory that is neither the root
          nor a root workspace member (covers both the root `exclude`
          form and the own-`[workspace]` form: a crate with its own
          `[workspace]` table builds fine via `cargo metadata` yet would
          otherwise be invisible to the exclude list alone).
          Workspace members resolve to the root lockfile and need no
          entry of their own.
  docker: every directory containing a `Dockerfile`.
  npm:    every directory containing a `package.json`.
  github-actions: "/" (`.github/workflows`).

It also pins the lockfile header comment in `dependabot.yml`: every
`Cargo.lock` on disk (outside build/output dirs) must be named in the
header and negated back into tracking in the root `.gitignore`, so the
header cannot go stale again the way it did when the worker lockfiles
were committed.

It also pins every external `FROM` in every `Dockerfile` (outside the
same skip dirs) to a `tag@sha256:<64hex>` pin: a digest-less FROM keeps
Dependabot from emitting digest-refresh PRs, and the header comment in
`dependabot.yml` asserts the pins hold, so an unpinned FROM is drift.
Stage refs (`FROM base`, where `base` was named by an earlier
`FROM ... AS base`) are internal and exempt, as is `FROM scratch`
(the empty base image takes no tag and cannot be pinned). The match
is a regex, not literal digests, so Dependabot digest-refresh PRs
stay green.

Stdlib only (no PyYAML: the two fields are extracted with a line scan).
Exit 0 = covered; 1 = drift found; 2 = unusable input.

Usage:
  python3 .github/scripts/check_dependabot_coverage.py [--root DIR]
"""

import argparse
import re
import sys
import tomllib
from pathlib import Path

# Directories never searched for manifests: VCS metadata, dependency
# installs, build output, and local deploy state. A manifest under one of
# these is never a Dependabot directory.
SKIP_DIRS = frozenset({
    ".git", "node_modules", "target", ".wrangler", ".svelte-kit",
    "build", "dist", ".output", "test-results", "playwright-report",
    "__pycache__",
})

ECOSYSTEM_RE = re.compile(
    r"""^[ \t]*-[ \t]*package-ecosystem:[ \t]*["']?([^"'#\s]+)"""
)
DIRECTORY_RE = re.compile(
    r"""^[ \t]*directory:[ \t]*["']?([^"'#\s]+)"""
)
FROM_RE = re.compile(r"^[ \t]*FROM[ \t]+(.+)$", re.IGNORECASE)
# Trailing `AS <stage>` on a FROM line (Docker keywords case-insensitive).
FROM_STAGE_RE = re.compile(
    r"[ \t]+AS[ \t]+([A-Za-z][A-Za-z0-9_.-]*)[ \t]*(#.*)?$", re.IGNORECASE
)
# External image ref with a tag+digest pin: the tag before `@sha256` is
# required, so digest-only refs (`img@sha256:...`) are drift, not pins.
# (`#` cannot appear in a ref, so anything after it is a trailing comment
# and stripped before matching).
FROM_DIGEST_RE = re.compile(r"[^@\s]+:[^/:@\s]+@sha256:[0-9a-f]{64}")


def fail_input(message):
    """Report an unusable input (exit 2): the check itself never ran."""
    print(f"dependabot-coverage: input error: {message}", file=sys.stderr)
    return 2


def normalize_directory(directory):
    """Canonical Dependabot directory form: leading slash, no trailing."""
    directory = directory.strip()
    if not directory.startswith("/"):
        directory = "/" + directory
    if len(directory) > 1:
        directory = directory.rstrip("/")
    return directory


VERSION_RE = re.compile(r"""^[ \t]*version:[ \t]*["']?2["']?[ \t]*(#.*)?$""")
UPDATES_RE = re.compile(r"""^[ \t]*updates:[ \t]*(#.*)?$""")
# Root `Cargo.lock` must match as a word, not as a substring of a nested
# lock path (every nested lock name contains `Cargo.lock`).
ROOT_LOCK_RE = re.compile(r"(?<![/\w])Cargo\.lock")


def validate_dependabot_structure(text):
    """Require the `version: 2` + `updates:` skeleton (exit-2 otherwise).

    The entry extractor is a line scan (no PyYAML), so a structurally
    broken file must not report OK: GitHub's Dependabot app check also
    validates this file, but the coverage gate stays fail-closed on its
    own. Returns an error string, or None when the skeleton is present.
    """
    has_version = any(VERSION_RE.match(line) for line in text.splitlines())
    has_updates = any(UPDATES_RE.match(line) for line in text.splitlines())
    if not has_version:
        return "dependabot.yml is missing 'version: 2'"
    if not has_updates:
        return "dependabot.yml is missing 'updates:'"
    return None


def parse_dependabot_entries(text):
    """Extract the (ecosystem, directory) set from dependabot.yml text."""
    entries = set()
    ecosystem = None
    for line in text.splitlines():
        eco = ECOSYSTEM_RE.match(line)
        if eco:
            ecosystem = eco.group(1).strip().strip("\"'")
            continue
        d = DIRECTORY_RE.match(line)
        if d and ecosystem:
            entries.add((ecosystem, normalize_directory(d.group(1))))
            ecosystem = None
    return entries


def header_comment(text):
    """Leading `#` comment block of dependabot.yml (the lockfile header)."""
    lines = []
    for line in text.splitlines():
        if line.startswith("#"):
            lines.append(line)
        elif not line.strip():
            continue
        else:
            break
    return "\n".join(lines)


def repo_relative(path, root):
    return "/" + path.relative_to(root).as_posix()


def walk_manifests(root, filename):
    """Dirs (as `/`-prefixed Dependabot dirs) holding `filename`."""
    found = set()
    for candidate in root.rglob(filename):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        parent = candidate.parent
        if parent == root:
            found.add("/")
        else:
            found.add("/" + parent.relative_to(root).as_posix())
    return found


GLOB_CHARS = frozenset("*?[]{}!")


def read_root_workspace(root):
    """Parse root `Cargo.toml` workspace members + exclude (input error else)."""
    try:
        data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, "root Cargo.toml not found"
    except (tomllib.TOMLDecodeError, OSError) as exc:
        return None, f"root Cargo.toml unreadable: {exc}"
    workspace = data.get("workspace", {})
    if not isinstance(workspace, dict):
        return None, "root Cargo.toml [workspace] is not a table"
    members = workspace.get("members", [])
    exclude = workspace.get("exclude", [])
    if not isinstance(members, list):
        return None, "root Cargo.toml workspace.members is not a list"
    if not isinstance(exclude, list):
        return None, "root Cargo.toml workspace.exclude is not a list"
    for entry in members:
        if isinstance(entry, str) and any(c in GLOB_CHARS for c in entry):
            return None, (
                f"root Cargo.toml workspace.members contains a glob {entry!r}: "
                "expand it to literal paths so the coverage check can resolve it"
            )
    return {"members": members, "exclude": exclude}, None


def standalone_crate_dirs(root):
    """Standalone cargo dirs: exclude list plus non-member manifest dirs.

    The exclude list alone misses the own-`[workspace]` form (a crate with
    its own `[workspace]` table builds fine yet is not in `exclude`), so
    every `Cargo.toml` directory that is neither the root nor a workspace
    member is also expected to have its own cargo entry.
    """
    workspace, err = read_root_workspace(root)
    if err is not None:
        return None, err
    dirs = set()
    for entry in workspace["exclude"]:
        if not isinstance(entry, str):
            continue
        if (root / entry / "Cargo.toml").is_file():
            dirs.add(normalize_directory(entry))
    members = set()
    for entry in workspace["members"]:
        if isinstance(entry, str):
            members.add(normalize_directory(entry))
    for manifest_dir in walk_manifests(root, "Cargo.toml"):
        if manifest_dir == "/":
            continue
        if manifest_dir in members:
            continue
        dirs.add(manifest_dir)
    return dirs, None


def expected_entries(root):
    """Derive the expected (ecosystem, directory) set from the repo."""
    standalone, err = standalone_crate_dirs(root)
    if err is not None:
        return None, err
    expected = {("cargo", "/")}
    expected |= {("cargo", d) for d in standalone}
    expected |= {("docker", d) for d in walk_manifests(root, "Dockerfile")}
    expected |= {("npm", d) for d in walk_manifests(root, "package.json")}
    if (root / ".github" / "workflows").is_dir():
        expected.add(("github-actions", "/"))
    return expected, None


def walk_dockerfiles(root):
    """All `Dockerfile` paths, relative POSIX form, outside SKIP_DIRS."""
    found = []
    for candidate in root.rglob("Dockerfile"):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        found.append(rel.as_posix())
    return sorted(found)


def unpinned_from_refs(text):
    """`(lineno, ref)` for each external FROM lacking a tag+digest pin.

    Stage refs are exempt: a FROM whose ref was named by an earlier
    `FROM ... AS <stage>` in the same file is internal (stage names match
    case-insensitively, like Docker itself). `FROM scratch` is exempt: the
    empty base image takes no tag and cannot be pinned. Leading
    `--platform=...` flags are skipped to reach the ref. An empty or
    unparseable ref is reported, not skipped: the gate stays fail-closed.
    """
    problems = []
    stages = set()
    for lineno, line in enumerate(text.splitlines(), start=1):
        match = FROM_RE.match(line)
        if not match:
            continue
        rest = match.group(1).strip()
        staged = FROM_STAGE_RE.search(rest)
        stage_name = None
        if staged:
            stage_name = staged.group(1).lower()
            rest = rest[:staged.start()].strip()
        tokens = rest.split()
        ref = next((t for t in tokens if not t.startswith("--")), "")
        ref = ref.split("#", 1)[0].strip()
        if not ref:
            problems.append((lineno, line.strip()))
        elif ref.lower() != "scratch" and ref.lower() not in stages:
            if not FROM_DIGEST_RE.fullmatch(ref):
                problems.append((lineno, ref))
        if stage_name is not None:
            # Docker defines the name for later lines only: recording it
            # before the check would let `FROM debian AS debian` exempt
            # its own floating ref.
            stages.add(stage_name)
    return problems


def tracked_lockfiles(root):
    """All `Cargo.lock` paths, relative POSIX form, outside SKIP_DIRS."""
    locks = []
    for candidate in root.rglob("Cargo.lock"):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        locks.append(rel.as_posix())
    return sorted(locks)


def gitignore_negations(root):
    """Set of `!`-negated paths from the root `.gitignore`."""
    try:
        text = (root / ".gitignore").read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    negated = set()
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("!"):
            negated.add(line[1:].lstrip("/"))
    return negated


def check_coverage(root):
    """Full check; returns a list of drift strings (empty when covered)."""
    try:
        text = (root / ".github" / "dependabot.yml").read_text(encoding="utf-8")
    except FileNotFoundError:
        return None, "dependabot.yml not found: .github/dependabot.yml"
    except OSError as exc:
        return None, f"dependabot.yml unreadable: {exc}"
    struct_err = validate_dependabot_structure(text)
    if struct_err is not None:
        return None, struct_err
    actual = parse_dependabot_entries(text)
    expected, err = expected_entries(root)
    if err is not None:
        return None, err
    drift = []
    for ecosystem, directory in sorted(expected):
        if (ecosystem, directory) not in actual:
            drift.append(
                f"missing {ecosystem} entry for directory {directory!r} "
                f"(add it to .github/dependabot.yml)"
            )
    # Every cargo entry must point at a real manifest directory.
    for ecosystem, directory in sorted(actual):
        if ecosystem != "cargo":
            continue
        if directory == "/":
            continue
        if not (root / directory.lstrip("/") / "Cargo.toml").is_file():
            drift.append(
                f"stale cargo entry for directory {directory!r} "
                f"(no Cargo.toml there)"
            )
    # Pin the FROM lines: every external FROM must carry a tag+digest pin
    # so Dependabot emits digest-refresh PRs (see the dependabot.yml header).
    for dockerfile in walk_dockerfiles(root):
        try:
            docker_text = (root / dockerfile).read_text(encoding="utf-8")
        except OSError as exc:
            return None, f"{dockerfile} unreadable: {exc}"
        for lineno, ref in unpinned_from_refs(docker_text):
            drift.append(
                f"{dockerfile}:{lineno}: external FROM {ref!r} lacks a "
                f"tag@sha256 pin (Dependabot cannot track it)"
            )
    # Pin the lockfile header: every Cargo.lock must be named in the
    # header comment and negated back into tracking in .gitignore.
    header = header_comment(text)
    locks = tracked_lockfiles(root)
    for lock in locks:
        # Nested lockfiles must be named by full relative path (a bare
        # `Cargo.lock` mention would match every lock's basename and let
        # the header go stale again); the root lock matches as a word so a
        # header naming only `workers/api/Cargo.lock` does not satisfy it.
        if lock == "Cargo.lock":
            named = ROOT_LOCK_RE.search(header) is not None
        else:
            named = lock in header
        if not named:
            drift.append(
                f"lockfile {lock} not named in the dependabot.yml header "
                f"(update the Lockfiles comment)"
            )
    negated = gitignore_negations(root)
    if negated is None:
        return None, "root .gitignore not found"
    for lock in locks:
        if lock not in negated:
            drift.append(
                f"lockfile {lock} not negated back into tracking in "
                f".gitignore (add '!/{lock}')"
            )
    # Every cargo-covered lockfile directory needs its own entry, except
    # the root: workspace members resolve to the root lockfile.
    for lock in locks:
        if lock == "Cargo.lock":
            continue
        lock_dir = "/" + lock.rsplit("/", 1)[0]
        if ("cargo", lock_dir) not in actual:
            drift.append(
                f"tracked lockfile {lock} has no cargo entry for {lock_dir!r}"
            )
    return drift, None


def main(argv):
    parser = argparse.ArgumentParser(
        description="Fail when a manifest directory lacks a Dependabot entry."
    )
    parser.add_argument("--root", default=".")
    args = parser.parse_args(argv)
    root = Path(args.root)
    drift, err = check_coverage(root)
    if err is not None:
        return fail_input(err)
    if drift:
        for item in drift:
            print(f"dependabot-coverage: drift: {item}")
        print(f"dependabot-coverage: DRIFT ({len(drift)} problem(s))")
        return 1
    print("dependabot-coverage: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
