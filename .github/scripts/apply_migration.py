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

A single exact outer ``BEGIN; ... COMMIT;`` envelope is lexically
validated and removed in memory, never executed. Effects and the
original-file checksum receipt then join the runner's transaction.
Other transaction control and psql meta-commands refuse, including in
``--record-only`` mode; that flag is for independently verified historic
backfill, not an unsupported-SQL workaround. Tracking bootstrap/ACL
transaction A commits independently before effects/receipt transaction B.
``--dry-run`` prints composition only, not successful apply evidence.
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

# The tracking table's rows are deploy proof for the staging gate, so
# its write privileges are pinned here. The dated backfill generator
# freezes its historical DDL separately. No role other than
# the table owner (the migrator role that creates it) may hold a write
# privilege — on the table (``pg_class.relacl``) or on any column
# (``pg_attribute.attacl``; column-level INSERT alone can forge a
# tracking row, and it never appears in ``relacl``) — whatever the
# database's default privileges granted at CREATE time. The revoke
# pass runs on every DDL execution, so a later stray GRANT is removed
# by the next apply; the staging gate also fails closed while such a
# grant exists. Read privileges are untouched. Qualify runner-owned
# routines/catalogs: the migration namespace deliberately puts public first.
DDL = """CREATE TABLE IF NOT EXISTS public.schema_migrations (
    file       TEXT PRIMARY KEY,
    sha256     TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT pg_catalog.now(),
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
        FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        CROSS JOIN pg_catalog.aclexplode(c.relacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN
              ('INSERT', 'UPDATE', 'DELETE', 'TRUNCATE', 'REFERENCES', 'TRIGGER')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE 'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                 || 'ON TABLE public.schema_migrations FROM PUBLIC';
        ELSE
            EXECUTE pg_catalog.format(
                'REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER '
                'ON TABLE public.schema_migrations FROM %I',
                pg_catalog.pg_get_userbyid(grant_row.grantee));
        END IF;
    END LOOP;
    -- Column privileges live in pg_attribute.attacl, not relacl, so the
    -- table-level pass above cannot see them; revoke per column. Only
    -- INSERT, UPDATE and REFERENCES exist as column-level writes.
    FOR grant_row IN
        SELECT DISTINCT a.grantee, t.attname
        FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_catalog.pg_attribute t ON t.attrelid = c.oid
            AND t.attnum > 0 AND NOT t.attisdropped
        CROSS JOIN pg_catalog.aclexplode(t.attacl) a
        WHERE n.nspname = 'public' AND c.relname = 'schema_migrations'
          AND a.privilege_type IN ('INSERT', 'UPDATE', 'REFERENCES')
          AND a.grantee <> c.relowner
    LOOP
        IF grant_row.grantee = 0 THEN
            EXECUTE pg_catalog.format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM PUBLIC',
                grant_row.attname, grant_row.attname, grant_row.attname);
        ELSE
            EXECUTE pg_catalog.format(
                'REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I) '
                'ON TABLE public.schema_migrations FROM %I',
                grant_row.attname, grant_row.attname, grant_row.attname,
                pg_catalog.pg_get_userbyid(grant_row.grantee));
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

FILE_RE = re.compile(r"^\d{4}_[A-Za-z0-9_]+\.sql$")
DATABASE_RE = re.compile(r"^[A-Za-z0-9_]+$")
DOLLAR_QUOTE_RE = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$")
WORD_RE = re.compile(r"[A-Za-z_][A-Za-z_0-9$]*")
TRANSACTION_WORDS = {
    "BEGIN", "START", "COMMIT", "END", "ABORT", "ROLLBACK", "SAVEPOINT", "RELEASE",
}
# Replace, never append to, caller PGOPTIONS. psql() also pins these for
# callers that supply a child environment directly (both transactions).
SESSION_OPTIONS = (
    "-c search_path=public,pg_catalog -c standard_conforming_strings=on "
    "-c statement_timeout=90000 -c lock_timeout=10000 "
    "-c idle_in_transaction_session_timeout=90000"
)


class ApplyError(Exception):
    pass


def sql_statements(text):
    """Lex a bounded SQL subset into statements of (token, start, end).

    Literals/quoted identifiers are opaque tokens; comments are trivia.
    Spans let us remove only validated boundary statements, not rewrite
    the migration. Nested block comments, doubled quotes, E strings and
    tagged dollar quotes follow PostgreSQL rules. Unicode escape forms,
    unquoted non-ASCII SQL, implicit string continuation and SQL-standard
    unquoted routine bodies are deliberately unsupported.
    No tokenization error may turn the rest of a file into hidden text.
    """
    if any(ord(ch) < 32 and ch not in "\t\n\r\f" for ch in text):
        raise ApplyError("unsupported control character in migration")
    statements, tokens, i, n = [], [], 0, len(text)
    while i < n:
        start = i
        if ord(text[i]) > 127:
            raise ApplyError("non-ASCII SQL must be quoted")
        if text[i] in " \t\n\r\f":
            i += 1
            continue
        if text.startswith("--", i):
            end = re.search(r"[\r\n]", text[i:])
            i = n if end is None else i + end.start()
            continue
        if text.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif text.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            if depth:
                raise ApplyError("unterminated block comment in migration")
            continue
        if text[i] == "\\":
            raise ApplyError("psql meta-commands are not supported")
        if text[i:i + 2].upper() == "U&":
            raise ApplyError("Unicode escape syntax is not supported")
        escaped = text[i:i + 2].upper() == "E'"
        if text[i] in "'\"" or escaped:
            if escaped:
                i += 1
            quote = text[i]
            i += 1
            while i < n:
                if escaped and text[i] == "\\":
                    i += 2
                elif text[i] == quote:
                    i += 1
                    if i < n and text[i] == quote:
                        i += 1
                    else:
                        break
                else:
                    i += 1
            else:
                raise ApplyError("unterminated quoted token in migration")
            token = "<identifier>" if quote == '"' else "<string>"
            # Newline-separated string constants inherit E-string escape
            # rules across the boundary in PostgreSQL. Refuse this rare
            # form rather than lex its continuation with different rules.
            if token == "<string>" and tokens and tokens[-1][0] == "<string>":
                raise ApplyError("implicit string continuation is not supported")
        elif match := DOLLAR_QUOTE_RE.match(text, i):
            end = text.find(match.group(0), match.end())
            if end == -1:
                raise ApplyError("unterminated dollar quote in migration")
            i = end + len(match.group(0))
            token = "<quoted>"
        elif match := WORD_RE.match(text, i):
            i = match.end()
            token = match.group(0).upper()
        else:
            token, i = text[i], i + 1
        if token == ";":
            if tokens:
                statements.append((tokens, i))
                tokens = []
        else:
            tokens.append((token, start, i))
    if tokens:
        raise ApplyError("migration statements must end with a semicolon")
    return statements


def transaction_body(body):
    """Accept plain SQL or one exact outer BEGIN; ... COMMIT; envelope.

    This is a lexical boundary validator, not a general SQL parser. SQL
    validity and transactional restrictions still belong to PostgreSQL.
    Never execute a file-level COMMIT or use record-only to skip refusal.
    """
    statements = sql_statements(body)
    if not statements:
        raise ApplyError("migration has no SQL statements")
    words = [[token for token, _, _ in tokens] for tokens, _ in statements]
    wrapped = words[0] == ["BEGIN"] and words[-1] == ["COMMIT"]
    inner = words[1:-1] if wrapped else words
    if not inner:
        raise ApplyError("transaction envelope must contain SQL")
    for statement in inner:
        # Fail closed on all transaction variants, including END/ABORT,
        # two-phase commit and SQL-standard BEGIN ATOMIC routine bodies.
        if (statement[0] in TRANSACTION_WORDS
                or statement[:2] == ["PREPARE", "TRANSACTION"]
                or "BEGIN" in statement
                or statement[0] in {"RESET", "DISCARD", "COPY"}):
            raise ApplyError("unsupported transaction control or SQL form")
        if statement[0] == "SET" and statement != [
                "SET", "SEARCH_PATH", "=", "PUBLIC", ",", "PG_CATALOG"]:
            raise ApplyError("only the pinned search_path SET is supported")
    if wrapped:
        # Remove boundary tokens + their semicolons, preserving every
        # effect statement and all surrounding comments/literal bytes.
        opening, closing = statements[0], statements[-1]
        return (body[:opening[0][0][1]] + body[opening[1]:closing[0][0][1]]
                + body[closing[1]:])
    return body


def load_manifest(manifest_path):
    try:
        data = tomllib.loads(pathlib.Path(manifest_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise ApplyError(f"cannot read manifest: {exc}") from exc
    return data.get("migration", [])


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
    when = f"'{applied_at}'::timestamptz" if applied_at else "pg_catalog.now()"
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
    IF pg_catalog.current_database() <> {quote_literal(expected_database, "expect-database")} THEN
        RAISE EXCEPTION '{WRONG_DATABASE}';
    END IF;
END
$dbguard$;"""


def ddl_sql(expected_database):
    # Bootstrap/ACL writes need serialization too: concurrent CREATE or
    # REVOKE can race on catalog tuples even when B is correctly locked.
    # A still commits independently and releases its lock before B.
    return (database_guard_sql(expected_database)
            + f"\nSELECT pg_catalog.pg_advisory_xact_lock({ADVISORY_LOCK_KEY});\n"
            + DDL + "\n")


def guard_sql(identity, expected_database, priors=()):
    """Abort the transaction on a wrong DB or bad recording state.

    The marker texts are ours, so the caller can tell "already applied
    with these bytes" (a no-op) from a conflicting record (refuse)
    without rendering database output. Priors are earlier same-target
    identities; each must already be recorded, under any of its names,
    before this migration may apply.
    """
    def pairs_of(pairs):
        return ", ".join(
            f"({quote_literal(name)}, '{sha}')" for name, sha in pairs)

    names = ", ".join(quote_literal(name) for name, _sha in identity)
    prior_checks = "".join(f"""
    IF NOT EXISTS (SELECT 1 FROM public.schema_migrations
               WHERE (file, sha256) IN ({pairs_of(prior)})) THEN
        RAISE EXCEPTION '{MISSING_PRIOR}';
    END IF;""" for prior in priors)
    return f"""SELECT pg_catalog.pg_advisory_xact_lock({ADVISORY_LOCK_KEY});
DO $guard$
BEGIN
    IF pg_catalog.current_database() <> {quote_literal(expected_database, "expect-database")} THEN
        RAISE EXCEPTION '{WRONG_DATABASE}';
    END IF;
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
    target, identity, priors = resolve_entry(
        load_manifest(manifest_path), filename, sha256)
    try:
        body = transaction_body(contents.decode("utf-8"))
    except UnicodeError as exc:
        raise ApplyError("migration must be valid UTF-8") from exc
    parts = [guard_sql(identity, expect_database, priors)]
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
        "PGOPTIONS": SESSION_OPTIONS,
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
    try:
        result = subprocess.run(
            args, input=sql, text=True, capture_output=True, timeout=120,
            env={**child, "PGOPTIONS": SESSION_OPTIONS})
    except subprocess.TimeoutExpired:
        raise ApplyError(
            "psql timed out; commit outcome may be unknown; reconcile the "
            "original checksum receipt and effects before retrying") from None
    except OSError:
        raise ApplyError("cannot start psql; connection details redacted") from None
    if result.returncode != 0:
        if ALREADY_SAME in result.stderr:
            raise _AlreadyRecorded(same=True)
        if ALREADY_DIFFERENT in result.stderr:
            raise _AlreadyRecorded(same=False)
        if WRONG_DATABASE in result.stderr:
            raise ApplyError(
                "connected database does not match --expect-database; refusing")
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
