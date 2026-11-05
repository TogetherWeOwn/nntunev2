#!/usr/bin/env python3
"""Apply one migration and record it in ``public.schema_migrations``.

This is the migrator path for staging and production (docs/ops.md,
"Staging migrations"). It refuses to apply a file whose bytes do not
match ``migrations/manifest.toml`` (current name+sha, or a
``renamed_from`` original when applying a pre-merge branch file), and
runs the already-applied check, the migration and the tracking insert
inside one advisory-locked transaction, so a database can never drift
from its record and two concurrent applies cannot both execute. The
row keeps the name the file was applied under; later renames are
recognised through the manifest, never by rewriting history.

Usage:
    DATABASE_URL=postgresql://... \\
    python3 .github/scripts/apply_migration.py \\
        --file migrations/0032_example.sql --applied-by "operator" \\
        --expect-database nntunev2_staging

``--expect-database`` is mandatory: the connected database must match
it both before connecting (against DATABASE_URL) and inside the apply
transaction (``current_database()``), for the DDL step, normal applies
and ``--record-only`` alike — a migrator URL pointed at the wrong
database must never create the tracking table, run migration DDL, or
record false history there.

Files carrying their own top-level transaction control (or psql
meta-commands, which can escape the transaction) cannot join the
tracking transaction; apply those by hand, then re-run with
``--record-only``. ``--dry-run`` prints the composed SQL without
touching a database.
"""
import argparse
import hashlib
import os
import pathlib
import posixpath
import re
import subprocess
import sys
import urllib.parse

import migration_history

# The tracking table's rows are deploy proof for the staging gate, so
# its write privileges are pinned here, in the one DDL constant shared
# by the migrator and the committed backfill files: no role other than
# the table owner (the migrator role that creates it) may hold a write
# privilege — on the table (``pg_class.relacl``) or on any column
# (``pg_attribute.attacl``; column-level INSERT alone can forge a
# tracking row, and it never appears in ``relacl``) — whatever the
# database's default privileges granted at CREATE time. The revoke
# pass runs on every DDL execution, so a later stray GRANT is removed
# by the next apply; the staging gate also fails closed while such a
# grant exists. Read privileges are untouched.
DDL = """CREATE TABLE IF NOT EXISTS public.schema_migrations (
    file       TEXT PRIMARY KEY,
    sha256     TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_by TEXT NOT NULL
);
REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER
    ON TABLE public.schema_migrations FROM PUBLIC;
DO $privileges$
DECLARE
    grant_row record;
BEGIN
    FOR grant_row IN
        SELECT DISTINCT a.grantee
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        CROSS JOIN aclexplode(c.relacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN
              ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE 'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                 || 'ON TABLE public.schema_migrations FROM PUBLIC';
        ELSE
            EXECUTE format(
                'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                'ON TABLE public.schema_migrations FROM %I',
                pg_get_userbyid(grant_row.grantee));
        END IF;
    END LOOP;
    -- Column privileges live in pg_attribute.attacl, not relacl, so the
    -- table-level pass above cannot see them; revoke per column. Only
    -- INSERT, UPDATE and REFERENCES exist as column-level writes.
    FOR grant_row IN
        SELECT DISTINCT a.grantee, t.attname
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_attribute t ON t.attrelid = c.oid
            AND t.attnum > 0 AND NOT t.attisdropped
        CROSS JOIN aclexplode(t.attacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN ('INSERT', 'UPDATE', 'REFERENCES')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM PUBLIC',
                grant_row.attname, grant_row.attname, grant_row.attname);
        ELSE
            EXECUTE format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM %I',
                grant_row.attname, grant_row.attname, grant_row.attname,
                pg_get_userbyid(grant_row.grantee));
        END IF;
    END LOOP;
END
$privileges$;"""

# Serializes every apply against this database (see the guard below).
ADVISORY_LOCK_KEY = 7271901
ALREADY_SAME = "schema-migrations: already recorded"
ALREADY_DIFFERENT = "schema-migrations: recorded with a different sha256"
WRONG_DATABASE = "schema-migrations: unexpected database"
MISSING_PRIOR = "schema-migrations: earlier migration not recorded"
CONFLICTING_HISTORY = "schema-migrations: conflicting recorded history"

FILE_RE = re.compile(r"^\d{4}_[A-Za-z0-9_]+\.sql$")
DATABASE_RE = re.compile(r"^[A-Za-z0-9_]+$")
TXN_CONTROL_RE = re.compile(
    r"\b(BEGIN|COMMIT|ROLLBACK)(\s+(WORK|TRANSACTION))?\s*;|\bSTART\s+TRANSACTION\b",
    re.I,
)
PSQL_META_RE = re.compile(r"^\s*\\[A-Za-z!]", re.M)
DOLLAR_QUOTE_RE = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$")


class ApplyError(Exception):
    pass


def strip_hidden_sql(text):
    """Blank dollar-quoted bodies, strings and comments.

    What remains is top-level SQL, so transaction control can be told
    apart from PL/pgSQL blocks (``BEGIN`` inside ``$$ ... $$``) and from
    words mentioned in comments or literals.
    """
    out, i, n = [], 0, len(text)
    while i < n:
        if text.startswith("--", i):
            end = text.find("\n", i)
            i = n if end == -1 else end
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            i = n if end == -1 else end + 2
        elif text[i] == "'":
            i += 1
            while i < n:
                if text[i] == "'":
                    if text.startswith("''", i):
                        i += 2
                        continue
                    i += 1
                    break
                i += 1
            out.append("''")
        elif match := DOLLAR_QUOTE_RE.match(text, i):
            end = text.find(match.group(0), match.end())
            i = n if end == -1 else end + len(match.group(0))
            out.append("$$ $$")
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def has_toplevel_txn_control(body):
    stripped = strip_hidden_sql(body)
    return bool(TXN_CONTROL_RE.search(stripped) or PSQL_META_RE.search(stripped))


def load_manifest(manifest_path):
    try:
        return migration_history.parse_manifest(migration_history.read_input(manifest_path))
    except migration_history.HistoryError as exc:
        raise ApplyError(f"cannot read manifest: {exc}") from exc


def validate_entries(entries):
    try:
        return migration_history.validate_entries(entries)
    except migration_history.HistoryError as exc:
        raise ApplyError(f"invalid manifest: {exc}") from exc


def entry_identity(entry):
    identity = [(entry["file"], entry["sha256"])]
    identity += [(alias["file"], alias["sha256"])
                 for alias in entry.get("renamed_from", [])]
    return identity


def resolve_entry(entries, filename, sha256):
    """Return (target, identity, priors) for an applied-under name, or raise.

    Identity is every (file, sha256) pair the entry may be recorded
    under: its current name plus each ``renamed_from`` original.
    Priors are the identities of every earlier same-target manifest
    entry; the guard requires each of them to be recorded already, so
    an apply can never skip or reorder history (the mirror target has
    no downstream gate that would catch a gap).
    """
    validate_entries(entries)
    for index, entry in enumerate(entries):
        identity = entry_identity(entry)
        if dict(identity).get(filename) is not None:
            if dict(identity)[filename] != sha256:
                raise ApplyError(
                    f"{filename} sha256 {sha256} does not match the manifest "
                    f"{dict(identity)[filename]}"
                )
            priors = [entry_identity(earlier) for earlier in entries[:index]
                      if earlier["target"] == entry["target"]]
            return entry["target"], identity, priors
    raise ApplyError(f"{filename} is not listed in the manifest")


def quote_literal(value, label="applied-by"):
    if any(ord(ch) < 0x20 for ch in value) or not value.strip():
        raise ApplyError(f"{label} must be non-empty printable text")
    return "'" + value.replace("'", "''") + "'"


def validate_expected_database(expected_database):
    if not isinstance(expected_database, str) \
            or not DATABASE_RE.match(expected_database):
        raise ApplyError(
            "--expect-database must be a plain database name ([A-Za-z0-9_]+)")
    return expected_database


def insert_sql(filename, sha256, applied_by, applied_at=None, on_conflict=True):
    # One value per line: the committed backfill files use this too, and
    # a public checksum sharing a line with an auth-flavoured filename
    # false-positives secret scanners as a credential pair.
    when = f"'{applied_at}'::timestamptz" if applied_at else "now()"
    conflict = "\nON CONFLICT (file) DO NOTHING" if on_conflict else ""
    return (
        "INSERT INTO public.schema_migrations (file, sha256, applied_at, applied_by)\n"
        "VALUES (\n"
        f"    {quote_literal(filename)},\n"
        f"    '{sha256}',\n"
        f"    {when},\n"
        f"    {quote_literal(applied_by)}\n"
        f"){conflict};"
    )


def database_guard_sql(expected_database):
    """Abort if the connected database is not the expected one.

    Runs inside whichever transaction follows it, so a mismatch leaves
    nothing behind — not even the tracking table from the DDL step.
    """
    return f"""DO $dbguard$
BEGIN
    IF current_database() <> {quote_literal(expected_database, "expect-database")} THEN
        RAISE EXCEPTION '{WRONG_DATABASE}';
    END IF;
END
$dbguard$;"""


def ddl_sql(expected_database):
    return database_guard_sql(expected_database) + "\n" + DDL + "\n"


def history_names_for(entries, identity):
    """Basenames of every source-history edge of the selected entry and
    of each earlier same-target entry.

    The selected entry is found by its identity in the full entry list.
    Edges are taken exactly as attached to those entries (both paths),
    with no inference from other names. Source history is a deny-list
    input only: it never makes any name part of an identity and never
    satisfies a prior.
    """
    index = next(i for i, entry in enumerate(entries)
                 if entry_identity(entry) == identity)
    target = entries[index]["target"]
    found = set()
    for entry in entries[:index + 1]:
        if entry["target"] != target:
            continue
        for edge in entry.get("source_rename_history", []):
            found.add(posixpath.basename(edge["old_path"]))
            found.add(posixpath.basename(edge["new_path"]))
    return tuple(sorted(found))


def guard_sql(identity, expected_database, priors=(), history_names=()):
    """Abort the transaction on a wrong DB or bad recording state.

    The marker texts are ours, so the caller can tell "already applied
    with these bytes" (a no-op) from a conflicting record (refuse)
    without rendering database output. Priors are earlier same-target
    identities; each must already be recorded, under any of its names,
    before this migration may apply. ``history_names`` are the historical
    source names attached to this entry and its earlier same-target
    entries; the current and alias names of the priors are denied too. A row under one of them,
    or under an identity name, is refused unless it is a permitted
    (file, sha256) pair, whatever else is recorded. History never adds
    a permitted pair and never satisfies a prior.
    """
    def pairs_of(pairs):
        return ", ".join(
            f"({quote_literal(name)}, '{sha}')" for name, sha in pairs)

    permitted = list(identity) + [pair for prior in priors for pair in prior]
    denied = sorted({name for pairs in (identity, *priors) for name, _sha in pairs}
                    | set(history_names))
    duplicate_checks = "".join(f"""
    IF (SELECT count(*) FROM public.schema_migrations
        WHERE (file, sha256) IN ({pairs_of(pairs)})) > 1 THEN
        RAISE EXCEPTION '{CONFLICTING_HISTORY}';
    END IF;""" for pairs in (identity, *priors))
    names = ", ".join(quote_literal(name) for name, _sha in identity)
    denied_names = ", ".join(quote_literal(name) for name in denied)
    prior_checks = "".join(f"""
    IF NOT EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE (file, sha256) IN ({pairs_of(prior)})) THEN
        RAISE EXCEPTION '{MISSING_PRIOR}';
    END IF;""" for prior in priors)
    return f"""SELECT pg_advisory_xact_lock({ADVISORY_LOCK_KEY});
DO $guard$
BEGIN
    IF current_database() <> {quote_literal(expected_database, "expect-database")} THEN
        RAISE EXCEPTION '{WRONG_DATABASE}';
    END IF;
    IF EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE file IN ({denied_names})
                 AND (file, sha256) NOT IN ({pairs_of(permitted)})) THEN
        RAISE EXCEPTION '{CONFLICTING_HISTORY}';
    END IF;
    {duplicate_checks.strip()}
    IF EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE (file, sha256) IN ({pairs_of(identity)})) THEN
        RAISE EXCEPTION '{ALREADY_SAME}';
    END IF;
    IF EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE file IN ({names})) THEN
        RAISE EXCEPTION '{ALREADY_DIFFERENT}';
    END IF;{prior_checks}
END
$guard$;"""


def compose(file_path, manifest_path, applied_by, record_only=False,
            *, expect_database):
    """Return (sql, filename, sha256, target) for one apply, or raise."""
    validate_expected_database(expect_database)
    path = pathlib.Path(file_path)
    filename = path.name
    if not FILE_RE.match(filename):
        raise ApplyError(f"not a numbered migration filename: {filename}")
    try:
        contents = path.read_bytes()
    except OSError as exc:
        raise ApplyError(f"cannot read migration: {exc}") from exc
    sha256 = hashlib.sha256(contents).hexdigest()
    entries = load_manifest(manifest_path)
    target, identity, priors = resolve_entry(entries, filename, sha256)
    history_names = history_names_for(entries, identity)
    body = contents.decode("utf-8")
    if not record_only and has_toplevel_txn_control(body):
        raise ApplyError(
            f"{filename} carries top-level transaction control or psql "
            "meta-commands; apply it by hand, then re-run with --record-only"
        )
    parts = [guard_sql(identity, expect_database, priors, history_names)]
    if not record_only:
        parts.append(body.rstrip() + "\n")
    parts.append(insert_sql(filename, sha256, applied_by, on_conflict=False))
    return "\n".join(parts) + "\n", filename, sha256, target


def connection_env(database_url):
    """libpq env from a URL; credentials never reach argv."""
    url = urllib.parse.urlsplit(database_url)
    database = urllib.parse.unquote(url.path.removeprefix("/"))
    if (url.scheme not in ("postgres", "postgresql") or not url.hostname
            or not url.username or not database or url.fragment):
        raise ApplyError("DATABASE_URL is missing or malformed")
    query = urllib.parse.parse_qs(url.query, strict_parsing=True)
    if set(query) - {"sslmode", "sslrootcert"} or len(query.get("sslmode", [])) > 1:
        raise ApplyError("DATABASE_URL carries unsupported connection options")
    # libpq's `sslrootcert=system` is honoured by pinning the system CA
    # bundle, exactly as the staging gate does; any other root cert is
    # an arbitrary trust anchor and refuses.
    if query.get("sslrootcert", ["system"]) != ["system"]:
        raise ApplyError("DATABASE_URL sslrootcert must be system when present")
    sslmode = query.get("sslmode", [None])[0]
    local = url.hostname in ("localhost", "127.0.0.1", "::1")
    if sslmode != "verify-full" and not local:
        raise ApplyError("DATABASE_URL requires sslmode=verify-full off localhost")
    child = {k: v for k, v in os.environ.items()
             if k in ("PATH", "HOME", "LANG", "LC_ALL", "SSL_CERT_FILE", "SSL_CERT_DIR")}
    child.update({
        "PGHOST": url.hostname,
        "PGPORT": str(url.port or 5432),
        "PGDATABASE": database,
        "PGUSER": urllib.parse.unquote(url.username),
        "PGCONNECT_TIMEOUT": "10",
    })
    if url.password:
        child["PGPASSWORD"] = urllib.parse.unquote(url.password)
    if sslmode:
        child["PGSSLMODE"] = sslmode
        child["PGSSLROOTCERT"] = "/etc/ssl/certs/ca-certificates.crt"
    return child


def psql(child, sql, single_transaction=False):
    args = ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
            "--no-align", "--set", "ON_ERROR_STOP=1"]
    if single_transaction:
        args.append("--single-transaction")
    result = subprocess.run(args, input=sql, text=True, capture_output=True,
                            timeout=120, env=child)
    if result.returncode != 0:
        if ALREADY_SAME in result.stderr:
            raise _AlreadyRecorded(same=True)
        if ALREADY_DIFFERENT in result.stderr:
            raise _AlreadyRecorded(same=False)
        if WRONG_DATABASE in result.stderr:
            raise ApplyError(
                "connected database does not match --expect-database; refusing")
        if CONFLICTING_HISTORY in result.stderr:
            raise ApplyError(
                "recorded history conflicts with the manifest or source "
                "rename history; refusing")
        if MISSING_PRIOR in result.stderr:
            raise ApplyError(
                "an earlier same-target migration is not recorded; "
                "apply migrations in manifest order")
        raise ApplyError("psql failed; see the operator-side psql output")
    return result.stdout


class _AlreadyRecorded(Exception):
    def __init__(self, same):
        super().__init__(same)
        self.same = same


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--file", required=True, help="migration SQL file")
    parser.add_argument("--applied-by", required=True)
    parser.add_argument("--expect-database", required=True,
                        help="database this apply must target; checked "
                             "against DATABASE_URL and current_database()")
    parser.add_argument("--manifest", default="migrations/manifest.toml")
    parser.add_argument("--record-only", action="store_true",
                        help="only write the tracking row (file already applied)")
    parser.add_argument("--dry-run", action="store_true",
                        help="print the composed SQL; do not connect")
    args = parser.parse_args(argv)
    try:
        sql, filename, _sha256, target = compose(
            args.file, args.manifest, args.applied_by, args.record_only,
            expect_database=args.expect_database)
        if args.dry_run:
            print(ddl_sql(args.expect_database) + sql, end="")
            return 0
        child = connection_env(os.environ.get("DATABASE_URL", ""))
        if child["PGDATABASE"] != args.expect_database:
            raise ApplyError(
                "DATABASE_URL database does not match --expect-database; refusing")
        # Guarded + single-transaction: on the wrong database the
        # tracking table itself must never appear.
        psql(child, ddl_sql(args.expect_database), single_transaction=True)
        try:
            psql(child, sql, single_transaction=True)
        except _AlreadyRecorded as recorded:
            if recorded.same:
                print(f"apply-migration: {filename} already recorded; nothing applied")
                return 0
            raise ApplyError(
                f"{filename} (or its pre-rename original) is already recorded "
                "with a different sha256; refusing") from None
    except ApplyError as exc:
        print(f"apply-migration: {exc}", file=sys.stderr)
        return 1
    print(f"apply-migration: {filename} ({target}) recorded for {args.applied_by}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
