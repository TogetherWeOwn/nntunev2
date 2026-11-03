#!/usr/bin/env python3
"""Require ``renamed_from`` manifest history for renamed migrations.

Staging applies happen pre-merge under the file's then-current name
and are recorded in ``schema_migrations`` under that name (docs/ops.md,
"Staging migrations"). If a PR later renumbers the file, the staging
gate can only recognise the applied row when the manifest entry keeps
the original name+sha in ``renamed_from``. This secretless per-PR
check enforces that; it replaced the PR-body staging declaration,
which nothing verified against the database.

Input is the PR file list as ``filename<TAB>previous_filename`` lines.
Exit 0 = no renames or all recorded, 1 = a rename lacks history,
2 = usage or unreadable input.
"""
import argparse
import pathlib
import re
import sys
import tomllib

MIGRATION_RE = re.compile(r"^migrations/[^/]+\.sql$")


def renames(lines):
    """Yield (old, new) basenames for migration files renamed in the PR."""
    for line in lines:
        parts = line.rstrip("\n").split("\t")
        if len(parts) != 2:
            continue
        new, old = parts[0].strip(), parts[1].strip()
        if old and old != new and MIGRATION_RE.match(old) and MIGRATION_RE.match(new):
            yield old.rsplit("/", 1)[-1], new.rsplit("/", 1)[-1]


def check(lines, manifest_text):
    """Return (ok, message)."""
    pairs = sorted(set(renames(lines)))
    if not pairs:
        return True, "no migration renames; nothing to record"
    try:
        data = tomllib.loads(manifest_text)
    except tomllib.TOMLDecodeError:
        return False, "migrations/manifest.toml does not parse"
    by_file = {}
    for entry in data.get("migration", []):
        if isinstance(entry, dict) and isinstance(entry.get("file"), str):
            by_file[entry["file"]] = {
                alias.get("file")
                for alias in entry.get("renamed_from", [])
                if isinstance(alias, dict)
            }
    missing = []
    for old, new in pairs:
        if old not in by_file.get(new, set()):
            missing.append(f"{old} -> {new}")
    if missing:
        return False, (
            "renamed migrations lack renamed_from history in "
            "migrations/manifest.toml: " + ", ".join(missing) + ". Add "
            "`renamed_from = [{ file = \"<old>\", sha256 = \"<same sha as the entry>\" }]` "
            "to each renamed entry so the staging gate recognises its "
            "applied row. See docs/ops.md (Staging migrations)."
        )
    return True, "renamed_from history present: " + ", ".join(
        f"{old} -> {new}" for old, new in pairs)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--files-file", required=True,
                        help="PR files as filename<TAB>previous_filename lines")
    parser.add_argument("--manifest", default="migrations/manifest.toml")
    args = parser.parse_args(argv)
    try:
        lines = pathlib.Path(args.files_file).read_text(encoding="utf-8").splitlines()
        manifest_text = pathlib.Path(args.manifest).read_text(encoding="utf-8")
    except OSError as err:
        print(f"migrations-staging: cannot read input: {err.strerror}", file=sys.stderr)
        return 2
    ok, message = check(lines, manifest_text)
    print(f"{'::error::' if not ok else ''}migrations-staging: {message}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
