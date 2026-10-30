#!/usr/bin/env python3
"""Execute the production schema gate on the disposable PostgreSQL CI service.

No provider calls or deployed databases: require the workflow's exact throwaway
endpoint, create a fresh fixture database, and refuse to reuse an existing one.
The deployment SQL and its verdict parser run unchanged; only provider discovery
and the connection environment are replaced with this isolated fixture.
"""
import os
import subprocess
from unittest.mock import patch

import staging_api_schema_gate as gate


TEST_URL = "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci"
PUBLIC_TABLE = """
CREATE TABLE public.sessions (
    id bigint,
    refresh_retry_hash text,
    refresh_issued_at bigint,
    refresh_claimed_at bigint
);
"""


def shadow(ddl):
    return (
        "CREATE SCHEMA schema_gate_fixture;\n"
        "SET search_path = schema_gate_fixture, public;\n" + ddl,
        "DROP SCHEMA schema_gate_fixture CASCADE;\n",
    )


# Cover both table-like objects and index/composite name-resolution shadows.
# Temp/system exceptions are deliberate; system fixtures use CI's superuser.
FIXTURES = {
    "public-only": ("", "", True),
    "other-name": (*shadow("CREATE TABLE schema_gate_fixture.other (id bigint);"), True),
    "table": (*shadow("CREATE TABLE schema_gate_fixture.sessions (id bigint);"), False),
    "partitioned-table": (*shadow(
        "CREATE TABLE schema_gate_fixture.sessions (id bigint) PARTITION BY RANGE (id);"
    ), False),
    "view": (*shadow("CREATE VIEW schema_gate_fixture.sessions AS SELECT 1 AS id;"), False),
    "materialized-view": (*shadow(
        "CREATE MATERIALIZED VIEW schema_gate_fixture.sessions AS SELECT 1 AS id;"
    ), False),
    "sequence": (*shadow("CREATE SEQUENCE schema_gate_fixture.sessions;"), False),
    "index": (*shadow(
        "CREATE TABLE schema_gate_fixture.anchor (id bigint);\n"
        "CREATE INDEX sessions ON schema_gate_fixture.anchor (id);"
    ), False),
    "partitioned-index": (*shadow(
        "CREATE TABLE schema_gate_fixture.anchor (id bigint) PARTITION BY RANGE (id);\n"
        "CREATE INDEX sessions ON schema_gate_fixture.anchor (id);"
    ), False),
    "composite": (*shadow("CREATE TYPE schema_gate_fixture.sessions AS (id bigint);"), False),
    "temp-exception": (
        "CREATE TEMP TABLE sessions (id bigint);", "", True,
    ),
    "information-schema-exception": (
        "CREATE VIEW information_schema.sessions AS SELECT 1 AS id;",
        "DROP VIEW information_schema.sessions;", True,
    ),
    "catalog-exception": (
        "SET allow_system_table_mods = on;\n"
        "CREATE VIEW pg_catalog.sessions AS SELECT 1 AS id;",
        "DROP VIEW pg_catalog.sessions;", True,
    ),
    "missing-column": ("ALTER TABLE public.sessions DROP COLUMN refresh_claimed_at;", "", False),
    "wrong-type": ("ALTER TABLE public.sessions ALTER COLUMN refresh_claimed_at TYPE text;", "", False),
    "not-null": ("ALTER TABLE public.sessions ALTER COLUMN refresh_claimed_at SET NOT NULL;", "", False),
}


def psql(args, **kwargs):
    result = subprocess.run(args, **kwargs)
    if result.returncode != 0:
        # A connection, setup or SQL error is never a successful negative test
        # or mutant kill. Do not print connection details or database stderr.
        raise RuntimeError("schema fixture PostgreSQL execution failed")
    return result


def assert_verdict(env, name):
    setup, cleanup, expected = FIXTURES[name]
    synthetic = {
        "CLOUDFLARE_API_TOKEN": "synthetic-token",
        "CLOUDFLARE_ACCOUNT_ID": "a" * 32,
        "HYPERDRIVE_ID_STAGING": "b" * 32,
        "PLANETSCALE_MIGRATOR_URL_STAGING":
            "postgresql://fixture:synthetic-password@database.example.test/nntunev2_staging",
    }
    payload = {"success": True, "result": {"origin": {
        "scheme": "postgres", "host": "database.example.test", "port": 5432,
        "database": "nntunev2_staging",
    }}}

    def run(args, **options):
        # One session preserves temporary-relation fixtures. psql --quiet keeps
        # setup/teardown command tags out of the production single-verdict parser.
        options["env"] = env
        options["input"] = (
            PUBLIC_TABLE + setup + "\n" + options["input"] + "\n" + cleanup
            + "\nDROP TABLE public.sessions;\n"
        )
        return psql(args, **options)

    try:
        gate.check(synthetic, fetch=lambda _: payload, run=run)
        actual = True
    except gate.GateError:
        actual = False
    assert actual == expected, f"{name}: expected verdict {expected}, got {actual}"


def main():
    if (os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("SCHEMA_GATE_TEST_DATABASE_URL") != TEST_URL):
        raise RuntimeError("requires the exact disposable PostgreSQL CI service")
    env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "LANG", "LC_ALL")}
    env.update({
        "PGHOST": "localhost", "PGPORT": "5432", "PGUSER": "postgres",
        "PGPASSWORD": "ci-throwaway", "PGDATABASE": "nntune_ci",
        "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5",
        "PGOPTIONS": "-c statement_timeout=5000 -c lock_timeout=1000",
    })
    args = ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
            "--no-align", "--set", "ON_ERROR_STOP=1"]
    # No IF NOT EXISTS: an existing database must never become our test target.
    psql(args, input="CREATE DATABASE nntunev2_staging;", env=env,
         text=True, capture_output=True, timeout=15)
    fixture_env = {**env, "PGDATABASE": "nntunev2_staging"}
    try:
        for name in FIXTURES:
            assert_verdict(fixture_env, name)
            print(f"staging-schema-postgres: passed {name}")

        mutations = (
            ("inverted exclusion", "AND NOT EXISTS", "AND EXISTS", "public-only"),
            *((f"omitted {fixture} kind", "WHERE c.relname='sessions'",
               "WHERE c.relname='sessions' AND c.relkind IN ('r','p','v','m','f','S')", fixture)
              for fixture in ("index", "partitioned-index", "composite")),
        )
        for name, before, after, fixture in mutations:
            assert gate.SCHEMA_SQL.count(before) == 1, f"{name}: mutation anchor changed"
            with patch.object(gate, "SCHEMA_SQL", gate.SCHEMA_SQL.replace(before, after, 1)):
                try:
                    assert_verdict(fixture_env, fixture)
                except AssertionError as error:
                    assert str(error).startswith(f"{fixture}: expected verdict "), error
                else:
                    raise AssertionError(f"{name}: mutant survived")
            # Also recheck the baseline after restoration; no disk source changes.
            assert_verdict(fixture_env, fixture)
            print(f"staging-schema-postgres: killed {name} (verdict assertion; source restored)")
    finally:
        psql(args, input="DROP DATABASE nntunev2_staging;", env=env,
             text=True, capture_output=True, timeout=15)


if __name__ == "__main__":
    main()
