#!/usr/bin/env python3
"""Validate migration numbering, manifest metadata, checksums, and drift."""

import argparse
import hashlib
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path


MANIFEST = Path("migrations/manifest.toml")
MIGRATIONS = Path("migrations")
NUMBERED_SQL = re.compile(r"^(\d{4})_.+\.sql$")
NUMBER_PREFIX = re.compile(r"^(\d{4})_")
VALID_TARGETS = {"app", "mirror"}


class GateError(Exception):
    """A usage, repository, or manifest parsing error."""


def git(root, *args, text=True):
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            check=True,
            capture_output=True,
            text=text,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else str(exc)
        if isinstance(detail, bytes):
            detail = detail.decode("utf-8", errors="replace")
        raise GateError(f"git {' '.join(args)} failed: {detail}") from exc
    return result.stdout


def report(code, filename, detail):
    print(f"migrations-gate: {code}: {filename}: {detail}")


def load_manifest(root):
    path = root / MANIFEST
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise GateError(f"cannot read {MANIFEST}: {exc}") from exc
    entries = data.get("migration", [])
    if not isinstance(entries, list) or any(not isinstance(item, dict) for item in entries):
        raise GateError(f"{MANIFEST} must contain [[migration]] tables")
    for index, entry in enumerate(entries, start=1):
        if not all(key in entry for key in ("file", "target", "sha256")):
            raise GateError(f"{MANIFEST} entry {index} requires file, target, and sha256")
        if not all(isinstance(entry[key], str) for key in ("file", "target", "sha256")):
            raise GateError(f"{MANIFEST} entry {index} fields must be strings")
        filename = entry["file"]
        if Path(filename).name != filename or filename in {".", ".."}:
            raise GateError(f"{MANIFEST} entry {index} file must be a migration filename")
    return entries


def check_manifest(root):
    entries = load_manifest(root)
    migrations_dir = root / MIGRATIONS
    try:
        sql_paths = [
            path for path in migrations_dir.rglob("*")
            if path.is_file() and path.suffix.lower() == ".sql"
        ]
        symlink_paths = [path for path in migrations_dir.rglob("*") if path.is_symlink()]
    except OSError as exc:
        raise GateError(f"cannot list {MIGRATIONS}: {exc}") from exc
    symlink_files = {path.relative_to(migrations_dir).as_posix() for path in symlink_paths}
    sql_files = {
        path.relative_to(migrations_dir).as_posix()
        for path in sql_paths
        if not path.is_symlink()
    }

    manifest_files = set()
    numbers = {}
    previous_number = None
    violations = 0
    for filename in sorted(symlink_files):
        report("symlink", filename, "migration files must not be symlinks")
        violations += 1
    for entry in entries:
        filename = entry["file"]
        if filename in manifest_files:
            report("duplicate-entry", filename, "migration is listed more than once")
            violations += 1
            continue
        manifest_files.add(filename)

        match = NUMBERED_SQL.fullmatch(filename)
        if not match:
            report("out-of-order", filename, "filename must start with a four-digit number and underscore")
            violations += 1
        else:
            number = int(match.group(1))
            if number in numbers:
                report("duplicate-number", filename, f"number {number:04d} is already used by {numbers[number]}")
                violations += 1
            else:
                numbers[number] = filename
            if previous_number is not None and number < previous_number:
                report("out-of-order", filename, "manifest numbers must be strictly increasing")
                violations += 1
            previous_number = number

        if entry["target"] not in VALID_TARGETS:
            report("bad-target", filename, "target must be app or mirror")
            violations += 1

        path = migrations_dir / filename
        if path.is_symlink():
            continue
        if not path.is_file():
            report("missing-file", filename, "manifest entry has no matching SQL file")
            violations += 1
            continue
        try:
            checksum = hashlib.sha256(path.read_bytes()).hexdigest()
        except OSError as exc:
            raise GateError(f"cannot read {path}: {exc}") from exc
        if not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]) or entry["sha256"] != checksum:
            report("checksum", filename, "sha256 does not match the migration contents")
            violations += 1

    for filename in sorted(sql_files - manifest_files):
        report("unlisted", filename, "SQL migration is missing from the manifest")
        violations += 1
    return violations


def check_drift(root, base_ref):
    try:
        base_commit = git(root, "rev-parse", "--verify", f"{base_ref}^{{commit}}").strip()
    except GateError:
        if base_ref != "main":
            raise
        base_commit = git(root, "rev-parse", "--verify", "origin/main^{commit}").strip()

    base_files = [
        os.fsdecode(path)
        for path in git(root, "ls-tree", "-r", "-z", "--name-only", base_commit, "--", "migrations", text=False).split(b"\0")
        if path
    ]
    base_sql = {
        path: path
        for path in base_files
        if path.startswith("migrations/") and Path(path).suffix.lower() == ".sql"
    }
    base_numbers = [
        int(match.group(1))
        for path in base_sql
        if Path(path).parent == Path("migrations")
        and (match := NUMBER_PREFIX.match(Path(path).name))
    ]
    base_max = max(base_numbers, default=0)
    base_flat_files = {
        Path(path).name for path in base_sql if Path(path).parent == Path("migrations")
    }
    current_entries = {entry["file"]: entry for entry in load_manifest(root)}

    base_manifest = {}
    has_base_manifest = "migrations/manifest.toml" in base_files
    if has_base_manifest:
        try:
            base_data = tomllib.loads(git(root, "show", f"{base_commit}:migrations/manifest.toml"))
        except (UnicodeError, tomllib.TOMLDecodeError) as exc:
            raise GateError(f"cannot parse base migrations/manifest.toml: {exc}") from exc
        base_entries = base_data.get("migration", [])
        if not isinstance(base_entries, list) or any(not isinstance(entry, dict) for entry in base_entries):
            raise GateError("base migrations/manifest.toml must contain [[migration]] tables")
        for index, entry in enumerate(base_entries, start=1):
            if not all(isinstance(entry.get(key), str) for key in ("file", "target", "sha256")):
                raise GateError(f"base migrations/manifest.toml entry {index} requires string file, target, and sha256")
        base_manifest = {entry["file"]: entry for entry in base_entries}

    violations = 0
    new_numbers = sorted(
        (int(match.group(1)), filename)
        for filename in current_entries
        if filename not in base_flat_files and (match := NUMBER_PREFIX.match(filename))
    )
    for offset, (number, filename) in enumerate(new_numbers, start=1):
        expected = base_max + offset
        if number != expected:
            report("out-of-order", filename, f"new migration number must be {expected:04d}")
            violations += 1

    for path in sorted(base_sql):
        filename = Path(path).name
        current = root / path
        if current.is_symlink():
            report("applied-edited", filename, "migration is a symlink instead of a regular file")
            violations += 1
            continue
        if not current.is_file():
            report("applied-removed", filename, f"migration exists on base {base_ref} but is missing here")
            violations += 1
            continue
        original = git(root, "show", f"{base_commit}:{path}", text=False)
        try:
            contents = current.read_bytes()
        except OSError as exc:
            raise GateError(f"cannot read {current}: {exc}") from exc
        contents_changed = contents != original
        if contents_changed:
            report("applied-edited", filename, f"migration contents differ from base {base_ref}")
            violations += 1
        if has_base_manifest and Path(path).parent == Path("migrations"):
            previous = base_manifest.get(filename)
            current_entry = current_entries.get(filename)
            manifest_changed = (previous is None) != (current_entry is None)
            if previous is not None and current_entry is not None:
                manifest_changed = manifest_changed or (
                    previous.get("target") != current_entry.get("target")
                    or (not contents_changed and previous.get("sha256") != current_entry.get("sha256"))
                )
            if manifest_changed:
                report("applied-edited", filename, f"manifest target or checksum differs from base {base_ref}")
                violations += 1
    return violations


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--base-ref")
    try:
        args = parser.parse_args(argv)
        root = args.root.resolve()
        if not root.is_dir():
            raise GateError(f"repository root is not a directory: {root}")
        violations = check_manifest(root)
        if args.base_ref:
            violations += check_drift(root, args.base_ref)
        return 1 if violations else 0
    except GateError as exc:
        print(f"migrations-gate: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
