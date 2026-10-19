#!/usr/bin/env python3
"""Dependabot coverage check: every manifest directory must have an entry.

Dependabot takes no globs and each ecosystem looks only in its configured
directory, so a manifest without a matching entry silently gets no
automated security updates (workers/repair shipped exactly this way).
This script derives the expected (ecosystem, directory) set from the repo
itself and fails when `.github/dependabot.yml` does not cover it:

  cargo:  "/" plus every standalone crate listed in the root
          `Cargo.toml` `exclude` that carries its own `Cargo.toml`
          (workspace members resolve to the root lockfile and need no
          entry of their own).
  docker: every directory containing a `Dockerfile`.
  npm:    every directory containing a `package.json`.
  github-actions: "/" (`.github/workflows`).

It also pins the lockfile header comment in `dependabot.yml`: every
`Cargo.lock` on disk (outside build/output dirs) must be named in the
header and negated back into tracking in the root `.gitignore`, so the
header cannot go stale again the way it did when the worker lockfiles
were committed.

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


def standalone_crate_dirs(root):
    """Standalone cargo dirs from the root `Cargo.toml` exclude list."""
    try:
        data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, "root Cargo.toml not found"
    except (tomllib.TOMLDecodeError, OSError) as exc:
        return None, f"root Cargo.toml unreadable: {exc}"
    exclude = data.get("workspace", {}).get("exclude", [])
    if not isinstance(exclude, list):
        return None, "root Cargo.toml workspace.exclude is not a list"
    dirs = set()
    for entry in exclude:
        if not isinstance(entry, str):
            continue
        if (root / entry / "Cargo.toml").is_file():
            dirs.add(normalize_directory(entry))
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
    # Pin the lockfile header: every Cargo.lock must be named in the
    # header comment and negated back into tracking in .gitignore.
    header = header_comment(text)
    locks = tracked_lockfiles(root)
    for lock in locks:
        # Nested lockfiles must be named by full relative path (a bare
        # `Cargo.lock` mention would match every lock's basename and let
        # the header go stale again); the root lock is just `Cargo.lock`.
        named = "Cargo.lock" in header if lock == "Cargo.lock" else lock in header
        if not named:
            drift.append(
                f"lockfile {lock} not named in the dependabot.yml header "
                f"(update the Lockfiles comment)"
            )
    negated = gitignore_negations(root)
    if negated is not None:
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
