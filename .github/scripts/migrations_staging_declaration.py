#!/usr/bin/env python3
"""Require a staging-apply declaration on PRs that touch ``migrations/*.sql``.

Staging migrations are applied out-of-band before merge (docs/ops.md), so a
PR that changes one must say whether that happened. Accepted PR-body lines:

    Staging migration: <files> — applied <YYYY-MM-DD> by <who>
    Staging migration: pending (blocked on <ticket>)

An ``applied`` line must name every changed migration (basename or path);
several lines may split the list. A ``pending`` line covers all of them.
HTML comments are ignored so the template's example never counts.

No secrets and no network: the workflow passes the PR body and the changed
file list as files. Exit 0 = satisfied or no migrations, 1 = missing, 2 = usage.
"""
import argparse
import datetime
import re
import sys

MIGRATION_RE = re.compile(r"^migrations/[^/]+\.sql$")
COMMENT_RE = re.compile(r"<!--.*?-->", re.S)
LINE_RE = re.compile(r"^\s*(?:[-*]\s+)?(?:\[[ xX]\]\s+)?staging migration:\s*(.*?)\s*$", re.I | re.M)
APPLIED_RE = re.compile(
    r"^(?P<files>.+?)\s+(?:—|–|--?)\s+applied\s+(?P<date>\d{4}-\d{2}-\d{2})\s+by\s+(?P<who>\S.*)$",
    re.I,
)
PENDING_RE = re.compile(r"^pending\s*\(\s*blocked on\s+(?P<ticket>[^)\s][^)]*)\)$", re.I)
PENDING_PREFIX = "staging migration pending"


def is_filled(value):
    """Reject unfilled template placeholders such as ``<who>``."""
    return not value.strip().startswith("<")


def is_date(value):
    try:
        datetime.date.fromisoformat(value)
    except ValueError:
        return False
    return True


def changed_migrations(paths):
    return sorted({p.strip() for p in paths if MIGRATION_RE.match(p.strip())})


def parse_declarations(body):
    """Return (pending_tickets, applied_files) from the PR body."""
    pending, applied = [], set()
    for value in LINE_RE.findall(COMMENT_RE.sub("", body or "")):
        if (m := PENDING_RE.match(value)) and is_filled(m["ticket"]):
            pending.append(m["ticket"].strip())
        elif (m := APPLIED_RE.match(value)) and is_date(m["date"]) and is_filled(m["who"]):
            for token in re.split(r"[\s,]+", m["files"]):
                token = token.strip("`'\"")
                if token:
                    applied.add(token)
                    applied.add(token.rsplit("/", 1)[-1])
    return pending, applied


def check(body, paths):
    """Return (ok, message)."""
    migrations = changed_migrations(paths)
    if not migrations:
        return True, "no migrations/*.sql changes; nothing to declare"
    pending, applied = parse_declarations(body)
    if pending:
        return True, f"{PENDING_PREFIX} (blocked on {', '.join(pending)}): {', '.join(migrations)}"
    missing = [m for m in migrations if m not in applied and m.rsplit("/", 1)[-1] not in applied]
    if not missing:
        return True, f"staging migration applied: {', '.join(migrations)}"
    return False, (
        f"PR changes {', '.join(missing)} but the body has no staging declaration for "
        f"{'it' if len(missing) == 1 else 'them'}. Add a line "
        "'Staging migration: <files> — applied <YYYY-MM-DD> by <who>' or "
        "'Staging migration: pending (blocked on <ticket>)'. See docs/ops.md (Staging migrations)."
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--body-file", required=True)
    parser.add_argument("--files-file", required=True, help="changed paths, one per line")
    args = parser.parse_args(argv)
    try:
        with open(args.body_file, encoding="utf-8") as f:
            body = f.read()
        with open(args.files_file, encoding="utf-8") as f:
            paths = f.read().splitlines()
    except OSError as err:
        print(f"migrations-staging: cannot read input: {err.strerror}", file=sys.stderr)
        return 2
    ok, message = check(body, paths)
    # Pending passes but stays visible: docs/ops.md forbids merging it as-is.
    level = "::error::" if not ok else "::warning::" if message.startswith(PENDING_PREFIX) else ""
    print(f"{level}migrations-staging: {message}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
