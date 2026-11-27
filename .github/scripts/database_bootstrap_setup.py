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

from database_bootstrap_admission import MAX_PLAN_BYTES, read_authorized_plan, reject_constant, unique_fields
from database_bootstrap_preflight import catalog_probe_sql, require_absent_catalog_result, validate_catalog_pins

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


def load_plan(raw):
    if not 0 < len(raw) <= MAX_PLAN_BYTES:
        raise ValueError("invalid bootstrap plan size")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields, parse_constant=reject_constant)
    except (ValueError, RecursionError):
        raise ValueError("invalid bootstrap plan JSON") from None


def valid_host(host):
    try:
        ipaddress.IPv4Address(host)
        return True
    except ValueError:
        label = r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?"
        return len(host) <= 253 and re.fullmatch(rf"{label}(?:\.{label})*\.[a-z]{{2,63}}", host)


def validate_ca(ca):
    if not re.fullmatch(r"-----BEGIN CERTIFICATE-----\n(?:[A-Za-z0-9+/=]+\n)+"
                        r"-----END CERTIFICATE-----\n", ca):
        raise ValueError
    ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT).load_verify_locations(cadata=ca)


def setup_plan(raw):
    data = load_plan(raw)
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
    if ("." in setup_role or "." in data["reader_role"]
            or not re.fullmatch(r"(?:[a-z0-9]{10}|[a-z0-9]{12})", branch_id)
            or data["setup_login"] != f"{setup_role}.{branch_id}"):
        raise ValueError
    validate_catalog_pins(data["maintenance"], data["launch"], setup_role)
    validate_ca(data["ca_pem"])
    return SimpleNamespace(**data)


def statements(plan, expiry):
    if type(expiry) is not int or not 0 < expiry <= 9999999999:
        raise ValueError("invalid setup expiry")
    probe = catalog_probe_sql(plan.maintenance, plan.launch, plan.setup_role)
    probe += f"""
SELECT 'roles-ready'
WHERE NOT pg_is_in_recovery()
AND EXISTS (
    SELECT FROM pg_roles r WHERE rolname = current_user
      AND rolconnlimit = 1 AND NOT (rolinherit OR rolsuper OR rolcreaterole OR rolreplication OR rolbypassrls)
      AND rolvaliduntil > clock_timestamp() AND rolvaliduntil <= to_timestamp({expiry})
      AND (SELECT count(*) FROM pg_auth_members WHERE member = r.oid) = 1
)
AND EXISTS (
    SELECT FROM pg_roles r WHERE rolname = '{plan.owner_role}'
      AND NOT (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
      AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = r.oid)
      AND EXISTS (
          SELECT FROM pg_auth_members m JOIN pg_roles member ON member.oid = m.member
          WHERE m.roleid = r.oid AND member.rolname = current_user
            AND m.set_option AND NOT (m.inherit_option OR m.admin_option)
      )
)
AND EXISTS (
    SELECT FROM pg_roles WHERE rolname = '{plan.migrator_role}'
      AND rolcanlogin AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
)
AND EXISTS (
    SELECT FROM pg_roles r WHERE rolname = '{plan.reader_role}'
      AND rolcanlogin AND rolconnlimit = 1 AND rolvaliduntil = 'epoch'::timestamptz
      AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)
      AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = r.oid)
      AND NOT EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = r.oid)
);
"""
    database = f'"{plan.launch}"'
    scope = f"""current_database() = '{plan.maintenance}' AND session_user = '{plan.setup_role}'
    AND current_user = '{plan.setup_role}' AND NOT pg_is_in_recovery()
    AND clock_timestamp() < to_timestamp({expiry})"""
    create = f"""
SELECT ({scope}) AS bootstrap_write
\\gset
\\if :bootstrap_write
CREATE DATABASE {database} TEMPLATE template0 ALLOW_CONNECTIONS false;
\\else
SELECT 1/0;
\\endif
"""
    admit = f"""
BEGIN;
DO $bootstrap$
BEGIN
    IF NOT ({scope}) OR NOT EXISTS (
        SELECT FROM pg_database d JOIN pg_roles r ON r.oid = d.datdba
        WHERE d.datname = '{plan.launch}' AND r.rolname = current_user
          AND NOT datistemplate AND NOT datallowconn AND datacl IS NULL
    ) THEN
        RAISE EXCEPTION 'database admission refused';
    END IF;
END
$bootstrap$;
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


def tls_environment(private, plan, database, login, password):
    ca = Path(private) / "root.crt"
    ca.write_text(plan.ca_pem, encoding="ascii")
    return {
        "PATH": os.environ["PATH"], "LC_ALL": "C", "HOME": private,
        "PGHOST": plan.host, "PGPORT": str(plan.port), "PGDATABASE": database,
        "PGUSER": login, "PGPASSWORD": password, "PGCONNECT_TIMEOUT": "3",
        "PGSSLMODE": "verify-full", "PGSSLROOTCERT": str(ca), "PGGSSENCMODE": "disable",
        "PGOPTIONS": "-c search_path=pg_catalog -c statement_timeout=10000 -c lock_timeout=2000 "
                     "-c transaction_timeout=15000 -c idle_session_timeout=5000 "
                     "-c client_connection_check_interval=500",
    }


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
            env = tls_environment(private, plan, plan.maintenance, plan.setup_login, password)
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
