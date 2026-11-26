"""Dormant SQL for the first-launch reader. No entry point or credential store."""

import re
from datetime import datetime, timezone

from database_bootstrap_baseline import MAX_RELATIONS, baseline_plan
from database_bootstrap_preflight import catalog_probe_sql
from database_bootstrap_role import DRAIN_SESSIONS, RoleLifecycle, role_oid, scram_verifier
from database_bootstrap_setup import setup_plan

MAX_READER_SECONDS = 120
USER_SCHEMA = """n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
    AND n.nspname !~ '^pg_(toast_)?temp_[0-9]+$'"""
PHYSICAL_RELATIONS = f"""
    SELECT c.oid, c.relnamespace, n.nspname, c.relname, c.relrowsecurity, c.relpersistence, am.amname
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_am am ON am.oid = c.relam
    WHERE {USER_SCHEMA} AND c.relkind IN ('r','m')
"""


class ReaderRoleLifecycle(RoleLifecycle):
    """The same pinned custodian prepares, activates, fences and retires the reader."""

    def __init__(self, raw_setup, issuer):
        self.plan = setup_plan(raw_setup)
        self.maintenance = self.plan.maintenance
        self.name = self.plan.reader_role
        catalog_probe_sql(self.maintenance, self.plan.launch, issuer)
        if (not re.fullmatch(r"[A-Za-z0-9_-]{1,63}", self.name)
                or issuer in (self.name, self.plan.setup_role, self.plan.owner_role, self.plan.migrator_role)):
            raise ValueError("invalid reader custody")
        self.issuer = issuer
        self.role = f'"{self.name}"'

    def _run(self, body, result="", *, plan=None, require_owner=False):
        database = self.maintenance
        guard = f"""OR pg_is_in_recovery() OR current_user <> '{self.issuer}'
           OR NOT EXISTS (SELECT FROM pg_roles WHERE rolname = current_user AND rolcreaterole AND NOT rolsuper)"""
        if plan is not None:
            database = plan.launch
            guard += f""" OR NOT EXISTS (
                SELECT FROM pg_database WHERE datname = current_database() AND oid = {role_oid(plan.database_oid)}
                    AND datallowconn AND NOT datistemplate
            )"""
            if require_owner:
                guard += f""" OR NOT EXISTS (
                    SELECT FROM pg_database d JOIN pg_roles r ON r.oid = d.datdba
                    WHERE d.datname = current_database() AND r.rolname = '{self.plan.owner_role}'
                )"""
        return self._transaction(body, result, database=database, guard=guard)

    def _bound_plan(self, raw, oid):
        plan = baseline_plan(raw)
        if plan.reader_oid != oid or any(getattr(plan, key) != getattr(self.plan, key)
               for key in ("host", "port", "ca_pem", "branch_id", "launch", "reader_role")):
            raise ValueError("baseline plan differs from setup or issuance")
        return plan

    def prepare(self):
        return self._run(f"""
    IF target IS NOT NULL OR EXISTS (SELECT FROM pg_database WHERE datname = '{self.plan.launch}') THEN
        RAISE EXCEPTION 'reader preparation refused';
    END IF;
    SET LOCAL createrole_self_grant = '';
    CREATE ROLE {self.role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT
        NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 1 PASSWORD NULL VALID UNTIL 'epoch';
""", f"SELECT oid FROM pg_roles WHERE rolname = '{self.name}';")

    def _pending(self, receipt):
        return f"""
    IF target IS DISTINCT FROM {receipt} OR NOT EXISTS (
        SELECT FROM pg_roles WHERE oid = target AND rolcanlogin AND rolconnlimit = 1 AND NOT
            (rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls)
            AND rolvaliduntil = 'epoch'::timestamptz
    ) OR EXISTS (
        SELECT FROM pg_auth_members WHERE member = target OR (
            roleid = target AND (member <> (SELECT oid FROM pg_roles WHERE rolname = current_user)
                                 OR NOT admin_option OR inherit_option OR set_option)
        )
    ) OR EXISTS (SELECT FROM pg_shdepend WHERE refclassid = 'pg_authid'::regclass AND refobjid = target)
    OR EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = target) THEN
        RAISE EXCEPTION 'reader is no longer pending';
    END IF;
"""

    def activate(self, raw_baseline, oid, expiry, password):
        receipt = role_oid(oid)
        if (type(expiry) is not int or not 0 < expiry <= 253402300799
                or not isinstance(password, str) or not re.fullmatch(r"[0-9a-f]{48}", password)):
            raise ValueError("invalid reader credential")
        until = datetime.fromtimestamp(expiry, timezone.utc).isoformat()
        return self._run(self._pending(receipt) + f"""
    IF to_timestamp({expiry}) <= clock_timestamp()
       OR to_timestamp({expiry}) > clock_timestamp() + interval '{MAX_READER_SECONDS} seconds'
       OR EXISTS (
           SELECT FROM pg_database d WHERE
               (d.datallowconn AND has_database_privilege(target, d.oid, 'CONNECT'))
               OR (d.datname = current_database() AND has_database_privilege(target, d.oid, 'CREATE,TEMP'))
       ) OR EXISTS (
           SELECT FROM pg_namespace n WHERE {USER_SCHEMA}
             AND has_schema_privilege(target, n.oid, 'USAGE,CREATE')
       ) OR EXISTS (
           SELECT FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
           WHERE {USER_SCHEMA} AND has_function_privilege(target, p.oid, 'EXECUTE')
       ) OR EXISTS (
           SELECT FROM pg_foreign_server s WHERE has_server_privilege(target, s.oid, 'USAGE')
       ) OR EXISTS (
           SELECT FROM pg_tablespace t WHERE has_tablespace_privilege(target, t.oid, 'CREATE')
       ) OR EXISTS (
           SELECT FROM pg_largeobject_metadata l, LATERAL aclexplode(l.lomacl) a
           WHERE a.grantee = 0
       ) OR NOT has_table_privilege(target, 'pg_largeobject_metadata', 'SELECT')
       OR has_table_privilege(target, 'pg_largeobject', 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR EXISTS (
           SELECT FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
           WHERE {USER_SCHEMA} AND CASE
               WHEN c.relkind = 'f' THEN true
               WHEN c.relkind = 'S' THEN has_sequence_privilege(target, c.oid, 'USAGE,SELECT,UPDATE')
               WHEN c.relkind IN ('r','m','p','v') THEN
                   has_any_column_privilege(target, c.oid, 'SELECT,INSERT,UPDATE,REFERENCES')
                   OR has_table_privilege(target, c.oid, 'DELETE,TRUNCATE,TRIGGER,MAINTAIN')
               ELSE false END
       ) OR (
           SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
           WHERE {USER_SCHEMA} AND c.relkind IN ('r','p','m','S','v')
       ) > {MAX_RELATIONS} THEN
        RAISE EXCEPTION 'reader scope refused';
    END IF;
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', current_database(), '{self.name}');
    IF NOT has_database_privilege(target, current_database(), 'CONNECT') THEN
        RAISE EXCEPTION 'reader CONNECT missing';
    END IF;
    FOR item IN {PHYSICAL_RELATIONS} LOOP
        IF item.relrowsecurity OR item.relpersistence = 't' OR item.amname IS DISTINCT FROM 'heap' THEN
            RAISE EXCEPTION 'reader coverage refused';
        END IF;
        IF NOT has_schema_privilege(target, item.relnamespace, 'USAGE') THEN
            EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', item.nspname, '{self.name}');
        END IF;
        EXECUTE format('GRANT SELECT ON TABLE %I.%I TO %I', item.nspname, item.relname, '{self.name}');
        IF NOT has_schema_privilege(target, item.relnamespace, 'USAGE')
           OR NOT has_table_privilege(target, item.oid, 'SELECT') THEN
            RAISE EXCEPTION 'reader coverage missing';
        END IF;
    END LOOP;
    IF to_timestamp({expiry}) <= clock_timestamp() THEN
        RAISE EXCEPTION 'reader authorization expired';
    END IF;
    ALTER ROLE {self.role} PASSWORD '{scram_verifier(password)}' VALID UNTIL '{until}';
""", plan=self._bound_plan(raw_baseline, oid), require_owner=True)

    def fence(self, oid):
        # The role is cluster-wide. Fence it even if the launch database is unavailable.
        receipt = role_oid(oid)
        return self._run(f"""
    IF target IS NOT NULL THEN
        IF target <> {receipt} THEN
            RAISE EXCEPTION 'reader fence refused';
        END IF;
        ALTER ROLE {self.role} NOLOGIN PASSWORD NULL VALID UNTIL 'epoch';
    END IF;
""")

    def _retirement_result(self, receipt):
        return f"""
SELECT 'retired' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE oid = {receipt} OR rolname = '{self.name}')
  AND NOT EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = {receipt});
"""

    def retirement_readback(self, raw_baseline, oid):
        """Read the retirement verdict on the bound launch database without changing the role."""
        return self._run("", self._retirement_result(role_oid(oid)), plan=self._bound_plan(raw_baseline, oid))

    def retire(self, raw_baseline, oid):
        receipt = role_oid(oid)
        plan = None if raw_baseline is None else self._bound_plan(raw_baseline, oid)
        # From maintenance, DROP ROLE itself refuses any surviving grant or ownership.
        revoke = f"""
        FOR item IN {PHYSICAL_RELATIONS} LOOP
            EXECUTE format('REVOKE SELECT ON TABLE %I.%I FROM %I', item.nspname, item.relname, '{self.name}');
        END LOOP;
        FOR item IN SELECT DISTINCT nspname FROM ({PHYSICAL_RELATIONS}) relations LOOP
            EXECUTE format('REVOKE USAGE ON SCHEMA %I FROM %I', item.nspname, '{self.name}');
        END LOOP;
        EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM %I', current_database(), '{self.name}');
""" if plan else ""
        return self._run(f"""
    IF target IS NOT NULL THEN
        IF target <> {receipt} OR NOT EXISTS (
            SELECT FROM pg_roles WHERE oid = target AND NOT rolcanlogin AND rolvaliduntil = 'epoch'::timestamptz
        ) THEN
            RAISE EXCEPTION 'reader retirement refused';
        END IF;
        {DRAIN_SESSIONS}
        {revoke}
        DROP ROLE {self.role};
    END IF;
""", self._retirement_result(receipt), plan=plan)
