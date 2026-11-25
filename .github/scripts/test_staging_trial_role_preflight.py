"""Check the protected preflight SQL against hosted disposable PostgreSQL."""

import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from staging_apply import StagingApplyError
import staging_trial_role_preflight as preflight
from test_apply_migration_postgres import run_psql
from test_ingest_session_migration_postgres import scratch_database


ADMIN = {
    "PATH": os.environ["PATH"], "PGHOSTADDR": "127.0.0.1", "PGPORT": "5432",
    "PGDATABASE": "nntune_ci", "PGUSER": "postgres", "PGPASSWORD": "ci-throwaway",
    "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5",
}
READER, WRITER, ISSUER = "preflight_fixture_reader", "preflight_fixture_writer", "preflight_fixture_issuer"
DIGEST = "3" * 64


class TrialRolePreflightTests(unittest.TestCase):
    def test_isolation(self):
        with scratch_database(ADMIN, preflight.DATABASE) as db:
            run_psql(db, f"""
                REVOKE CONNECT, TEMP ON DATABASE {preflight.DATABASE} FROM PUBLIC;
                GRANT CONNECT ON DATABASE {preflight.DATABASE} TO {READER}, {WRITER};
                CREATE TABLE public.schema_migrations (file text, sha256 text);
                INSERT INTO public.schema_migrations VALUES ('{preflight.BASELINE}', '{DIGEST}');
                GRANT SELECT ON public.schema_migrations TO {ISSUER};
                CREATE TABLE public.preflight_data (hint text);
                CREATE SEQUENCE public.preflight_seq;
            """)

            def role_oid(name):
                return int(run_psql(db, f"SELECT oid FROM pg_roles WHERE rolname = '{name}';"))

            scope = preflight.parse_scope(json.dumps({
                "database_oid": int(run_psql(db, "SELECT oid FROM pg_database WHERE datname = current_database();")),
                "issuer_oid": role_oid(ISSUER),
                "reader": [READER, role_oid(READER)], "writer": [WRITER, role_oid(WRITER)],
            }))
            self.assertTrue(all(preflight.observe(db, scope, DIGEST).values()))
            admin_grant = f"GRANT {READER} TO {ISSUER} WITH ADMIN TRUE, INHERIT FALSE, SET FALSE;"
            run_psql(db, admin_grant)
            self.assertTrue(all(preflight.observe(db, scope, DIGEST).values()))
            run_psql(db, f"ALTER ROLE {ISSUER} NOCREATEROLE;")
            self.assertFalse(preflight.observe(db, scope, DIGEST)["role membership"])
            run_psql(db, f"REVOKE {READER} FROM {ISSUER}; ALTER ROLE {ISSUER} CREATEROLE;")

            for connection, pin, digest in (
                (ADMIN, scope, DIGEST), (db, {**scope, "database_oid": 1}, DIGEST), (db, scope, "4" * 64),
            ):
                with self.subTest(connection=connection["PGDATABASE"], oid=pin["database_oid"], digest=digest):
                    with self.assertRaises(StagingApplyError):
                        preflight.observe(connection, pin, digest)
            self.assertFalse(preflight.observe(db, {**scope, "reader": [READER, 1]}, DIGEST)["issued identities"])

            run = subprocess.run
            for switch, expected in ((f"SET SESSION AUTHORIZATION {ISSUER};", f"{ISSUER}|{ISSUER}"),
                                     (f"SET ROLE {ISSUER};", f"postgres|{ISSUER}")):
                with self.subTest(switch=switch):
                    self.assertEqual(run_psql(db, switch +
                                             "SELECT session_user, current_user FROM public.schema_migrations;"),
                                     expected)

                    def switched_psql(*args, input, **kwargs):
                        return run(*args, input=switch + "\n" + input, **kwargs)

                    with patch.object(preflight.subprocess, "run", side_effect=switched_psql):
                        with self.assertRaises(StagingApplyError):
                            preflight.observe(db, scope, DIGEST)

            for grant, revoke, verdict in (
                (f"ALTER ROLE {READER} BYPASSRLS", f"ALTER ROLE {READER} NOBYPASSRLS", "login attributes"),
                (f"ALTER ROLE {WRITER} INHERIT", f"ALTER ROLE {WRITER} NOINHERIT", "login attributes"),
                (f"GRANT {WRITER} TO {READER} WITH INHERIT FALSE", f"REVOKE {WRITER} FROM {READER}", "role membership"),
                (f"GRANT {READER} TO {ISSUER} WITH ADMIN TRUE, INHERIT FALSE, SET TRUE", f"REVOKE {READER} FROM {ISSUER}", "role membership"),
                (f"GRANT {READER} TO {ISSUER} WITH ADMIN TRUE, INHERIT TRUE, SET FALSE", f"REVOKE {READER} FROM {ISSUER}", "role membership"),
                (f"GRANT CONNECT ON DATABASE postgres TO {WRITER}", f"REVOKE CONNECT ON DATABASE postgres FROM {WRITER}", "database isolation"),
                (f"GRANT CONNECT ON DATABASE {preflight.DATABASE} TO PUBLIC; REVOKE CONNECT ON DATABASE {preflight.DATABASE} FROM {READER}",
                 f"GRANT CONNECT ON DATABASE {preflight.DATABASE} TO {READER}; REVOKE CONNECT ON DATABASE {preflight.DATABASE} FROM PUBLIC", "staging access"),
                (f"GRANT TEMP ON DATABASE {preflight.DATABASE} TO PUBLIC", f"REVOKE TEMP ON DATABASE {preflight.DATABASE} FROM PUBLIC", "staging access"),
                (f"GRANT CREATE ON SCHEMA public TO {READER}", f"REVOKE CREATE ON SCHEMA public FROM {READER}", "schema creation"),
                (f"CREATE SCHEMA owned AUTHORIZATION {WRITER}", "DROP SCHEMA owned", "object ownership"),
                ("GRANT SELECT (hint) ON public.preflight_data TO PUBLIC", "REVOKE SELECT (hint) ON public.preflight_data FROM PUBLIC", "relation and sequence access"),
                (f"GRANT USAGE ON SEQUENCE public.preflight_seq TO {WRITER}", f"REVOKE USAGE ON SEQUENCE public.preflight_seq FROM {WRITER}", "relation and sequence access"),
                ("CREATE FUNCTION public.preflight_fn() RETURNS int LANGUAGE sql SECURITY DEFINER AS 'SELECT 1'", "DROP FUNCTION public.preflight_fn()", "privileged routines"),
            ):
                with self.subTest(verdict=verdict):
                    run_psql(db, grant)
                    try:
                        self.assertFalse(preflight.observe(db, scope, DIGEST)[verdict])
                    finally:
                        run_psql(db, revoke)

            with TemporaryDirectory() as tmp:
                summary = Path(tmp) / "summary.md"
                with patch.dict(os.environ, {
                    preflight.SCOPE_ENV: json.dumps(scope), preflight.SECRET_ENV: "synthetic-url",
                    "GITHUB_STEP_SUMMARY": str(summary),
                }, clear=True), patch.object(preflight, "evidence_child_env", return_value=db), \
                        patch.object(preflight, "load_entries", return_value=[
                            {"file": preflight.BASELINE, "sha256": DIGEST, "target": "app"},
                        ]):
                    self.assertEqual(preflight.main(), 0)
                report = summary.read_text()
                self.assertIn("database isolation — pass", report)
                for sensitive in (READER, WRITER, ISSUER, str(scope["database_oid"]), ADMIN["PGPASSWORD"]):
                    self.assertNotIn(sensitive, report)


class TrialConnectionReadbackTests(unittest.TestCase):
    def test_matching_identity_requires_origin_tls_for_every_binding(self):
        bindings = ("PRIVATE_TRIAL_UPLOAD_DB", "PRIVATE_TRIAL_AUTH_DB",
                    "PRIVATE_TRIAL_PUBLISH_DB", "PRIVATE_TRIAL_ENROLL_DB")
        # The synthetic database, login and effective role all match on each binding.
        identity_verdicts = dict.fromkeys(bindings, "matched")
        complete_tls = dict.fromkeys(bindings, True)

        self.assertEqual(preflight.binding_connection_verdict(identity_verdicts, complete_tls), "qualified")
        for binding in bindings:
            with self.subTest(binding=binding):
                incomplete_tls = complete_tls.copy()
                del incomplete_tls[binding]
                self.assertEqual(preflight.binding_connection_verdict(identity_verdicts, incomplete_tls), "pending")
                incomplete_tls[binding] = None
                self.assertEqual(preflight.binding_connection_verdict(identity_verdicts, incomplete_tls), "pending")


if __name__ == "__main__":
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("requires the disposable GitHub-hosted PostgreSQL service")
    run_psql(ADMIN, f"""BEGIN;
        CREATE ROLE {READER} LOGIN NOINHERIT;
        CREATE ROLE {WRITER} LOGIN NOINHERIT;
        CREATE ROLE {ISSUER} CREATEROLE NOINHERIT;
        REVOKE CONNECT ON DATABASE postgres, nntune_ci FROM PUBLIC;
        COMMIT;""")
    try:
        result = unittest.main(exit=False).result
    finally:
        run_psql(ADMIN, f"""BEGIN;
            GRANT CONNECT ON DATABASE postgres, nntune_ci TO PUBLIC;
            DROP ROLE {READER}, {WRITER}, {ISSUER};
            COMMIT;""")
    raise SystemExit(not result.wasSuccessful())
