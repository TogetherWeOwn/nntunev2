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
A_SHA, B_SHA = "a" * 64, "b" * 64  # renames are byte-identical: alias sha == entry sha
EXPECTED = [
    ("0002_alpha.sql", A_SHA, ()),
    ("0003_beta.sql", B_SHA, (("0001_beta_old.sql", B_SHA),)),
]
PUBLIC_TABLE = f"""
CREATE TABLE public.schema_migrations (
    file       TEXT PRIMARY KEY,
    sha256     TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    applied_by TEXT NOT NULL
);
INSERT INTO public.schema_migrations (file, sha256, applied_by) VALUES
    ('0002_alpha.sql', '{A_SHA}', 'fixture'),
    ('0003_beta.sql', '{B_SHA}', 'fixture');
"""
DROP_TABLE = "DROP TABLE IF EXISTS public.schema_migrations;\n"


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
    "table": (*shadow("CREATE TABLE schema_gate_fixture.schema_migrations (id bigint);"), False),
    "partitioned-table": (*shadow(
        "CREATE TABLE schema_gate_fixture.schema_migrations (id bigint) PARTITION BY RANGE (id);"
    ), False),
    "view": (*shadow("CREATE VIEW schema_gate_fixture.schema_migrations AS SELECT 1 AS id;"), False),
    "materialized-view": (*shadow(
        "CREATE MATERIALIZED VIEW schema_gate_fixture.schema_migrations AS SELECT 1 AS id;"
    ), False),
    "sequence": (*shadow("CREATE SEQUENCE schema_gate_fixture.schema_migrations;"), False),
    "index": (*shadow(
        "CREATE TABLE schema_gate_fixture.anchor (id bigint);\n"
        "CREATE INDEX schema_migrations ON schema_gate_fixture.anchor (id);"
    ), False),
    "partitioned-index": (*shadow(
        "CREATE TABLE schema_gate_fixture.anchor (id bigint) PARTITION BY RANGE (id);\n"
        "CREATE INDEX schema_migrations ON schema_gate_fixture.anchor (id);"
    ), False),
    "composite": (*shadow("CREATE TYPE schema_gate_fixture.schema_migrations AS (id bigint);"), False),
    "temp-exception": (
        "CREATE TEMP TABLE schema_migrations (id bigint);", "", True,
    ),
    "information-schema-exception": (
        "CREATE VIEW information_schema.schema_migrations AS SELECT 1 AS id;",
        "DROP VIEW information_schema.schema_migrations;", True,
    ),
    "catalog-exception": (
        "SET allow_system_table_mods = on;\n"
        "CREATE VIEW pg_catalog.schema_migrations AS SELECT 1 AS id;",
        "DROP VIEW pg_catalog.schema_migrations;", True,
    ),
    "missing-table": (DROP_TABLE, "", False),
    "missing-column": ("ALTER TABLE public.schema_migrations DROP COLUMN sha256;", "", False),
    "wrong-type": (
        "ALTER TABLE public.schema_migrations DROP COLUMN sha256;\n"
        "ALTER TABLE public.schema_migrations ADD COLUMN sha256 bigint;", "", False),
    "missing-row": (
        "DELETE FROM public.schema_migrations WHERE file = '0003_beta.sql';", "", False),
    "extra-row": (
        f"INSERT INTO public.schema_migrations (file, sha256, applied_by) "
        f"VALUES ('0004_extra.sql', '{'d' * 64}', 'fixture');", "", False),
    "sha-mismatch": (
        f"UPDATE public.schema_migrations SET sha256 = '{'e' * 64}' "
        "WHERE file = '0002_alpha.sql';", "", False),
    "renamed-row": (
        "DELETE FROM public.schema_migrations WHERE file = '0003_beta.sql';\n"
        f"INSERT INTO public.schema_migrations (file, sha256, applied_by) "
        f"VALUES ('0001_beta_old.sql', '{B_SHA}', 'fixture');", "", True),
    "double-apply": (
        f"INSERT INTO public.schema_migrations (file, sha256, applied_by) "
        f"VALUES ('0001_beta_old.sql', '{B_SHA}', 'fixture');", "", False),
    # Rows are deploy proof: a write grant to anyone but the owner must
    # fail the guard even when the rows themselves match the manifest.
    "non-owner-write-grant": (
        "GRANT INSERT ON public.schema_migrations TO PUBLIC;",
        "REVOKE INSERT ON public.schema_migrations FROM PUBLIC;", False),
    "non-owner-read-grant": (
        "GRANT SELECT ON public.schema_migrations TO PUBLIC;",
        "REVOKE SELECT ON public.schema_migrations FROM PUBLIC;", True),
    # Column-level write grants live in pg_attribute.attacl, not relacl:
    # a named role holding ONLY column INSERT can still forge a row
    # (applied_at defaults), so the guard must reject it too. Roles are
    # cluster-global, so cleanup revokes and drops them before the
    # table drop that follows.
    "non-owner-column-write-grant": (
        "CREATE ROLE column_gate_sneak;\n"
        "GRANT INSERT (file, sha256, applied_by) "
        "ON public.schema_migrations TO column_gate_sneak;",
        "REVOKE INSERT (file, sha256, applied_by) "
        "ON public.schema_migrations FROM column_gate_sneak;\n"
        "DROP ROLE column_gate_sneak;", False),
    "non-owner-column-read-grant": (
        "CREATE ROLE column_gate_reader;\n"
        "GRANT SELECT (file, sha256) "
        "ON public.schema_migrations TO column_gate_reader;",
        "REVOKE SELECT (file, sha256) "
        "ON public.schema_migrations FROM column_gate_reader;\n"
        "DROP ROLE column_gate_reader;", True),
}

ARGS = ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
        "--no-align", "--set", "ON_ERROR_STOP=1"]


def psql(args, **kwargs):
    result = subprocess.run(args, **kwargs)
    if result.returncode != 0:
        # A connection, setup or SQL error is never a successful negative test
        # or mutant kill. Do not print connection details or database stderr.
        raise RuntimeError("schema fixture PostgreSQL execution failed")
    return result


def exec_sql(env, sql):
    psql(ARGS, input=sql, env=env, text=True, capture_output=True, timeout=15)


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
        options["env"] = env
        if name == "temp-exception" and "pg_class" in options.get("input", ""):
            # A temp table exists only in its own session, so running the
            # setup through a separate exec_sql session would make this
            # fixture vacuous: the guard would never see the pg_temp
            # relation whose exclusion it is meant to exercise. Prepend
            # the CREATE TEMP TABLE to the guard's own psql script (the
            # one carrying the pg_class guard query), so the temp shadow
            # is live in the session that runs GUARD_SQL.
            options["input"] = setup + "\n" + options["input"]
        return psql(args, **options)

    exec_sql(env, PUBLIC_TABLE + ("" if name == "temp-exception" else setup))
    try:
        try:
            gate.check(synthetic, fetch=lambda _: payload, run=run, expected=EXPECTED)
            actual = True
        except gate.CategorizedFailure:
            actual = False
    finally:
        exec_sql(env, cleanup + DROP_TABLE)
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
    # No IF NOT EXISTS: an existing database must never become our test target.
    exec_sql(env, "CREATE DATABASE nntunev2_staging;")
    fixture_env = {**env, "PGDATABASE": "nntunev2_staging"}
    try:
        for name in FIXTURES:
            assert_verdict(fixture_env, name)
            print(f"staging-schema-postgres: passed {name}")

        mutations = (
            ("inverted exclusion", "AND NOT EXISTS (SELECT 1 FROM pg_class c JOIN",
             "AND EXISTS (SELECT 1 FROM pg_class c JOIN", "public-only"),
            # Dropping the pg_temp exclusion must turn the same-session
            # temp shadow into a guard failure; if this mutant survives,
            # the temp-exception fixture is not exercising the exclusion.
            ("dropped pg_temp exclusion",
             "AND n.nspname NOT LIKE 'pg\\_temp\\_%'", "AND true", "temp-exception"),
            # Dropping the non-owner write-grant exclusion must let a
            # PUBLIC write grant pass the guard. Anchors carry their
            # privilege-type line so the table-level (relacl) and
            # column-level (attacl) exclusions stay distinguishable.
            ("dropped write-grant exclusion",
             "AND a.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER')\n"
             "                 AND a.grantee <> c.relowner",
             "AND a.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE','REFERENCES','TRIGGER')\n"
             "                 AND false",
             "non-owner-write-grant"),
            # Same for the column-level exclusion: dropping it must let
            # a role holding only column INSERT pass the guard.
            ("dropped column write-grant exclusion",
             "AND a.privilege_type IN ('INSERT','UPDATE','REFERENCES')\n"
             "                 AND a.grantee <> c.relowner",
             "AND a.privilege_type IN ('INSERT','UPDATE','REFERENCES')\n"
             "                 AND false",
             "non-owner-column-write-grant"),
            *((f"omitted {fixture} kind", "WHERE c.relname='schema_migrations'",
               "WHERE c.relname='schema_migrations' AND c.relkind IN ('r','p','v','m','f','S')", fixture)
              for fixture in ("index", "partitioned-index", "composite")),
        )
        for name, before, after, fixture in mutations:
            assert gate.GUARD_SQL.count(before) == 1, f"{name}: mutation anchor changed"
            with patch.object(gate, "GUARD_SQL", gate.GUARD_SQL.replace(before, after, 1)):
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
        exec_sql(env, "DROP DATABASE nntunev2_staging;")


if __name__ == "__main__":
    main()
