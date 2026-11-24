"""Exercise the dormant probe on the disposable api-migrations service."""

import os
import subprocess
import unittest

from database_bootstrap_preflight import (
    BootstrapPreflightError,
    catalog_probe_sql,
    require_absent_catalog_result,
)


def run_sql(sql):
    # Supply the entire connection environment. This fixture uses the
    # job's throwaway loopback service even if the caller exports a URL.
    result = subprocess.run(
        ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"],
        input=sql,
        env={
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "PGHOSTADDR": "127.0.0.1",
            "PGPORT": "5432",
            "PGDATABASE": "postgres",
            "PGUSER": "postgres",
            "PGPASSWORD": "ci-throwaway",
            "PGSSLMODE": "disable",
            "PGCONNECT_TIMEOUT": "3",
        },
        capture_output=True, text=True, timeout=20,
    )
    return result.returncode, result.stdout


def probe(launch_database="unused_launch_fixture", maintenance_database="postgres",
          creator_role="postgres", session_setup="", session_cleanup=""):
    return run_sql(session_setup
                   + catalog_probe_sql(maintenance_database, launch_database, creator_role)
                   + session_cleanup)


class BootstrapCatalogTests(unittest.TestCase):
    def test_absent_name_on_expected_session(self):
        sql = catalog_probe_sql("postgres", "unused_launch_fixture", "postgres")
        self.assertTrue(sql.startswith("BEGIN READ ONLY;\n"))
        require_absent_catalog_result(*probe(
            creator_role="bootstrap_creator_fixture",
            session_setup=("CREATE ROLE bootstrap_creator_fixture LOGIN CREATEDB;\n"
                           "SET SESSION AUTHORIZATION bootstrap_creator_fixture;\n"),
            session_cleanup="RESET SESSION AUTHORIZATION; DROP ROLE bootstrap_creator_fixture;\n",
        ))

    def test_existing_name_requires_verification(self):
        with self.assertRaisesRegex(BootstrapPreflightError, "already exists"):
            require_absent_catalog_result(*probe(launch_database="nntune_ci"))

    def test_wrong_database_and_switched_roles_refuse(self):
        for pins in (
            {"maintenance_database": "nntune_ci"},
            {"creator_role": "pg_monitor", "session_setup": "SET ROLE pg_monitor;\n"},
            {"session_setup": "SET ROLE pg_monitor;\n"},
        ):
            with self.subTest(pins=pins), self.assertRaisesRegex(
                BootstrapPreflightError, "differs from the approved session"
            ):
                require_absent_catalog_result(*probe(**pins))

    def test_creator_without_login_or_createdb_refuses(self):
        for attributes in ("LOGIN NOCREATEDB", "NOLOGIN CREATEDB"):
            with self.subTest(attributes=attributes), self.assertRaisesRegex(
                BootstrapPreflightError, "lacks LOGIN or CREATEDB"
            ):
                require_absent_catalog_result(*probe(
                    creator_role="bootstrap_ineligible_fixture",
                    session_setup=(f"CREATE ROLE bootstrap_ineligible_fixture {attributes};\n"
                                   "SET SESSION AUTHORIZATION bootstrap_ineligible_fixture;\n"),
                    session_cleanup=("RESET SESSION AUTHORIZATION;"
                                     " DROP ROLE bootstrap_ineligible_fixture;\n"),
                ))

    def test_template0_with_unexpected_flags_refuses(self):
        for flag, invalid, original in (
            ("ALLOW_CONNECTIONS", "true", "false"),
            ("IS_TEMPLATE", "false", "true"),
        ):
            with self.subTest(flag=flag):
                try:
                    with self.assertRaisesRegex(BootstrapPreflightError, "template0 catalog entry"):
                        require_absent_catalog_result(*probe(
                            session_setup=f"ALTER DATABASE template0 {flag} {invalid};\n",
                        ))
                finally:
                    self.assertEqual(run_sql(f"ALTER DATABASE template0 {flag} {original};\n"), (0, ""))

    def test_unsafe_pins_and_ambiguous_output_refuse(self):
        for name in ("postgres", "template0", "template1", "a" * 64, "last\n", "name'with_quote"):
            with self.subTest(name=name), self.assertRaises(BootstrapPreflightError):
                catalog_probe_sql("postgres", name, "fixture_creator")
        with self.assertRaises(BootstrapPreflightError):
            catalog_probe_sql("control_fixture", "control_fixture", "fixture_creator")
        with self.assertRaises(BootstrapPreflightError):
            catalog_probe_sql("postgres", "launch_fixture", "bad'role")
        for out in ("", "name-absent", "name-absent\nalready-exists\n", "foreign diagnostic"):
            with self.subTest(out=out), self.assertRaisesRegex(
                BootstrapPreflightError, "^unexpected catalog probe output$"
            ):
                require_absent_catalog_result(0, out)
        with self.assertRaisesRegex(BootstrapPreflightError, "^catalog probe failed;"):
            require_absent_catalog_result(3, "name-absent\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
