"""Dormant SQL for a protected setup-role custodian. No connection entry point."""

import re
from datetime import datetime, timezone

from database_bootstrap_preflight import catalog_probe_sql
from database_bootstrap_setup import setup_plan


class SetupRoleLifecycle:
    """Use verified direct TLS and exclusive custody for a single approved plan.

    Require successful SQL completion as well as the expected result. Keep the
    issuance OID private and reconcile an ambiguous commit before any mutation.
    """

    def __init__(self, raw_plan):
        plan = setup_plan(raw_plan)
        catalog_probe_sql(plan.maintenance, plan.launch, plan.setup_role)
        self.maintenance = plan.maintenance
        self.name = plan.setup_role
        self.role = f'"{self.name}"'
        self.owner = plan.owner_role
        self.fenced = "NOT rolcanlogin AND NOT rolcreatedb AND rolvaliduntil = 'epoch'::timestamptz"

    def _transaction(self, body, result=""):
        return f"""BEGIN;
SET LOCAL search_path = pg_catalog;
SET LOCAL statement_timeout = '10s';
SET LOCAL lock_timeout = '2s';
SET LOCAL password_encryption = 'scram-sha-256';
DO $lifecycle$
DECLARE target oid;
BEGIN
    IF current_database() <> '{self.maintenance}' OR session_user <> current_user
       OR current_user = '{self.name}'
       OR NOT EXISTS (SELECT FROM pg_stat_ssl WHERE pid = pg_backend_pid() AND ssl) THEN
        RAISE EXCEPTION 'wrong custodian session';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('{self.name}', 0));
    SELECT oid INTO target FROM pg_roles WHERE rolname = '{self.name}';
    {body}
END
$lifecycle$;
{result}
COMMIT;
"""

    def issue(self, expiry, password):
        # A generated 192-bit hex password fits the setup caller's secret contract.
        if (type(expiry) is not int or not 0 < expiry <= 253402300799
                or not re.fullmatch(r"[0-9a-f]{48}", password)):
            raise ValueError("invalid issuance inputs")
        # VALID UNTIL accepts a literal, unlike the timestamp comparison below.
        until = datetime.fromtimestamp(expiry, timezone.utc).isoformat()
        exact = f"""rolcanlogin AND rolcreatedb AND NOT rolinherit AND rolconnlimit = 1
           AND NOT (rolsuper OR rolcreaterole OR rolreplication OR rolbypassrls)
           AND rolvaliduntil = to_timestamp({expiry})"""
        return self._transaction(f"""
    IF target IS NOT NULL OR to_timestamp({expiry}) <= clock_timestamp()
       OR to_timestamp({expiry}) > clock_timestamp() + interval '2 hours'
       OR NOT EXISTS (
           SELECT FROM pg_roles r WHERE rolname = '{self.owner}'
             AND NOT (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls)
             AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = r.oid)
       ) THEN
        RAISE EXCEPTION 'issuance refused';
    END IF;
    CREATE ROLE {self.role} LOGIN CREATEDB NOINHERIT NOSUPERUSER NOCREATEROLE
        NOREPLICATION NOBYPASSRLS CONNECTION LIMIT 1 PASSWORD '{password}' VALID UNTIL '{until}';
    GRANT "{self.owner}" TO {self.role} WITH ADMIN FALSE, INHERIT FALSE, SET TRUE;
    SELECT oid INTO STRICT target FROM pg_roles WHERE rolname = '{self.name}' AND {exact};
    IF (SELECT count(*) FROM pg_auth_members WHERE member = target) <> 1
       OR NOT EXISTS (
           SELECT FROM pg_auth_members m JOIN pg_roles r ON r.oid = m.roleid
           WHERE m.member = target AND r.rolname = '{self.owner}'
             AND m.set_option AND NOT (m.inherit_option OR m.admin_option)
       ) THEN
        RAISE EXCEPTION 'issuance readback failed';
    END IF;
""", f"SELECT oid FROM pg_roles WHERE rolname = '{self.name}';")

    def _cleanup(self, oid, action, *, require_fence=True, result=""):
        if type(oid) is not int or not 0 < oid <= 4294967295:
            raise ValueError("invalid issuance receipt")
        fence_check = f"OR NOT EXISTS (SELECT FROM pg_roles WHERE oid = target AND {self.fenced})"
        return self._transaction(f"""
    IF target IS NOT NULL THEN
        IF target <> {oid}::oid {fence_check if require_fence else ''} THEN
            RAISE EXCEPTION 'cleanup refused';
        END IF;
        {action}
    END IF;
""", result)

    def fence(self, oid):
        # Commit before trying to terminate sessions. A later failure must leave
        # new logins disabled, even if the role still owns a fenced database.
        return self._cleanup(oid, f"""ALTER ROLE {self.role} NOLOGIN NOCREATEDB
            PASSWORD NULL VALID UNTIL 'epoch';""", require_fence=False)

    def retire(self, oid):
        # Signalling survives rollback. DROP ROLE refuses outstanding database
        # dependencies and removes memberships only when it succeeds.
        return self._cleanup(oid, f"""
        IF EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = target
                   AND NOT pg_terminate_backend(pid, 2000)) THEN
            RAISE EXCEPTION 'session termination failed';
        END IF;
        PERFORM pg_stat_clear_snapshot();
        IF EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = target) THEN
            RAISE EXCEPTION 'session closure unconfirmed';
        END IF;
        DROP ROLE {self.role};
""", f"""
SELECT 'retired' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE oid = {oid}::oid OR rolname = '{self.name}')
  AND NOT EXISTS (SELECT FROM pg_stat_activity WHERE usesysid = {oid}::oid);
""")
