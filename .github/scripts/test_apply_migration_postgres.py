#!/usr/bin/env python3
"""Exercise the migrator end-to-end on the disposable PostgreSQL CI service.

The atomicity claim of ``apply_migration.py`` — advisory lock, database
pin, prior-history guard, migration body and tracking insert committing
or rolling back as one — rests on psql's non-interactive
``--single-transaction`` behaviour, which the unit tests (``compose()``
only) cannot prove and which floats with the distro psql client. This
test drives the real ``main()`` / ``psql()`` path against a throwaway
database and proves:

- a wrong expected database creates nothing, not even the tracking
  table (DDL step), and records nothing via ``--record-only`` either;
- applying out of manifest order refuses until priors are recorded;
- the advisory lock is held while the migration body runs;
- a mid-migration failure leaves neither schema change nor tracking row;
- re-running the DDL revokes a stray non-owner write grant, table-level
  or column-level (``pg_attribute.attacl``).

No provider calls or deployed databases: require the workflow's exact
throwaway endpoint, create a fresh fixture database, and refuse to
reuse an existing one.
"""
import hashlib
import os
import pathlib
import subprocess
import tempfile

import apply_migration


TEST_URL = "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci"
TEST_DB = "nntune_apply_test"
OTHER_DB = "nntune_ci"

ARGS = ["psql", "--no-psqlrc", "--no-password", "--quiet", "--tuples-only",
        "--no-align", "--set", "ON_ERROR_STOP=1"]


def run_psql(env, sql):
    result = subprocess.run(ARGS, input=sql, env=env, text=True,
                            capture_output=True, timeout=15)
    if result.returncode != 0:
        # Never print connection details or database stderr.
        raise RuntimeError("apply fixture PostgreSQL execution failed")
    return result.stdout.strip()


def scalar(env, sql):
    return run_psql(env, sql)


def assert_psql_fails(env, sql):
    result = subprocess.run(ARGS, input=sql, env=env, text=True,
                            capture_output=True, timeout=15)
    # Never print connection details or database stderr.
    assert result.returncode != 0, "expected psql to fail, but it succeeded"


MIGRATIONS = {
    "0002_alpha.sql": "CREATE TABLE alpha (id int);\n",
    # Records, from inside the migration body, whether this backend
    # holds a granted advisory lock — true only if the guard's
    # pg_advisory_xact_lock ran in the same transaction before the body.
    "0003_beta.sql": (
        "CREATE TABLE beta (id int);\n"
        "CREATE TABLE lock_probe (held boolean NOT NULL);\n"
        "INSERT INTO lock_probe SELECT EXISTS (\n"
        "    SELECT 1 FROM pg_locks\n"
        "    WHERE locktype = 'advisory' AND granted AND pid = pg_backend_pid());\n"
    ),
    "0004_broken.sql": "CREATE TABLE should_not_exist (id int);\nSELECT 1 / 0;\n",
    "0005_own_txn.sql": "BEGIN;\nCREATE TABLE own_txn (id int);\nCOMMIT;\n",
}


class Fixture:
    def __init__(self, root):
        self.root = root
        manifest = ""
        for name in sorted(MIGRATIONS):
            (root / name).write_text(MIGRATIONS[name], encoding="utf-8")
            sha = hashlib.sha256(MIGRATIONS[name].encode()).hexdigest()
            manifest += (f'[[migration]]\nfile = "{name}"\ntarget = "app"\n'
                         f'sha256 = "{sha}"\n')
        self.manifest = root / "manifest.toml"
        self.manifest.write_text(manifest, encoding="utf-8")

    def apply(self, name, expect_database=TEST_DB, record_only=False, database=TEST_DB):
        old = os.environ.get("DATABASE_URL")
        os.environ["DATABASE_URL"] = (
            f"postgresql://postgres:ci-throwaway@localhost:5432/{database}")
        argv = ["--file", str(self.root / name),
                "--manifest", str(self.manifest),
                "--applied-by", "apply-fixture",
                "--expect-database", expect_database]
        if record_only:
            argv.append("--record-only")
        try:
            return apply_migration.main(argv)
        finally:
            if old is None:
                del os.environ["DATABASE_URL"]
            else:
                os.environ["DATABASE_URL"] = old


def recorded_files(env):
    return scalar(env, "SELECT string_agg(file, ',' ORDER BY file) "
                       "FROM public.schema_migrations;")


def main():
    if (os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("APPLY_MIGRATION_TEST_DATABASE_URL") != TEST_URL):
        raise RuntimeError("requires the exact disposable PostgreSQL CI service")
    env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "LANG", "LC_ALL")}
    env.update({
        "PGHOST": "localhost", "PGPORT": "5432", "PGUSER": "postgres",
        "PGPASSWORD": "ci-throwaway", "PGDATABASE": "nntune_ci",
        "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5",
    })
    # No IF NOT EXISTS: an existing database must never become our test target.
    run_psql(env, f"CREATE DATABASE {TEST_DB};")
    test_env = {**env, "PGDATABASE": TEST_DB}
    try:
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Fixture(pathlib.Path(tmp))

            # Wrong expected database: the guarded DDL step must leave
            # no tracking table behind in the connected database.
            assert fixture.apply("0002_alpha.sql", expect_database=OTHER_DB) == 1
            assert scalar(test_env, "SELECT to_regclass('public.schema_migrations') IS NULL;") == "t"
            assert scalar(test_env, "SELECT to_regclass('public.alpha') IS NULL;") == "t"
            print("apply-migration-postgres: passed wrong-database-ddl")

            # Out of order: 0003 refuses until 0002 is recorded.
            assert fixture.apply("0003_beta.sql") == 1
            assert scalar(test_env, "SELECT to_regclass('public.beta') IS NULL;") == "t"
            assert recorded_files(test_env) == ""
            print("apply-migration-postgres: passed out-of-order-refused")

            # In order: both apply, and the lock probe recorded from
            # inside 0003's body proves the advisory lock was held
            # while the body ran, in the same transaction.
            assert fixture.apply("0002_alpha.sql") == 0
            assert fixture.apply("0003_beta.sql") == 0
            assert recorded_files(test_env) == "0002_alpha.sql,0003_beta.sql"
            assert scalar(test_env, "SELECT held FROM lock_probe;") == "t"
            print("apply-migration-postgres: passed in-order-and-lock-held")

            # Already recorded is a no-op, not a failure.
            assert fixture.apply("0002_alpha.sql") == 0
            assert recorded_files(test_env) == "0002_alpha.sql,0003_beta.sql"
            print("apply-migration-postgres: passed already-recorded-noop")

            # Mid-migration failure: neither the schema change nor the
            # tracking row survives (the --single-transaction claim).
            assert fixture.apply("0004_broken.sql") == 1
            assert scalar(test_env, "SELECT to_regclass('public.should_not_exist') IS NULL;") == "t"
            assert recorded_files(test_env) == "0002_alpha.sql,0003_beta.sql"
            print("apply-migration-postgres: passed mid-migration-failure-atomic")

            # --record-only against the wrong database records nothing:
            # drive compose()+psql() directly so the in-transaction
            # current_database() check (not main's URL precheck) is
            # what refuses.
            sql, *_ = apply_migration.compose(
                fixture.root / "0004_broken.sql", fixture.manifest, "apply-fixture",
                record_only=True, expect_database=OTHER_DB)
            try:
                apply_migration.psql(test_env, sql, single_transaction=True)
            except apply_migration.ApplyError as exc:
                assert "does not match --expect-database" in str(exc), exc
            else:
                raise AssertionError("record-only against wrong database was accepted")
            assert recorded_files(test_env) == "0002_alpha.sql,0003_beta.sql"
            print("apply-migration-postgres: passed record-only-wrong-database")

            # Record-only happy path for a file applied by hand.
            run_psql(test_env, MIGRATIONS["0005_own_txn.sql"])
            assert fixture.apply("0005_own_txn.sql", record_only=True) == 1, \
                "0005 must refuse while 0004 is unrecorded"
            assert fixture.apply("0004_broken.sql", record_only=True) == 0
            assert fixture.apply("0005_own_txn.sql", record_only=True) == 0
            assert recorded_files(test_env) == (
                "0002_alpha.sql,0003_beta.sql,0004_broken.sql,0005_own_txn.sql")
            print("apply-migration-postgres: passed record-only-in-order")

            # Privilege pinning: a stray non-owner write grant is
            # revoked by the next DDL execution; read grants survive.
            run_psql(test_env, "GRANT INSERT, SELECT ON public.schema_migrations TO PUBLIC;")
            apply_migration.psql(
                test_env, apply_migration.ddl_sql(TEST_DB), single_transaction=True)
            grants = scalar(test_env, """
                SELECT string_agg(a.privilege_type, ',' ORDER BY a.privilege_type)
                FROM pg_class c CROSS JOIN aclexplode(c.relacl) a
                WHERE c.oid = to_regclass('public.schema_migrations')
                  AND a.grantee = 0;""")
            assert grants == "SELECT", f"expected only SELECT for PUBLIC, got {grants!r}"
            print("apply-migration-postgres: passed write-privileges-pinned")

            # Column-level pinning: column privileges live in
            # pg_attribute.attacl, invisible to the relacl pass. A role
            # holding ONLY column INSERT can forge a tracking row
            # (applied_at defaults) until the next DDL run revokes it;
            # column SELECT must survive. Roles are cluster-global, so
            # the scenario drops its role again in the finally.
            run_psql(test_env, "CREATE ROLE apply_column_sneak;")
            try:
                run_psql(test_env, """
                    GRANT INSERT (file, sha256, applied_by)
                        ON public.schema_migrations TO apply_column_sneak;
                    GRANT SELECT (file)
                        ON public.schema_migrations TO apply_column_sneak;
                    GRANT UPDATE (applied_by)
                        ON public.schema_migrations TO PUBLIC;""")
                forged = ("SET ROLE apply_column_sneak;\n"
                          "INSERT INTO public.schema_migrations "
                          "(file, sha256, applied_by) VALUES "
                          f"('0006_forged.sql', '{'f' * 64}', 'sneak');")
                # Pre-DDL the forged insert succeeds, so the post-DDL
                # failure below cannot pass vacuously.
                run_psql(test_env, forged)
                run_psql(test_env, "DELETE FROM public.schema_migrations "
                                   "WHERE file = '0006_forged.sql';")
                apply_migration.psql(
                    test_env, apply_migration.ddl_sql(TEST_DB),
                    single_transaction=True)
                write_grants = scalar(test_env, """
                    SELECT count(*) FROM pg_class c
                    JOIN pg_attribute t ON t.attrelid = c.oid
                    CROSS JOIN aclexplode(t.attacl) a
                    WHERE c.oid = to_regclass('public.schema_migrations')
                      AND a.privilege_type IN ('INSERT', 'UPDATE', 'REFERENCES')
                      AND a.grantee <> c.relowner;""")
                assert write_grants == "0", \
                    f"expected no non-owner column write grants, got {write_grants!r}"
                sneak_grants = scalar(test_env, """
                    SELECT string_agg(DISTINCT a.privilege_type, ',')
                    FROM pg_attribute t CROSS JOIN aclexplode(t.attacl) a
                    WHERE t.attrelid = to_regclass('public.schema_migrations')
                      AND a.grantee = (SELECT oid FROM pg_roles
                                       WHERE rolname = 'apply_column_sneak');""")
                assert sneak_grants == "SELECT", \
                    f"expected only SELECT for the sneak role, got {sneak_grants!r}"
                assert_psql_fails(test_env, forged)
                assert recorded_files(test_env) == (
                    "0002_alpha.sql,0003_beta.sql,0004_broken.sql,0005_own_txn.sql")
            finally:
                run_psql(test_env, "DROP OWNED BY apply_column_sneak;\n"
                                   "DROP ROLE apply_column_sneak;")
            print("apply-migration-postgres: passed column-write-privileges-pinned")
    finally:
        run_psql(env, f"DROP DATABASE {TEST_DB};")


if __name__ == "__main__":
    main()
