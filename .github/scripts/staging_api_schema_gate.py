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


SCHEMA_SQL = """BEGIN READ ONLY;
SELECT current_database() = 'nntunev2_staging'
   AND EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE n.nspname='public' AND c.relname='sessions' AND c.relkind='r')
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
    origin = payload.get("result", {}).get("origin", {})
    if (payload.get("success") is not True or origin.get("scheme") not in ("postgres", "postgresql")
            or origin.get("database") != database
            or not isinstance(origin.get("host"), str)
            or origin["host"].lower().rstrip(".") != url.hostname.lower().rstrip(".")
            or origin.get("port") != (url.port or 5432)):
        raise GateError("staging database does not match the selected binding")
    query = urllib.parse.parse_qs(url.query, strict_parsing=True)
    if set(query) - {"sslmode"} or len(query.get("sslmode", [])) > 1:
        raise GateError("unsupported staging connection options")
    sslmode = query.get("sslmode", ["verify-full"])[0]
    if sslmode != "verify-full":
        raise GateError("staging schema check requires verified TLS")
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


def check(env, fetch=fetch_config, run=subprocess.run):
    child = connection_env(env, fetch(env))
    result = run(
        ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
         "--no-align", "--set", "ON_ERROR_STOP=1"],
        input=SCHEMA_SQL, text=True, capture_output=True, timeout=15, env=child,
    )
    if result.returncode != 0 or result.stdout.strip() != "t":
        raise GateError("staging session schema prerequisite not verified")


def main():
    try:
        check(dict(os.environ))
    except Exception:
        # Deliberately do not render exceptions, SQL output, URLs, provider
        # payloads, or stderr; each can contain credentials or private targets.
        print("staging-api-schema: FAIL (configuration, target or schema not verified)", file=sys.stderr)
        return 1
    print("staging-api-schema: PASS (actual staging binding; read-only schema check)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
