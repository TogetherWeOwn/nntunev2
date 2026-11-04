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
- recorded rows under an identity name or a source-rename-history name
  that are not a permitted (file, sha256) pair, and duplicate accepted
  rows (current name plus alias), refuse before "already recorded", for
  normal and record-only applies, with migration effects and rows left
  unchanged, and history never satisfies a prior;
- re-running the DDL revokes a stray non-owner write grant, table-level
  or column-level (``pg_attribute.attacl``).

No provider calls or deployed databases: require the workflow's exact
throwaway endpoint, create a fresh fixture database, and refuse to
reuse an existing one.
"""
from concurrent.futures import ThreadPoolExecutor
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


HISTORY_MIGRATIONS = {
    "0010_hist.sql": "CREATE TABLE hist_current (id int);\n",
    "0011_first.sql": "CREATE TABLE hist_first (id int);\n",
    "0012_second.sql": "CREATE TABLE hist_second (id int);\n",
}
BAD_SHA = "f" * 64


def history_manifest(root):
    """Separate manifest: 0010 has an alias and a multi-hop history
    chain attached to it; 0011 (0012's prior) has an edge of its own."""
    edges = {
        "0010_hist.sql": [("0010_hist_hop.sql", "0010_hist_prior.sql"),
                          ("0010_hist_prior.sql", "0010_hist.sql")],
        "0011_first.sql": [("0011_first_old.sql", "0011_first.sql")],
    }
    manifest = ""
    for name in sorted(HISTORY_MIGRATIONS):
        (root / name).write_text(HISTORY_MIGRATIONS[name], encoding="utf-8")
        sha = hashlib.sha256(HISTORY_MIGRATIONS[name].encode()).hexdigest()
        manifest += (f'[[migration]]\nfile = "{name}"\ntarget = "app"\n'
                     f'sha256 = "{sha}"\n')
        if name == "0010_hist.sql":
            manifest += (f'renamed_from = [{{ file = "0010_hist_old.sql", '
                         f'sha256 = "{sha}" }}]\n')
        for old, new in edges.get(name, []):
            manifest += ("[[migration.source_rename_history]]\n"
                         f'parent = "{"a" * 40}"\ncommit = "{"b" * 40}"\n'
                         f'old_path = "migrations/{old}"\n'
                         f'new_path = "migrations/{new}"\n'
                         f'old_sha256 = "{"c" * 64}"\nnew_sha256 = "{"d" * 64}"\n')
    path = root / "history-manifest.toml"
    path.write_text(manifest, encoding="utf-8")
    return path


def history_sha(name):
    return hashlib.sha256(HISTORY_MIGRATIONS[name].encode()).hexdigest()


def all_rows(env):
    return scalar(env, "SELECT coalesce(string_agg(file || ':' || sha256, ',' "
                       "ORDER BY file), '') FROM public.schema_migrations;")


def run_history_cases(fixture, manifest, test_env):
    def apply(name, record_only=False):
        saved = fixture.manifest
        fixture.manifest = manifest
        try:
            return fixture.apply(name, record_only=record_only)
        finally:
            fixture.manifest = saved

    def seed(*rows):
        run_psql(test_env, "".join(
            "INSERT INTO public.schema_migrations (file, sha256, applied_by) "
            f"VALUES ('{file}', '{sha}', 'seed');\n" for file, sha in rows))

    def clear():
        run_psql(test_env, "DELETE FROM public.schema_migrations "
                           "WHERE file >= '0010';")

    def refused(name, table, rows, record_only=False):
        seed(*rows)
        before = all_rows(test_env)
        assert apply(name, record_only=record_only) == 1, (name, rows)
        assert all_rows(test_env) == before, "refusal changed recorded rows"
        assert scalar(test_env, f"SELECT to_regclass('public.{table}') IS NULL;") == "t"
        clear()

    base = all_rows(test_env)
    good = history_sha("0010_hist.sql")

    # Different-byte historical row under a source-history name.
    refused("0010_hist.sql", "hist_current", [("0010_hist_prior.sql", BAD_SHA)])
    print("apply-migration-postgres: passed history-different-bytes-refused")

    # A good current row must not hide a conflicting historical row:
    # the deny check runs before "already recorded" (which is exit 0).
    refused("0010_hist.sql", "hist_current",
            [("0010_hist.sql", good), ("0010_hist_prior.sql", BAD_SHA)])
    print("apply-migration-postgres: passed good-current-bad-history-refused")

    # record-only takes the same guard.
    refused("0010_hist.sql", "hist_current",
            [("0010_hist_prior.sql", BAD_SHA)], record_only=True)
    refused("0010_hist.sql", "hist_current",
            [("0010_hist.sql", good), ("0010_hist_prior.sql", BAD_SHA)],
            record_only=True)
    print("apply-migration-postgres: passed history-record-only-refused")

    # Current name plus alias both recorded: more than one accepted row.
    refused("0010_hist.sql", "hist_current",
            [("0010_hist.sql", good), ("0010_hist_old.sql", good)])
    print("apply-migration-postgres: passed current-and-alias-duplicate-refused")

    # Multi-hop: an intermediate path that is no alias is still denied.
    refused("0010_hist.sql", "hist_current", [("0010_hist_hop.sql", BAD_SHA)])

    # Right bytes under a history name are still not an identity.
    refused("0010_hist.sql", "hist_current", [("0010_hist_prior.sql", good)])

    # A good prior cannot hide conflicting prior history, and history
    # rows never satisfy the prior.
    first = history_sha("0011_first.sql")
    refused("0012_second.sql", "hist_second",
            [("0011_first.sql", first), ("0011_first_old.sql", BAD_SHA)])
    refused("0012_second.sql", "hist_second", [("0011_first_old.sql", first)])
    print("apply-migration-postgres: passed prior-not-supplied-by-history")
    refused("0012_second.sql", "hist_second",
            [("0010_hist.sql", good), ("0010_hist_old.sql", good),
             ("0011_first.sql", first)])
    print("apply-migration-postgres: passed duplicate-prior-refused")

    assert all_rows(test_env) == base
    # Positive controls: with clean rows the same applies succeed, and
    # an alias row alone is accepted.
    assert apply("0010_hist.sql") == 0
    assert apply("0011_first.sql") == 0
    assert apply("0012_second.sql") == 0
    assert scalar(test_env, "SELECT to_regclass('public.hist_second') IS NOT NULL;") == "t"
    clear()
    run_psql(test_env, "DROP TABLE hist_current, hist_first, hist_second;")
    seed(("0010_hist_old.sql", good))
    assert apply("0010_hist.sql") == 0, "alias-recorded identity is an already-recorded no-op"
    assert all_rows(test_env).endswith(f"0010_hist_old.sql:{good}")
    clear()
    print("apply-migration-postgres: passed history-positive-controls")


def run_concurrent_apply(root, env):
    from threading import Barrier

    name = "0020_concurrent.sql"
    body = "SELECT pg_sleep(0.25);\nCREATE TABLE concurrent_probe (id int);\n"
    (root / name).write_text(body, encoding="utf-8")
    sha = hashlib.sha256(body.encode()).hexdigest()
    manifest = root / "concurrent.toml"
    manifest.write_text(f'[[migration]]\nfile = "{name}"\ntarget = "app"\nsha256 = "{sha}"\n')
    sql, *_ = apply_migration.compose(root / name, manifest, "concurrency-fixture",
                                      expect_database=TEST_DB)
    start = Barrier(2, timeout=10)

    def apply(_index):
        start.wait()
        try:
            apply_migration.psql(env, sql, single_transaction=True)
            return "applied"
        except apply_migration._AlreadyRecorded as recorded:
            assert recorded.same
            return "same"

    with ThreadPoolExecutor(max_workers=2) as pool:
        assert sorted(pool.map(apply, range(2))) == ["applied", "same"]
    assert scalar(env, "SELECT count(*) FROM public.schema_migrations "
                       f"WHERE file = '{name}' AND sha256 = '{sha}';") == "1"
    assert scalar(env, "SELECT to_regclass('public.concurrent_probe') IS NOT NULL;") == "t"
    print("apply-migration-postgres: passed concurrent-apply-exactly-once")


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

            run_history_cases(fixture, history_manifest(fixture.root), test_env)
            run_concurrent_apply(fixture.root, test_env)
    finally:
        run_psql(env, f"DROP DATABASE {TEST_DB};")


if __name__ == "__main__":
    main()
