"""Rehearse registration and application of the pending app migration."""

from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import shutil
from tempfile import TemporaryDirectory
import unittest

import apply_migration as migrator
import migrations_gate
from test_apply_migration_postgres import run_psql


ROOT = Path(__file__).resolve().parents[2]
API_SQL = ROOT / "workers/api/src"
INGEST_SQL = ROOT / "workers/ingest/src"
FIXTURE = ROOT / "workers/api/tests/fixtures/ingest_session_migration.sql"
ROLES = ("upload_fixture_runtime", "upload_fixture_reader", "upload_fixture_writer")
NO_TABLES = """
    SELECT to_regclass('public.ingest_session_reservations') IS NULL
       AND to_regclass('public.ingest_session_cleanup_intents') IS NULL;
"""
SEED = """
    INSERT INTO public.users (id, email, created_at)
    VALUES ('upload-owner', 'upload-owner@example.invalid', 11);
    INSERT INTO public.storage_environments VALUES ('10000000-0000-4000-8000-000000000001');
    INSERT INTO public.storage_library_namespaces VALUES (
        '20000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001',
        'upload-owner');
    INSERT INTO public.storage_object_intents VALUES (
        '40000000-0000-4000-8000-000000000001', '50000000-0000-4000-8000-000000000001',
        '10000000-0000-4000-8000-000000000001', '20000000-0000-4000-8000-000000000001',
        '60000000-0000-4000-8000-000000000001', 64, 128, decode(repeat('ab', 48), 'hex'));
"""
OLD_ROWS = """
    SELECT to_jsonb(u), to_jsonb(l)
    FROM public.users u JOIN public.storage_library_namespaces l ON l.owner_id = u.id
    WHERE u.id = 'upload-owner';
"""
DEFAULT_ACL = """
    ALTER DEFAULT PRIVILEGES IN SCHEMA public
        GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO upload_fixture_runtime;
    ALTER DEFAULT PRIVILEGES IN SCHEMA public
        GRANT SELECT ON TABLES TO upload_fixture_reader, PUBLIC;
    GRANT SELECT ON public.users, public.storage_library_namespaces
        TO upload_fixture_runtime, upload_fixture_reader;
"""
STATEMENTS = (
    ("reserve", "uuid, uuid, text, text, bytea, uuid", "uuid"),
    ("status", "uuid, uuid, text, uuid", "TABLE (state text, expires_at timestamptz)"),
    ("acquire_write_lease", "uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, uuid", "uuid"),
    ("advance_checkpoint", "uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, bigint, uuid", "bigint"),
    ("terminate", "uuid, uuid, text, uuid, text, bytea, uuid, bigint, bigint, uuid", "text"),
    ("claim_cleanup", "text, text, text, text",
     "TABLE (session_id text, kind text, delivery_claim_id text, delay_seconds int)"),
    ("ack_cleanup_enqueue", "text, text, text, text, text, text", "timestamptz"),
)


@contextmanager
def scratch_database(env, name):
    # Refuse if this database already exists. Only drop a database we created.
    run_psql(env, f"CREATE DATABASE {name} TEMPLATE template0;")
    try:
        yield {**env, "PGDATABASE": name}
    finally:
        run_psql(env, f"DROP DATABASE {name};")


class UploadMigrationTests(unittest.TestCase):
    def apply(self, env, filename, manifest):
        sql, *_ = migrator.compose(
            manifest.parent / filename, manifest, "upload-fixture",
            expect_database=env["PGDATABASE"],
        )
        migrator.psql(env, sql, single_transaction=True)

    def check_scoped_calls(self, env):
        for role in ROLES[:2]:
            self.assertTrue(run_psql(env, f"SET ROLE {role};\n" + OLD_ROWS))
        # Create temporary invoker wrappers from the unchanged worker SQL.
        wrappers = []
        for name, args, result in STATEMENTS:
            query = (API_SQL / f"ingest_session_{name}.sql").read_text()
            returns = result if result.startswith("TABLE") else f"SETOF {result}"
            wrappers.append(f"CREATE FUNCTION pg_temp.{name}({args}) RETURNS {returns} "
                            f"LANGUAGE sql AS $query${query}$query$;")
        reconcile = (INGEST_SQL / "cleanup_dispatch/postgres/reconcile.sql").read_text()
        wrappers.append(
            "CREATE FUNCTION pg_temp.reconcile(text, text, text, text, text, text, text, text) "
            "RETURNS TABLE (media_key text, upload_id text, deadline bigint) "
            f"LANGUAGE sql AS $query${reconcile}$query$;"
        )
        lock = (INGEST_SQL / "multipart/session_upload_lock.sql").read_text()
        wrappers.append(
            "CREATE FUNCTION pg_temp.lock_upload(text, text, text, text, bytea, text, bigint, bigint, "
            "text, text, text, text, bigint, bytea) "
            "RETURNS TABLE (claimed boolean, upload_id text, deadline bigint) "
            f"LANGUAGE sql AS $query${lock}$query$;"
        )
        run_psql(env, FIXTURE.read_text().replace("-- worker statements", "\n".join(wrappers)))

    def test_fresh_app_and_populated_upgrade(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            shutil.copytree(ROOT / "migrations", root / "migrations")
            manifest = root / "migrations/manifest.toml"
            entries = migrator.load_manifest(manifest)
            app_files = [entry["file"] for entry in entries if entry["target"] == "app"]
            number = int(entries[-1]["file"].split("_", 1)[0]) + 1
            filename = f"{number:04d}_ingest_sessions.sql"
            contents = (API_SQL / "ingest_session_schema.sql").read_bytes()
            checksum = hashlib.sha256(contents).hexdigest()
            (manifest.parent / filename).write_bytes(contents)
            with manifest.open("a", encoding="utf-8") as f:
                f.write(f'\n[[migration]]\nfile = "{filename}"\ntarget = "app"\n'
                        f'sha256 = "{checksum}"\n')
            self.assertEqual(migrations_gate.check_manifest(root), 0)
            receipt = f"FROM public.schema_migrations WHERE file = '{filename}'"

            for mode in ("fresh", "upgrade"):
                with self.subTest(mode=mode), scratch_database(ENV, f"nntune_upload_{mode}") as db:
                    migrator.psql(db, migrator.ddl_sql(db["PGDATABASE"]), single_transaction=True)
                    for prior in app_files[:-1]:
                        self.apply(db, prior, manifest)
                    with self.assertRaisesRegex(migrator.ApplyError, "earlier same-target migration"):
                        self.apply(db, filename, manifest)
                    self.assertEqual(run_psql(db, NO_TABLES), "t")
                    self.apply(db, app_files[-1], manifest)
                    run_psql(db, DEFAULT_ACL)
                    if mode == "upgrade":
                        run_psql(db, SEED)
                        before = run_psql(db, OLD_ROWS)
                        self.assertTrue(before)
                        # Fail the receipt INSERT after the candidate DDL ran.
                        run_psql(db, "ALTER TABLE public.schema_migrations ADD CONSTRAINT "
                                 f"fixture_refuse_receipt CHECK (file <> '{filename}');")
                        with self.assertRaisesRegex(migrator.ApplyError, "psql failed"):
                            self.apply(db, filename, manifest)
                        self.assertEqual(run_psql(db, NO_TABLES), "t")
                        self.assertEqual(run_psql(db, f"SELECT count(*) {receipt};"), "0")
                        self.assertEqual(run_psql(db, OLD_ROWS), before)
                        run_psql(db, "ALTER TABLE public.schema_migrations "
                                 "DROP CONSTRAINT fixture_refuse_receipt;")
                    self.apply(db, filename, manifest)
                    self.assertEqual(run_psql(db, f"SELECT sha256 {receipt};"), checksum)
                    if mode == "upgrade":
                        self.assertEqual(run_psql(db, OLD_ROWS), before)
                    else:
                        run_psql(db, SEED)
                    self.check_scoped_calls(db)
                    with self.assertRaises(migrator._AlreadyRecorded) as replay:
                        self.apply(db, filename, manifest)
                    self.assertTrue(replay.exception.same)
                    self.assertEqual(run_psql(db, f"SELECT sha256 {receipt};"), checksum)
                    print(f"upload session migration: {mode} passed", flush=True)


if __name__ == "__main__":
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("requires the disposable GitHub-hosted PostgreSQL service")
    # Fully replace libpq environment and pin the address before the first connection.
    ENV = {
        "PATH": os.environ["PATH"], "PGHOSTADDR": "127.0.0.1", "PGPORT": "5432",
        "PGDATABASE": "nntune_ci", "PGUSER": "postgres", "PGPASSWORD": "ci-throwaway",
        "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5",
    }
    run_psql(ENV, "BEGIN;" + "".join(f"CREATE ROLE {role};" for role in ROLES) + "COMMIT;")
    try:
        result = unittest.TextTestRunner(verbosity=2).run(
            unittest.defaultTestLoader.loadTestsFromTestCase(UploadMigrationTests))
    finally:
        run_psql(ENV, "DROP ROLE " + ", ".join(ROLES) + ";")
    raise SystemExit(not result.wasSuccessful())
