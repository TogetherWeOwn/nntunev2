"""Create and admit a fenced first-launch database from one authorized plan."""

import ipaddress
import json
import os
import re
import select
import signal
import ssl
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

from database_bootstrap_admission import read_authorized_plan
from database_bootstrap_preflight import catalog_probe_sql, require_absent_catalog_result

MAX_SECONDS = 60
MAX_SQL_BYTES = 4096
MAX_RESULT_BYTES = 64
PORT = 5432
FIELDS = {
    "version", "host", "port", "ca_pem", "maintenance", "launch",
    "branch_id", "setup_login", "setup_role", "owner_role", "migrator_role", "reader_role",
}
interrupted = False


def remember_signal(_signum, _frame):
    global interrupted
    interrupted = True


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def valid_host(host):
    try:
        ipaddress.IPv4Address(host)
        return True
    except ValueError:
        label = r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?"
        return len(host) <= 253 and re.fullmatch(rf"{label}(?:\.{label})*\.[a-z]{{2,63}}", host)


def setup_plan(raw):
    data = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    if (not isinstance(data, dict) or data.keys() != FIELDS
            or type(data["version"]) is not int or data["version"] != 2
            or type(data["port"]) is not int or data["port"] != PORT
            or any(not isinstance(data[key], str) for key in FIELDS - {"port", "version"})
            or not valid_host(data["host"])):
        raise ValueError
    roles = [data[key] for key in ("setup_role", "owner_role", "migrator_role", "reader_role")]
    if len(set(roles)) != 4 or any(
        not re.fullmatch(r"[A-Za-z0-9_.-]{1,63}", role)
        or role.lower().startswith("pg_") or role.lower() == "public"
        for role in roles
    ):
        raise ValueError
    setup_role, branch_id = data["setup_role"], data["branch_id"]
    if ("." in setup_role or not re.fullmatch(r"[a-z0-9]{10}", branch_id)
            or data["setup_login"] != f"{setup_role}.{branch_id}"):
        raise ValueError
    ca = data["ca_pem"]
    if not re.fullmatch(r"-----BEGIN CERTIFICATE-----\n(?:[A-Za-z0-9+/=]+\n)+"
                        r"-----END CERTIFICATE-----\n", ca):
        raise ValueError
    ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT).load_verify_locations(cadata=ca)
    return SimpleNamespace(**data)


def statements(plan, expiry):
    probe = catalog_probe_sql(plan.maintenance, plan.launch, plan.setup_role)
    probe += f"""
SELECT 'roles-ready'
WHERE EXISTS (
    SELECT FROM pg_roles WHERE rolname = current_user
      AND NOT (rolsuper OR rolcreaterole OR rolreplication OR rolbypassrls)
      AND rolvaliduntil > clock_timestamp() AND rolvaliduntil <= to_timestamp({expiry})
)
AND EXISTS (
    SELECT FROM pg_roles WHERE rolname = '{plan.owner_role}'
      AND NOT (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
      AND pg_has_role(current_user, oid, 'SET') AND NOT pg_has_role(current_user, oid, 'USAGE')
)
AND (
    SELECT count(*) FROM pg_roles WHERE rolname IN ('{plan.migrator_role}', '{plan.reader_role}')
      AND rolcanlogin AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
) = 2
AND NOT pg_has_role('{plan.reader_role}', '{plan.migrator_role}', 'MEMBER')
AND NOT pg_has_role('{plan.reader_role}', '{plan.owner_role}', 'MEMBER')
AND NOT pg_has_role('{plan.reader_role}', '{plan.setup_role}', 'MEMBER');
"""
    database = f'"{plan.launch}"'
    create = f"CREATE DATABASE {database} TEMPLATE template0 ALLOW_CONNECTIONS false;"
    admit = f"""
BEGIN;
REVOKE ALL ON DATABASE {database} FROM PUBLIC;
GRANT CONNECT, CREATE ON DATABASE {database} TO "{plan.migrator_role}";
ALTER DATABASE {database} ALLOW_CONNECTIONS true;
ALTER DATABASE {database} OWNER TO "{plan.owner_role}";
COMMIT;
"""
    return probe, create, admit


def remaining(deadline):
    left = deadline - time.monotonic()
    if interrupted or left <= 0:
        raise TimeoutError
    return left


def run_sql(sql, env, deadline):
    with tempfile.TemporaryFile() as script:
        script.write(sql)
        script.seek(0)
        remaining(deadline)
        process = subprocess.Popen(
            ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-f", "-"],
            env=env, stdin=script, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        try:
            output = b""
            while True:
                # Read only when ready, with a fixed cap even for a malformed reply.
                if not select.select([process.stdout], [], [], min(remaining(deadline), 0.1))[0]:
                    continue
                chunk = os.read(process.stdout.fileno(), MAX_RESULT_BYTES + 1 - len(output))
                if not chunk:
                    break
                output += chunk
                if len(output) > MAX_RESULT_BYTES:
                    raise ValueError
            while process.poll() is None:
                time.sleep(min(remaining(deadline), 0.1))
            remaining(deadline)
            if process.returncode != 0:
                raise ValueError
            return output
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=1)
            process.stdout.close()


def setup():
    write_attempted = False
    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            signal.signal(signum, remember_signal)
        raw = read_authorized_plan(os.environ)
        if raw is None:
            return "refused"
        expiry = int(os.environ["BOOTSTRAP_EXPIRY"])
        deadline = time.monotonic() + min(MAX_SECONDS, expiry - time.time())
        plan = setup_plan(raw)
        password = os.environ["BOOTSTRAP_SETUP_PASSWORD"]
        if not 0 < len(password.encode("utf-8")) <= 1024 or not password.isprintable():
            raise ValueError
        probe, create, admit = (sql.encode("ascii") for sql in statements(plan, expiry))
        if max(map(len, (probe, create, admit))) > MAX_SQL_BYTES:
            raise ValueError

        with tempfile.TemporaryDirectory(prefix="database-bootstrap-ca-") as private:
            ca = Path(private) / "root.crt"
            ca.write_text(plan.ca_pem, encoding="ascii")
            env = {
                "PATH": os.environ["PATH"], "LC_ALL": "C", "HOME": private,
                "PGHOST": plan.host, "PGPORT": str(plan.port), "PGDATABASE": plan.maintenance,
                "PGUSER": plan.setup_login, "PGPASSWORD": password, "PGCONNECT_TIMEOUT": "3",
                "PGSSLMODE": "verify-full", "PGSSLROOTCERT": str(ca), "PGGSSENCMODE": "disable",
                "PGOPTIONS": "-c search_path=pg_catalog -c statement_timeout=10000 -c lock_timeout=2000 "
                             "-c transaction_timeout=15000 -c idle_session_timeout=5000 "
                             "-c client_connection_check_interval=500",
            }
            result = run_sql(probe, env, deadline)
            first, newline, rest = result.partition(b"\n")
            require_absent_catalog_result(0, (first + newline).decode("ascii"))
            if rest != b"roles-ready\n":
                raise ValueError
            write_attempted = True
            if run_sql(create, env, deadline) or run_sql(admit, env, deadline):
                raise ValueError
            remaining(deadline)
        return "complete"
    except Exception:
        return "outcome-uncertain" if write_attempted else "refused"


if __name__ == "__main__":
    verdict = setup()
    print(f"database-bootstrap-setup: {verdict}", file=sys.stdout if verdict == "complete" else sys.stderr)
    sys.exit(0 if verdict == "complete" else 1)
