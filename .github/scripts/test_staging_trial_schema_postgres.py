"""Run the protected overlay against the recorded app baseline in hosted CI."""

import contextlib
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import apply_migration as migrator
import staging_apply
import staging_trial_schema as trial
from test_apply_migration_postgres import run_psql
from test_ingest_session_migration_postgres import SEED, scratch_database


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "migrations/manifest.toml"
DATABASE = "nntunev2_staging"
WRITER, READER, OTHER = "trial-fixture-writer", "trial_fixture_reader", "trial_fixture_outsider"
NO_SCHEMA = "SELECT " + " AND ".join(
    f"to_regclass('{table}') IS NULL" for table in trial.TABLES.split(", ")
) + f" AND to_regprocedure('{trial.LOCATOR_CHECK}') IS NULL;"


class TrialSchemaTests(unittest.TestCase):
    def dispatch(self, db, summary, mode="single", data_root=ROOT, writer=WRITER, *, connects=True):
        env = {
            staging_apply.SECRET_ENV: f"postgresql://fixture:synthetic@127.0.0.1/{DATABASE}",
            "TRIAL_WRITER_ROLE": writer, "TRIAL_READER_ROLE": READER,
            "GITHUB_STEP_SUMMARY": str(summary),
        }
        errors = io.StringIO()
        # Replace only the TLS transport for the isolated service. The
        # protected entry point still selects and composes the real SQL.
        with patch.dict(os.environ, env, clear=True), contextlib.redirect_stderr(errors), \
                patch.object(migrator, "connection_env", return_value=db) as connect:
            code = staging_apply.main([
                "apply", "--file", "private-trial", "--mode", mode, "--expect-database", DATABASE,
                "--trusted-root", str(ROOT), "--data-root", str(data_root), "--applied-by", "fixture",
            ])
        if connects:
            connect.assert_called_once_with(env[staging_apply.SECRET_ENV] + "?sslmode=verify-full")
        else:
            connect.assert_not_called()
        return code, errors.getvalue()

    def test_dispatch_refuses_changed_pinned_source_before_connect(self):
        with tempfile.TemporaryDirectory() as tmp:
            data_root = Path(tmp)
            for name in (trial.RECIPE, *trial.SCHEMAS):
                candidate = data_root / name
                candidate.parent.mkdir(parents=True, exist_ok=True)
                candidate.write_bytes((ROOT / name).read_bytes())
            with (data_root / trial.SCHEMAS[-1]).open("ab") as candidate:
                candidate.write(b"\n-- changed source\n")
            code, error = self.dispatch({}, data_root / "summary.md", data_root=data_root, connects=False)
            self.assertEqual(code, 1)
            self.assertIn("differs from protected main", error)

    def assert_refused_roles(self, db, summary):
        for writer in (READER, "public", 'invalid"writer'):
            with self.subTest(writer=writer):
                code, error = self.dispatch(db, summary, writer=writer, connects=False)
                self.assertEqual(code, 1)
                self.assertIn("configure distinct private trial", error)
        for grant, revoke in (
            (f'GRANT "{WRITER}" TO "{READER}" WITH INHERIT FALSE;', f'REVOKE "{WRITER}" FROM "{READER}";'),
            (f'GRANT postgres TO "{WRITER}" WITH INHERIT FALSE;', f'REVOKE postgres FROM "{WRITER}";'),
            (f'GRANT postgres TO "{READER}" WITH INHERIT FALSE;', f'REVOKE postgres FROM "{READER}";'),
            (f'GRANT CREATE ON SCHEMA public TO "{WRITER}";', f'REVOKE CREATE ON SCHEMA public FROM "{WRITER}";'),
            ('REVOKE USAGE ON SCHEMA public FROM PUBLIC;', 'GRANT USAGE ON SCHEMA public TO PUBLIC;'),
        ):
            with self.subTest(grant=grant):
                run_psql(db, grant)
                code, error = self.dispatch(db, summary)
                self.assertEqual(code, 1)
                self.assertIn("psql failed", error)
                self.assertEqual(run_psql(db, NO_SCHEMA), "t")
                run_psql(db, revoke)

    def assert_access(self, db):
        for table in trial.TABLES.split(", "):
            with self.subTest(table=table):
                self.assertEqual(run_psql(db, f"""
                    SELECT has_table_privilege('{READER}', '{table}', 'SELECT')
                       AND has_table_privilege('{WRITER}', '{table}', 'SELECT')
                       AND has_table_privilege('{WRITER}', '{table}', 'INSERT')
                       AND NOT has_any_column_privilege('{READER}', '{table}', 'INSERT,UPDATE')
                       AND NOT has_any_column_privilege('{OTHER}', '{table}', 'SELECT,INSERT,UPDATE')
                       AND NOT has_table_privilege('{READER}', '{table}', 'DELETE,TRUNCATE,TRIGGER,MAINTAIN')
                       AND NOT has_table_privilege('{WRITER}', '{table}', 'DELETE,TRUNCATE,TRIGGER,MAINTAIN')
                       AND NOT has_table_privilege('{OTHER}', '{table}', 'DELETE,TRUNCATE,TRIGGER,MAINTAIN');
                """), "t")
        self.assertEqual(run_psql(db, f"""
            SELECT NOT has_function_privilege('{OTHER}', '{trial.LOCATOR_CHECK}', 'EXECUTE')
               AND NOT has_any_column_privilege('{WRITER}', 'public.storage_s3_locator_intents', 'UPDATE')
               AND has_column_privilege('{WRITER}', 'public.catalogue_s3_sources', 'accepted_seq', 'UPDATE')
               AND NOT has_column_privilege('{WRITER}', 'public.catalogue_s3_sources', 'track_id', 'UPDATE')
               AND has_column_privilege('{WRITER}', 'public.private_library_admissions', 'object_id', 'UPDATE')
               AND NOT has_column_privilege('{WRITER}', 'public.private_library_admissions', 'invite_id', 'UPDATE');
        """), "t")

    def test_protected_overlay_from_recorded_baseline(self):
        with scratch_database(ENV, DATABASE) as db, tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.md"
            migrator.psql(db, migrator.ddl_sql(DATABASE), single_transaction=True)
            entries = migrator.load_manifest(MANIFEST)
            for entry in entries:
                if entry["target"] != "app":
                    continue
                filename = entry["file"]
                if filename == trial.BASELINE:
                    code, error = self.dispatch(db, summary, mode="all-pending", connects=False)
                    self.assertEqual(code, 1)
                    self.assertIn("single protected staging apply", error)
                    code, error = self.dispatch(db, summary)
                    self.assertEqual(code, 1)
                    self.assertIn("earlier same-target migration", error)
                    self.assertEqual(run_psql(db, NO_SCHEMA), "t")
                sql, *_ = migrator.compose(MANIFEST.parent / filename, MANIFEST, "fixture",
                                           expect_database=DATABASE)
                migrator.psql(db, sql, single_transaction=True)
                if filename == trial.BASELINE:
                    break
            else:
                self.fail("the recorded private trial baseline was not applied")
            history = "SELECT jsonb_agg(to_jsonb(m) ORDER BY file) FROM public.schema_migrations m;"
            before = run_psql(db, history)
            self.assertEqual(run_psql(db, NO_SCHEMA), "t")
            self.assert_refused_roles(db, summary)

            run_psql(db, f"""
                ALTER DEFAULT PRIVILEGES GRANT ALL ON TABLES TO "{OTHER}", PUBLIC;
                ALTER DEFAULT PRIVILEGES GRANT EXECUTE ON FUNCTIONS TO "{OTHER}";
                CREATE FUNCTION public.fail_trial_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$
                BEGIN RAISE EXCEPTION 'fixture refused late trial DDL'; END $$;
                CREATE EVENT TRIGGER fail_trial_ddl ON ddl_command_start WHEN TAG IN ('CREATE INDEX')
                    EXECUTE FUNCTION public.fail_trial_ddl();
            """)
            code, error = self.dispatch(db, summary)
            self.assertEqual(code, 1)
            self.assertIn("psql failed", error)
            self.assertEqual(run_psql(db, NO_SCHEMA), "t")
            run_psql(db, "DROP EVENT TRIGGER fail_trial_ddl;")

            self.assertEqual(self.dispatch(db, summary), (0, ""))
            self.assertIn("Private trial schema installed", summary.read_text())
            self.assert_access(db)
            # Model the trigger's existing base-table read with a minimal
            # fixture grant, then execute the actual locator write as caller.
            self.assertEqual(run_psql(db, SEED + f"""
                INSERT INTO public.storage_object_envelope_intents
                SELECT object_id, root_id, decode('4e4e544d454430310002', 'hex') || uuid_send(root_id)
                       || uuid_send(environment_id) || uuid_send(library_id) || uuid_send(object_id)
                       || uuid_send(generation_id) || uuid_send(provider_configuration_version_id)
                       || int8send(plaintext_bytes) || int8send(envelope_bytes) || header_tag
                       || decode(repeat('00', 12) || repeat('cd', 48), 'hex')
                FROM public.storage_object_intents,
                     (VALUES ('f0000000-0000-4000-8000-000000000001'::UUID)) AS r(root_id);
                GRANT SELECT (object_id, generation_id) ON public.storage_object_intents TO "{WRITER}";
                SET ROLE "{WRITER}";
                INSERT INTO public.storage_s3_locator_intents
                    (object_id, root_version_id, s3_bucket, s3_key, s3_version_id)
                VALUES ('40000000-0000-4000-8000-000000000001',
                        'f0000000-0000-4000-8000-000000000001', 'trial-media',
                        'media/40000000000040008000000000000001/50000000000040008000000000000001',
                        'trial-fixture-version')
                RETURNING s3_version_id;
            """), "trial-fixture-version")
            run_psql(db, """
                DO $$
                BEGIN
                    UPDATE public.storage_s3_locator_intents SET s3_etag = s3_etag;
                    RAISE EXCEPTION 'locator update must fail even for the owner';
                EXCEPTION WHEN check_violation THEN NULL;
                END $$;
            """)
            version = "SELECT 'public.catalogue_s3_sources'::regclass::oid;"
            original_oid = run_psql(db, version)
            self.assertEqual(self.dispatch(db, summary), (0, ""))
            self.assertIn("Private trial schema already installed", summary.read_text())
            self.assertEqual(run_psql(db, version), original_oid)
            self.assertEqual(run_psql(db, history), before)
            receipt = "SELECT obj_description('public.private_library_admissions'::regclass, 'pg_class');"
            original_receipt = run_psql(db, receipt)
            code, error = self.dispatch(db, summary, writer=OTHER)
            self.assertEqual(code, 1)
            self.assertIn("psql failed", error)
            self.assertEqual(run_psql(db, receipt), original_receipt)
            self.assert_access(db)


if __name__ == "__main__":
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("requires the disposable GitHub-hosted PostgreSQL service")
    ENV = {
        "PATH": os.environ["PATH"], "PGHOSTADDR": "127.0.0.1", "PGPORT": "5432",
        "PGDATABASE": "nntune_ci", "PGUSER": "postgres", "PGPASSWORD": "ci-throwaway",
        "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5",
    }
    roles = tuple(f'"{role}"' for role in (WRITER, READER, OTHER))
    run_psql(ENV, "BEGIN;" + "".join(f"CREATE ROLE {role};" for role in roles) + "COMMIT;")
    try:
        result = unittest.TextTestRunner(verbosity=2).run(
            unittest.defaultTestLoader.loadTestsFromTestCase(TrialSchemaTests))
    finally:
        run_psql(ENV, "DROP ROLE " + ", ".join(roles) + ";")
    raise SystemExit(not result.wasSuccessful())
