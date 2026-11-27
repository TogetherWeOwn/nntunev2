"""Read a bounded launch database baseline through the distinct reader lane."""

import os
import re
import signal
import sys
import tempfile
import time
from types import SimpleNamespace

from database_bootstrap_admission import read_authorized_plan
from database_bootstrap_setup import (
    MAX_SECONDS, PORT, load_plan, remember_signal, run_sql, tls_environment, valid_host, validate_ca,
)

WORKFLOW = ".github/workflows/deploy-database-baseline.yml"
OIDS = {"database_oid", "reader_oid"}
FIELDS = {"version", "host", "port", "ca_pem", "branch_id", "launch", "reader_role"} | OIDS
MAX_RELATIONS = 256
MAX_SQL_BYTES = 8192


def baseline_plan(raw):
    data = load_plan(raw)
    if (
        not isinstance(data, dict) or data.keys() != FIELDS
        or type(data["version"]) is not int or data["version"] != 2
        or type(data["port"]) is not int or data["port"] != PORT
        or any(type(data[k]) is not int or not 0 < data[k] <= 4294967295 for k in OIDS)
        or any(not isinstance(data[k], str) for k in FIELDS - OIDS - {"version", "port"})
        or not valid_host(data["host"])
        or not re.fullmatch(r"(?:[a-z0-9]{10}|[a-z0-9]{12})", data["branch_id"])
        or not re.fullmatch(r"[a-z_][a-z0-9_]{0,62}", data["launch"])
        or data["launch"] in ("postgres", "template0", "template1")
        or not re.fullmatch(r"[A-Za-z0-9_-]{1,63}", data["reader_role"])
        or data["reader_role"].lower().startswith("pg_")
        or data["reader_role"].lower() == "public"
    ):
        raise ValueError
    validate_ca(data["ca_pem"])
    return SimpleNamespace(**data)


def baseline_sql(plan):
    return f"""
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL row_security = off;
DO $baseline$
DECLARE
    target record;
    relation_count integer := 0;
    occupied boolean := false;
    found_row boolean;
    probes text[] := ARRAY[]::text[];
    probe text;
    role_id oid := (SELECT oid FROM pg_roles WHERE rolname = current_user);
BEGIN
    IF pg_is_in_recovery() OR current_database() <> '{plan.launch}'
        OR session_user <> '{plan.reader_role}' OR current_user <> '{plan.reader_role}'
        OR NOT EXISTS (
            SELECT FROM pg_database WHERE datname = current_database()
              AND oid = '{plan.database_oid}'::oid AND NOT datistemplate AND datdba <> role_id
        )
        OR NOT EXISTS (
            SELECT FROM pg_roles WHERE oid = role_id AND rolcanlogin
              AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
        )
        OR EXISTS (SELECT FROM pg_auth_members WHERE member = role_id)
        OR has_database_privilege(current_database(), 'CREATE,TEMP,CONNECT WITH GRANT OPTION')
        OR NOT has_table_privilege('pg_largeobject_metadata', 'SELECT')
        OR has_table_privilege('pg_largeobject', 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
        OR has_any_column_privilege('pg_largeobject', 'SELECT,INSERT,UPDATE,REFERENCES')
        OR EXISTS (
            SELECT FROM pg_largeobject_metadata l
            LEFT JOIN LATERAL aclexplode(l.lomacl) a ON true
            WHERE l.lomowner = role_id
              OR a.grantee IN (0, role_id)
        )
        OR EXISTS (
            SELECT FROM pg_namespace
            WHERE nspowner = role_id OR has_schema_privilege(oid, 'CREATE,USAGE WITH GRANT OPTION')
        )
        OR EXISTS (
            SELECT FROM pg_proc
            WHERE proowner = role_id OR (
                prosecdef AND pronamespace NOT IN ('pg_catalog'::regnamespace, 'information_schema'::regnamespace)
                AND has_function_privilege(oid, 'EXECUTE')
            )
        )
    THEN
        RAISE EXCEPTION 'baseline scope refused';
    END IF;
    IF role_id IS DISTINCT FROM '{plan.reader_oid}'::oid THEN
        RAISE EXCEPTION 'baseline reader OID refused';
    END IF;

    FOR target IN
        SELECT c.oid, n.oid AS schema_oid, n.nspname, c.relname, c.relkind, c.relowner,
               c.relrowsecurity, c.relpersistence, am.amname
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        LEFT JOIN pg_am am ON am.oid = c.relam
        WHERE c.relkind IN ('r','p','m','f','S','v')
          AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
          AND n.nspname !~ '^pg_(toast_)?temp_[0-9]+$'
        LIMIT {MAX_RELATIONS + 1}
    LOOP
        relation_count := relation_count + 1;
        IF relation_count > {MAX_RELATIONS} OR target.relowner = role_id
            OR target.relkind = 'f' OR target.relpersistence = 't' THEN
            RAISE EXCEPTION 'baseline inventory refused';
        END IF;
        IF target.relkind = 'S' THEN
            IF has_sequence_privilege(target.oid, 'USAGE,UPDATE,SELECT WITH GRANT OPTION') THEN
                RAISE EXCEPTION 'baseline sequence privilege refused';
            END IF;
        ELSE
            IF has_table_privilege(target.oid,
                'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN,SELECT WITH GRANT OPTION')
                OR has_any_column_privilege(target.oid, 'INSERT,UPDATE,REFERENCES,SELECT WITH GRANT OPTION')
            THEN
                RAISE EXCEPTION 'baseline table privilege refused';
            END IF;
            -- Views and partition roots have no storage of their own. Inspect
            -- each physical partition separately and never expand a view.
            IF target.relkind IN ('r','m') THEN
                IF target.relrowsecurity OR target.amname IS DISTINCT FROM 'heap'
                    OR NOT has_schema_privilege(target.schema_oid, 'USAGE')
                    OR NOT has_table_privilege(target.oid, 'SELECT') THEN
                    RAISE EXCEPTION 'baseline coverage refused';
                END IF;
                probes := array_append(probes,
                    format('SELECT EXISTS (SELECT FROM ONLY %I.%I)', target.nspname, target.relname));
            END IF;
        END IF;
    END LOOP;
    FOREACH probe IN ARRAY probes LOOP
        EXECUTE probe INTO found_row;
        occupied := occupied OR found_row;
    END LOOP;
    SELECT EXISTS (SELECT FROM pg_largeobject_metadata) OR occupied INTO occupied;
    PERFORM set_config('nntune.baseline', CASE WHEN occupied THEN 'rows-present' ELSE 'empty' END, true);
END
$baseline$;
SELECT current_setting('nntune.baseline');
COMMIT;
""".encode("ascii")


def baseline():
    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            signal.signal(signum, remember_signal)
        if os.environ.get("BASELINE_ENABLED") != "true":
            return "refused"
        raw = read_authorized_plan(os.environ, workflow=WORKFLOW)
        if raw is None:
            return "refused"
        deadline = time.monotonic() + min(MAX_SECONDS, int(os.environ["BOOTSTRAP_EXPIRY"]) - time.time())
        plan = baseline_plan(raw)
        password = os.environ["BASELINE_READER_PASSWORD"]
        sql = baseline_sql(plan)
        if not 0 < len(password.encode("utf-8")) <= 1024 or not password.isprintable() or len(sql) > MAX_SQL_BYTES:
            return "refused"
        with tempfile.TemporaryDirectory(prefix="database-baseline-ca-") as private:
            env = tls_environment(private, plan, plan.launch, f"{plan.reader_role}.{plan.branch_id}", password)
            result = run_sql(sql, env, deadline)
        return {b"empty\n": "empty", b"rows-present\n": "rows-present"}.get(result, "refused")
    except Exception:
        return "refused"


if __name__ == "__main__":
    verdict = baseline()
    print(f"database-bootstrap-baseline: {verdict}", file=sys.stderr if verdict == "refused" else sys.stdout)
    sys.exit(1 if verdict == "refused" else 0)
