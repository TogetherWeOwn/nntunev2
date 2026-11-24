"""Read-only catalog precondition for a future protected database bootstrap.

The protected caller must authenticate and pin the branch and direct TLS
connection before running this SQL. A passing probe observes the catalog at
one point in time.
"""

import re

from apply_migration import quote_literal


class BootstrapPreflightError(Exception):
    pass


def catalog_probe_sql(maintenance_database, launch_database, creator_role):
    """Build one bounded probe for the actual PostgreSQL session identity."""
    if not all(re.fullmatch(r"[a-z_][a-z0-9_]{0,62}", name)
               for name in (maintenance_database, launch_database)):
        raise BootstrapPreflightError("expected plain lower-case database names")
    if launch_database in (maintenance_database, "postgres", "template0", "template1"):
        raise BootstrapPreflightError("launch database must be a dedicated target")
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,63}", creator_role):
        raise BootstrapPreflightError("expected a plain SQL creator role name")

    return f"""BEGIN READ ONLY;
SET LOCAL search_path = pg_catalog;
SET LOCAL statement_timeout = '10s';
SET LOCAL lock_timeout = '2s';
SELECT CASE
    WHEN current_database() <> {quote_literal(maintenance_database)}
      OR session_user <> {quote_literal(creator_role)}
      OR current_user <> {quote_literal(creator_role)}
        THEN 'wrong-session'
    WHEN EXISTS (
        SELECT FROM pg_catalog.pg_database
        WHERE datname = {quote_literal(launch_database)}
    ) THEN 'already-exists'
    WHEN NOT EXISTS (
        SELECT FROM pg_catalog.pg_roles
        WHERE rolname = current_user AND rolcanlogin AND rolcreatedb
    ) THEN 'creator-ineligible'
    WHEN NOT EXISTS (
        SELECT FROM pg_catalog.pg_database
        WHERE datname = 'template0' AND datistemplate AND NOT datallowconn
    ) THEN 'template-unavailable'
    ELSE 'name-absent'
END;
COMMIT;
"""


def require_absent_catalog_result(returncode, stdout):
    """Accept exactly one verdict from a successful quiet, unaligned psql run."""
    if returncode != 0:
        raise BootstrapPreflightError("catalog probe failed; database output redacted")
    if stdout == "name-absent\n":
        return
    reasons = {
        "wrong-session\n": "connected database or role differs from the approved session",
        "already-exists\n": "launch name already exists; stop and verify its identity and state",
        "creator-ineligible\n": "approved session role lacks LOGIN or CREATEDB",
        "template-unavailable\n": "expected template0 catalog entry is unavailable",
    }
    raise BootstrapPreflightError(reasons.get(stdout, "unexpected catalog probe output"))
