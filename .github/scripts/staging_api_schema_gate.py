#!/usr/bin/env python3
"""Read-only, fail-closed schema prerequisite for the actual staging binding.

Never apply migrations here. Connection details and provider errors are private;
only a fixed verdict reaches CI output. The direct staging migrator target must
match the selected Hyperdrive origin before any database connection is opened.

The schema verdict compares the staging database's ``schema_migrations``
tracking rows (written by ``apply_migration.py`` on every apply) against
``migrations/manifest.toml``: every ``app`` entry must be recorded exactly
once under its current name+sha256 or under a ``renamed_from`` original
name+sha256, and no unlisted rows may exist.
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.parse
import urllib.request

from migration_history import HistoryError, parse_manifest, read_input


class GateError(Exception):
    pass


# Fixed, non-secret failure categories. Only one of these words ever reaches
# CI output, never exception text, so the redaction contract is unchanged:
#   config   - repo secret/variable missing or malformed (no network yet)
#   binding  - Hyperdrive config fetch failed or does not match the migrator
#   connect  - psql could not connect/authenticate (TLS, auth, network, timeout)
#   query    - connected, but the read-only catalog query errored
#   schema   - query ran; schema_migrations absent/shadowed or != manifest
CATEGORIES = ("config", "binding", "connect", "query", "schema")

# Fixed, non-secret operator hints. Like the category word, these are constants
# and never derived from exception text, so redaction is unaffected.
HINTS = {
    "schema": "staging schema behind repo migrations — see docs/ops.md staging migrations",
}
DEFAULT_HINT = "configuration, target or schema not verified"


class CategorizedFailure(Exception):
    def __init__(self, category):
        assert category in CATEGORIES
        super().__init__(category)
        self.category = category


def _stage(category, fn, *args):
    try:
        return fn(*args)
    except CategorizedFailure:
        raise
    except Exception as exc:
        raise CategorizedFailure(category) from exc


FILE_RE = re.compile(r"^\d{4}_[A-Za-z0-9_]+\.sql$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_ROWS_OUTPUT = 1_000_000


def load_expected(manifest_path=None):
    """Return the app-target manifest entries the staging DB must record.

    Each entry is ``(file, sha256, renamed_from)`` where ``renamed_from``
    is a tuple of original ``(file, sha256)`` pairs a pre-merge renumber
    may have been applied under. Anything malformed raises GateError;
    the manifest is repo configuration, so callers stage this as config.
    """
    if manifest_path is None:
        manifest_path = pathlib.Path(__file__).resolve().parents[2] / "migrations" / "manifest.toml"
    try:
        entries = parse_manifest(read_input(manifest_path))
    except (OSError, UnicodeError, HistoryError) as exc:
        raise GateError("cannot read migrations manifest") from exc
    expected = []
    for entry in entries:
        if entry["target"] != "app":
            continue
        # Source history is parsed strictly for both targets, but is NEVER
        # part of this positive accepted set. Different-byte rows stay unknown.
        aliases = tuple((a["file"], a["sha256"]) for a in entry.get("renamed_from", []))
        expected.append((entry["file"], entry["sha256"], aliases))
    if not expected:
        raise GateError("migrations manifest has no app entries")
    return expected


def compare(expected, rows):
    """True iff applied rows satisfy every manifest entry exactly once.

    A row satisfies an entry under the entry's current name+sha or any
    ``renamed_from`` original name+sha. Unknown rows, sha mismatches,
    missing entries and an entry satisfied by two rows (applied, then
    applied again under the new name) all fail.
    """
    acceptable = {}
    for index, (filename, sha, renamed) in enumerate(expected):
        for pair in [(filename, sha), *renamed]:
            if pair in acceptable:
                return False
            acceptable[pair] = index
    satisfied = [0] * len(expected)
    seen_files = set()
    for row in rows:
        if row[0] in seen_files or row not in acceptable:
            return False
        seen_files.add(row[0])
        satisfied[acceptable[row]] += 1
    return all(count == 1 for count in satisfied)


def parse_rows(stdout):
    """Parse the bounded JSON row transport; anything else fails schema.

    JSON (not delimiter-joined text) so a stored value containing a tab
    or newline can never frame-shift one row into another.
    """
    if len(stdout) > MAX_ROWS_OUTPUT:
        raise CategorizedFailure("schema")
    try:
        payload = json.loads(stdout)
    except ValueError as exc:
        raise CategorizedFailure("schema") from exc
    if not isinstance(payload, list):
        raise CategorizedFailure("schema")
    rows = []
    for item in payload:
        if (not isinstance(item, list) or len(item) != 2
                or not isinstance(item[0], str) or not FILE_RE.match(item[0])
                or not isinstance(item[1], str) or not SHA_RE.match(item[1])):
            raise CategorizedFailure("schema")
        rows.append((item[0], item[1]))
    return rows


# The gate connects as the migrator and reads `public.schema_migrations`
# qualified, but a same-named relation in another non-system schema would
# shadow the unqualified name for other roles; requiring that none exists
# keeps the checked table the only one any search path can resolve.
# The rows are deploy proof, so the guard also requires that no role
# other than the table owner (the migrator) holds a write privilege on
# the table (relacl) or on any column (attacl — column-level INSERT
# alone can forge a tracking row and never appears in relacl):
# apply_migration.py's DDL pins that on every apply, and the gate
# fails closed in between if a write grant appears.
GUARD_SQL = """BEGIN READ ONLY;
SELECT current_database() = 'nntunev2_staging'
   AND EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE n.nspname='public' AND c.relname='schema_migrations' AND c.relkind='r')
   AND NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE c.relname='schema_migrations'
                 AND n.nspname NOT IN ('public','pg_catalog','information_schema')
                 AND n.nspname NOT LIKE 'pg\\_temp\\_%'
                 AND n.nspname NOT LIKE 'pg\\_toast\\_temp\\_%')
   AND NOT EXISTS (SELECT 1 FROM pg_class c CROSS JOIN aclexplode(c.relacl) a
               WHERE c.oid=to_regclass('public.schema_migrations')
                 AND a.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER')
                 AND a.grantee <> c.relowner)
   AND NOT EXISTS (SELECT 1 FROM pg_class c
               JOIN pg_attribute t ON t.attrelid=c.oid AND t.attnum > 0 AND NOT t.attisdropped
               CROSS JOIN aclexplode(t.attacl) a
               WHERE c.oid=to_regclass('public.schema_migrations')
                 AND a.privilege_type IN ('INSERT','UPDATE','REFERENCES')
                 AND a.grantee <> c.relowner)
   AND (SELECT count(*) = 2 FROM pg_attribute a
        WHERE a.attrelid=to_regclass('public.schema_migrations') AND a.attnum > 0
          AND NOT a.attisdropped AND a.atttypmod = -1
          AND a.atttypid='text'::regtype
          AND a.attname IN ('file','sha256'));
COMMIT;
"""

ROWS_SQL = """BEGIN READ ONLY;
SELECT COALESCE(json_agg(json_build_array(file, sha256) ORDER BY file), '[]'::json)
FROM public.schema_migrations;
COMMIT;
"""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def fetch_config(env):
    account = env.get("CLOUDFLARE_ACCOUNT_ID", "")
    config = env.get("HYPERDRIVE_ID_STAGING", "")
    token = env.get("CLOUDFLARE_API_TOKEN", "")
    if not token or not all(re.fullmatch(r"[a-fA-F0-9-]{32,36}", x) for x in (account, config)):
        raise GateError("missing or invalid staging binding configuration")
    request = urllib.request.Request(
        f"https://api.cloudflare.com/client/v4/accounts/{account}/hyperdrive/configs/{config}",
        headers={"Authorization": "Bearer " + token},
    )
    opener = urllib.request.build_opener(NoRedirect())
    with opener.open(request, timeout=10) as response:
        raw = response.read(65537)
    if len(raw) > 65536:
        raise GateError("oversized staging binding response")
    return json.loads(raw)


def connection_env(env, payload):
    url = urllib.parse.urlsplit(env.get("PLANETSCALE_MIGRATOR_URL_STAGING", ""))
    database = urllib.parse.unquote(url.path.removeprefix("/"))
    if (url.scheme not in ("postgres", "postgresql") or not url.hostname
            or not url.username or not url.password or url.fragment
            or database != "nntunev2_staging"):
        raise GateError("missing or invalid staging database configuration")
    query = urllib.parse.parse_qs(url.query, strict_parsing=True)
    # `sslrootcert=system` (libpq's system trust store, as PlanetScale issues
    # it) is accepted only because the gate pins the system CA bundle below;
    # any other root cert would point verification at an arbitrary file.
    if (set(query) - {"sslmode", "sslrootcert"} or len(query.get("sslmode", [])) > 1
            or query.get("sslrootcert", ["system"]) != ["system"]):
        raise GateError("unsupported staging connection options")
    sslmode = query.get("sslmode", ["verify-full"])[0]
    if sslmode != "verify-full":
        raise GateError("staging schema check requires verified TLS")
    if payload is None:
        url.port  # raises on a malformed port
        return None
    origin = payload.get("result", {}).get("origin", {})
    if (payload.get("success") is not True or origin.get("scheme") not in ("postgres", "postgresql")
            or origin.get("database") != database
            or not isinstance(origin.get("host"), str)
            or origin["host"].lower().rstrip(".") != url.hostname.lower().rstrip(".")
            or origin.get("port") != (url.port or 5432)):
        raise GateError("staging database does not match the selected binding")
    # No URL/password in argv, inherited service files, passfiles, or arbitrary
    # libpq options. Never inherit alternative credentials after a failure.
    child = {k: v for k, v in env.items()
             if k in ("PATH", "HOME", "LANG", "LC_ALL", "SSL_CERT_FILE", "SSL_CERT_DIR")}
    child.update({
        "PGHOST": url.hostname,
        "PGPORT": str(url.port or 5432),
        "PGDATABASE": database,
        "PGUSER": urllib.parse.unquote(url.username),
        "PGPASSWORD": urllib.parse.unquote(url.password),
        "PGSSLMODE": sslmode,
        "PGSSLROOTCERT": "/etc/ssl/certs/ca-certificates.crt",
        "PGCONNECT_TIMEOUT": "5",
        "PGOPTIONS": "-c default_transaction_read_only=on -c statement_timeout=5000 -c lock_timeout=1000 -c search_path=pg_catalog",
    })
    return child


def validate_inputs(env):
    """Shape-check every input before any network call (category: config)."""
    if not env.get("CLOUDFLARE_API_TOKEN") or not all(
            re.fullmatch(r"[a-fA-F0-9-]{32,36}", env.get(k, ""))
            for k in ("CLOUDFLARE_ACCOUNT_ID", "HYPERDRIVE_ID_STAGING")):
        raise GateError("missing or invalid staging binding configuration")
    # connection_env validates the migrator URL before reading the payload.
    connection_env(env, None)


def _run_sql(run, child, sql):
    try:
        result = run(
            ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
             "--no-align", "--set", "ON_ERROR_STOP=1"],
            input=sql, text=True, capture_output=True, timeout=15, env=child,
        )
    except Exception as exc:
        raise CategorizedFailure("connect") from exc
    # psql exit codes: 2 = connection to the server went bad, 3 = an error
    # occurred in the script (ON_ERROR_STOP), 1 = other fatal client error.
    if result.returncode == 2:
        raise CategorizedFailure("connect")
    if result.returncode != 0:
        raise CategorizedFailure("query")
    return result.stdout


def check(env, fetch=fetch_config, run=subprocess.run, expected=None):
    _stage("config", validate_inputs, env)
    if expected is None:
        expected = _stage("config", load_expected)
    payload = _stage("binding", fetch, env)
    child = _stage("binding", connection_env, env, payload)
    # Guard first, so a missing or shadowed tracking table is a schema
    # verdict rather than a query error from selecting from it.
    if _run_sql(run, child, GUARD_SQL).strip() != "t":
        raise CategorizedFailure("schema")
    rows = parse_rows(_run_sql(run, child, ROWS_SQL))
    if not compare(expected, rows):
        raise CategorizedFailure("schema")


def main():
    try:
        check(dict(os.environ))
    except Exception as exc:
        # Deliberately do not render exceptions, SQL output, URLs, provider
        # payloads, or stderr; each can contain credentials or private targets.
        # Only a fixed category word from CATEGORIES is printed.
        category = exc.category if isinstance(exc, CategorizedFailure) else "unknown"
        hint = HINTS.get(category, DEFAULT_HINT)
        print(f"staging-api-schema: FAIL category={category} ({hint})", file=sys.stderr)
        return 1
    print("staging-api-schema: PASS (actual staging binding; read-only schema check)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
