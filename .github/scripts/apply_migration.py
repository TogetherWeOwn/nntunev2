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
        --file migrations/0032_example.sql --applied-by "operator"

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
import re
import subprocess
import sys
import tomllib
import urllib.parse

DDL = """CREATE TABLE IF NOT EXISTS public.schema_migrations (
    file       TEXT PRIMARY KEY,
    sha256     TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_by TEXT NOT NULL
);"""

# Serializes every apply against this database (see the guard below).
ADVISORY_LOCK_KEY = 7271901
ALREADY_SAME = "schema-migrations: already recorded"
ALREADY_DIFFERENT = "schema-migrations: recorded with a different sha256"

FILE_RE = re.compile(r"^\d{4}_[A-Za-z0-9_]+\.sql$")
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
        data = tomllib.loads(pathlib.Path(manifest_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise ApplyError(f"cannot read manifest: {exc}") from exc
    return data.get("migration", [])


def resolve_entry(entries, filename, sha256):
    """Return (target, identity) for an applied-under name, or raise.

    Identity is every (file, sha256) pair the entry may be recorded
    under: its current name plus each ``renamed_from`` original.
    """
    for entry in entries:
        identity = [(entry["file"], entry["sha256"])]
        identity += [(alias["file"], alias["sha256"])
                     for alias in entry.get("renamed_from", [])]
        if dict(identity).get(filename) is not None:
            if dict(identity)[filename] != sha256:
                raise ApplyError(
                    f"{filename} sha256 {sha256} does not match the manifest "
                    f"{dict(identity)[filename]}"
                )
            return entry["target"], identity
    raise ApplyError(f"{filename} is not listed in the manifest")


def quote_literal(value):
    if any(ord(ch) < 0x20 for ch in value) or not value.strip():
        raise ApplyError("applied-by must be non-empty printable text")
    return "'" + value.replace("'", "''") + "'"


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


def guard_sql(identity):
    """Abort the transaction if this migration identity is recorded.

    The marker texts are ours, so the caller can tell "already applied
    with these bytes" (a no-op) from a conflicting record (refuse)
    without rendering database output.
    """
    pairs = ", ".join(
        f"({quote_literal(name)}, '{sha}')" for name, sha in identity)
    names = ", ".join(quote_literal(name) for name, _sha in identity)
    return f"""SELECT pg_advisory_xact_lock({ADVISORY_LOCK_KEY});
DO $guard$
BEGIN
    IF EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE (file, sha256) IN ({pairs})) THEN
        RAISE EXCEPTION '{ALREADY_SAME}';
    END IF;
    IF EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE file IN ({names})) THEN
        RAISE EXCEPTION '{ALREADY_DIFFERENT}';
    END IF;
END
$guard$;"""


def compose(file_path, manifest_path, applied_by, record_only=False):
    """Return (sql, filename, sha256, target) for one apply, or raise."""
    path = pathlib.Path(file_path)
    filename = path.name
    if not FILE_RE.match(filename):
        raise ApplyError(f"not a numbered migration filename: {filename}")
    try:
        contents = path.read_bytes()
    except OSError as exc:
        raise ApplyError(f"cannot read migration: {exc}") from exc
    sha256 = hashlib.sha256(contents).hexdigest()
    target, identity = resolve_entry(load_manifest(manifest_path), filename, sha256)
    body = contents.decode("utf-8")
    if not record_only and has_toplevel_txn_control(body):
        raise ApplyError(
            f"{filename} carries top-level transaction control or psql "
            "meta-commands; apply it by hand, then re-run with --record-only"
        )
    parts = [guard_sql(identity)]
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
    parser.add_argument("--manifest", default="migrations/manifest.toml")
    parser.add_argument("--record-only", action="store_true",
                        help="only write the tracking row (file already applied)")
    parser.add_argument("--dry-run", action="store_true",
                        help="print the composed SQL; do not connect")
    args = parser.parse_args(argv)
    try:
        sql, filename, _sha256, target = compose(
            args.file, args.manifest, args.applied_by, args.record_only)
        if args.dry_run:
            print(DDL + "\n" + sql, end="")
            return 0
        child = connection_env(os.environ.get("DATABASE_URL", ""))
        psql(child, DDL + "\n")
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
