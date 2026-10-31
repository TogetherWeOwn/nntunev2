#!/usr/bin/env python3
"""Read-only, fail-closed schema prerequisite for the actual staging binding.

Never apply migrations here. Connection details and provider errors are private;
only a fixed verdict reaches CI output. The direct staging migrator target must
match the selected Hyperdrive origin before any database connection is opened.
"""
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request


class GateError(Exception):
    pass


# Fixed, non-secret failure categories. Only one of these words ever reaches
# CI output, never exception text, so the redaction contract is unchanged:
#   config   - repo secret/variable missing or malformed (no network yet)
#   binding  - Hyperdrive config fetch failed or does not match the migrator
#   connect  - psql could not connect/authenticate (TLS, auth, network, timeout)
#   query    - connected, but the read-only catalog query errored
#   schema   - query ran; required sessions columns absent or shadowed
CATEGORIES = ("config", "binding", "connect", "query", "schema")


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


# The Worker resolves unqualified `sessions` through its runtime role's
# search path, while this gate connects as the migrator. Requiring that no
# other non-system `sessions` relation exists means any search path that
# resolves `sessions` at all resolves the checked public table.
SCHEMA_SQL = """BEGIN READ ONLY;
SELECT current_database() = 'nntunev2_staging'
   AND EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE n.nspname='public' AND c.relname='sessions' AND c.relkind='r')
   AND NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE c.relname='sessions'
                 AND n.nspname NOT IN ('public','pg_catalog','information_schema')
                 AND n.nspname NOT LIKE 'pg\\_temp\\_%'
                 AND n.nspname NOT LIKE 'pg\\_toast\\_temp\\_%')
   AND (SELECT count(*) = 3 FROM pg_attribute a
        WHERE a.attrelid=to_regclass('public.sessions') AND a.attnum > 0
          AND NOT a.attisdropped AND NOT a.attnotnull AND a.atttypmod = -1
          AND ((a.attname='refresh_retry_hash' AND a.atttypid='text'::regtype)
            OR (a.attname='refresh_issued_at' AND a.atttypid='int8'::regtype)
            OR (a.attname='refresh_claimed_at' AND a.atttypid='int8'::regtype)));
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
    if set(query) - {"sslmode"} or len(query.get("sslmode", [])) > 1:
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


def check(env, fetch=fetch_config, run=subprocess.run):
    _stage("config", validate_inputs, env)
    payload = _stage("binding", fetch, env)
    child = _stage("binding", connection_env, env, payload)
    try:
        result = run(
            ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
             "--no-align", "--set", "ON_ERROR_STOP=1"],
            input=SCHEMA_SQL, text=True, capture_output=True, timeout=15, env=child,
        )
    except Exception as exc:
        raise CategorizedFailure("connect") from exc
    # psql exit codes: 2 = connection to the server went bad, 3 = an error
    # occurred in the script (ON_ERROR_STOP), 1 = other fatal client error.
    if result.returncode == 2:
        raise CategorizedFailure("connect")
    if result.returncode != 0:
        raise CategorizedFailure("query")
    if result.stdout.strip() != "t":
        raise CategorizedFailure("schema")


def main():
    try:
        check(dict(os.environ))
    except Exception as exc:
        # Deliberately do not render exceptions, SQL output, URLs, provider
        # payloads, or stderr; each can contain credentials or private targets.
        # Only a fixed category word from CATEGORIES is printed.
        category = exc.category if isinstance(exc, CategorizedFailure) else "unknown"
        print(f"staging-api-schema: FAIL category={category} "
              "(configuration, target or schema not verified)", file=sys.stderr)
        return 1
    print("staging-api-schema: PASS (actual staging binding; read-only schema check)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
