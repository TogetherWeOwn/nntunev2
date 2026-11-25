"""Offline contract for a dedicated stock PostgreSQL trial target."""

import hashlib
import re
import ssl
import subprocess
from urllib.parse import parse_qs, urlsplit

from staging_apply import StagingApplyError, evidence_child_env
from staging_trial_isolated import ROLES
from staging_trial_role_acl import other_database_connections, require
from staging_trial_role_lifecycle import check_catalog_connections, psql_output, target
from staging_trial_role_preflight import DATABASE, oid

VERSION = 180006
CONTROL = "SELECT system_identifier FROM pg_catalog.pg_control_system()"
PEM = r"-----BEGIN CERTIFICATE-----\s+[A-Za-z0-9+/=\s]+-----END CERTIFICATE-----"


def fingerprint(pem):
    if not re.fullmatch(PEM, pem.strip()):
        raise ValueError
    return hashlib.sha256(ssl.PEM_cert_to_DER_cert(pem.strip())).hexdigest()


def tls_connection(raw, pins, route, role, ca_file, *, run=subprocess.run):
    # Give libpq a dedicated CA. A separate unauthenticated handshake checks
    # the leaf pin before sending credentials; libpq verifies TLS again.
    if parse_qs(urlsplit(raw).query, strict_parsing=True) != {"sslmode": ["verify-full"]}:
        raise ValueError
    env = evidence_child_env(raw, DATABASE)
    host, port = pins["routes"][route]
    if ((env["PGHOST"], env["PGPORT"], env["PGUSER"]) != (host, str(port), ROLES[role])
            or not env["PGPASSWORD"] or ca_file.stat().st_size > 8192
            or fingerprint(ca_file.read_text(encoding="ascii")) != pins["ca_sha256"]):
        raise ValueError
    probe = run(["openssl", "s_client", "-starttls", "postgres", "-connect", f"{host}:{port}",
                 "-servername", host, "-verify_hostname", host, "-verify_return_error", "-CAfile",
                 str(ca_file), "-no-CApath", "-no-CAstore", "-tls1_3", "-showcerts"],
                input="", capture_output=True, text=True, timeout=15, env={"PATH": env["PATH"]})
    leaf = re.search(PEM, probe.stdout)
    if probe.returncode or not leaf or fingerprint(leaf[0]) != pins["server_sha256"]:
        raise StagingApplyError("isolated server certificate identity")
    return env | {"PGSSLROOTCERT": str(ca_file), "PGSSLMINPROTOCOLVERSION": "TLSv1.3",
                  "PGCHANNELBINDING": "require", "PGGSSENCMODE": "disable", "PGPASSFILE": "/dev/null"}


def session_guard(pins, role):
    role_oid = pins["role_oids"][role]
    if not oid(role_oid):
        raise ValueError
    sql = require(f"""to_regrole(current_user)::oid = {role_oid}
        AND current_setting('server_version_num')::int = {VERSION}
        AND EXISTS (SELECT FROM pg_catalog.pg_stat_ssl WHERE pid = pg_backend_pid()
                    AND ssl AND version = 'TLSv1.3')""")
    if role == "owner":
        sql += require(f"({CONTROL}) = {pins['system_identifier']}")
    return sql


def check_catalog(pins, owner, executor, *, query=psql_output):
    check_catalog_connections({"database_oid": pins["database_oid"], "executor_oid": pins["role_oids"]["executor"]},
                              owner, executor, owner_guard=session_guard(pins, "owner"),
                              executor_guard=session_guard(pins, "executor"), query=query)


def check_session(pins, role, env, *, query=psql_output):
    # The owner is the sole SQL control-identity probe. Serving roles prove
    # their own canonical session and role OIDs without control-file privileges.
    guard = session_guard(pins, role)
    if role in ("reader", "writer"):
        guard += require(f"NOT EXISTS ({other_database_connections('current_user', include_closed=True)})")
    query(env, "BEGIN READ ONLY;\n" + target(pins, ROLES[role], False) + guard + "COMMIT;\n",
          "isolated session continuity")
