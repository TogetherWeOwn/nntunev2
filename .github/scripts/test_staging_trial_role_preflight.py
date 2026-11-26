"""Check protected trial role preflight and target admission."""

from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from staging_apply import StagingApplyError
import staging_trial_role_lifecycle as lifecycle
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
BINDING_ROLES = {
    "PRIVATE_TRIAL_UPLOAD_DB": "preflight_fixture_upload_writer",
    "PRIVATE_TRIAL_AUTH_DB": "preflight_fixture_authority_reader",
    "PRIVATE_TRIAL_PUBLISH_DB": "preflight_fixture_publication_writer",
    "PRIVATE_TRIAL_ENROLL_DB": "preflight_fixture_registrar",
}


def require_disposable_runner():
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("requires the disposable GitHub-hosted PostgreSQL service")


class TrialBindingIdentityTests(unittest.TestCase):
    def test_fixed_binding_verdicts(self):
        database_oid = 40200
        pins = {binding: (name, number) for number, (binding, name) in
                enumerate(BINDING_ROLES.items(), 40201)}
        rows = {binding: (preflight.DATABASE, database_oid, name, number, name, number)
                for binding, (name, number) in pins.items()}
        matched = dict.fromkeys(BINDING_ROLES, "matched")
        self.assertEqual(preflight.binding_identity_verdicts(database_oid, pins, rows), matched)

        for binding in BINDING_ROLES:
            with self.subTest(binding=binding, case="missing"):
                self.assertEqual(preflight.binding_identity_verdicts(database_oid, pins, {
                    key: row for key, row in rows.items() if key != binding
                }), {**matched, binding: "pending"})

        upload, auth, publish, enroll = BINDING_ROLES
        row = rows[publish]
        for case, observed, expected in (
            ("swapped roles", {upload: rows[auth], auth: rows[upload]},
             {upload: "pending", auth: "pending"}),
            ("effective role only", {publish: (*row[:2], *pins[auth], *row[4:])}, {publish: "pending"}),
            ("session role only", {publish: (*row[:4], *pins[auth])}, {publish: "pending"}),
            ("reissued role", {publish: (*row[:-1], row[-1] + 10)}, {publish: "pending"}),
            ("other database", {enroll: ("synthetic_other_db", *rows[enroll][1:])}, {enroll: "pending"}),
            ("other database oid", {publish: (row[0], database_oid + 1, *row[2:])}, {publish: "pending"}),
            ("malformed result", {enroll: ("postgresql://synthetic@example.invalid/fixture",)},
             {enroll: "pending"}),
            ("duplicate result", {publish: (row, row)}, {publish: "pending"}),
        ):
            with self.subTest(case=case):
                self.assertEqual(preflight.binding_identity_verdicts(database_oid, pins, {**rows, **observed}),
                                 {**matched, **expected})
        for altered in ({**pins, auth: pins[upload]},
                        {**pins, auth: (pins[auth][0], pins[upload][1])},
                        {key: pin for key, pin in pins.items() if key != auth}):
            self.assertEqual(preflight.binding_identity_verdicts(database_oid, altered, rows),
                             dict.fromkeys(BINDING_ROLES, "pending"))

    def test_disposable_binding_identity_query(self):
        require_disposable_runner()
        with scratch_database(ADMIN, preflight.DATABASE) as db:
            database_oid = int(run_psql(db, "SELECT oid FROM pg_database WHERE datname = current_database();"))
            pins = {binding: (role, int(run_psql(db, f"SELECT '{role}'::regrole::oid;")))
                    for binding, role in BINDING_ROLES.items()}

            def read_identity(role, setup=""):
                result = run_psql({**db, "PGUSER": role},
                                  "BEGIN READ ONLY;\n" + setup + preflight.BINDING_IDENTITY_SQL + "ROLLBACK;\n")
                name, db_oid, login, login_oid, effective, effective_oid = result.split("|")
                return name, int(db_oid), login, int(login_oid), effective, int(effective_oid)

            rows = {binding: read_identity(role) for binding, role in BINDING_ROLES.items()}
            matched = dict.fromkeys(BINDING_ROLES, "matched")
            self.assertEqual(preflight.binding_identity_verdicts(database_oid, pins, rows), matched)

            upload, auth, *_ = BINDING_ROLES
            upload_role, auth_role = BINDING_ROLES[upload], BINDING_ROLES[auth]
            run_psql(db, f"GRANT {auth_role} TO {upload_role} WITH INHERIT FALSE, SET TRUE;")
            switched = read_identity(upload_role, f"SET LOCAL ROLE {auth_role};\n")
            self.assertEqual(switched, (preflight.DATABASE, database_oid, *pins[upload], *pins[auth]))
            self.assertEqual(preflight.binding_identity_verdicts(database_oid, pins, {**rows, upload: switched}),
                             {**matched, upload: "pending"})


class TrialRolePreflightTests(unittest.TestCase):
    def test_isolated_grant_refuses_disagreeing_database_oids_before_psql(self):
        reader, writer = lifecycle.NAMES
        scope = {"database_oid": 46001, "issuer_oid": 46002,
                 "reader": [reader, 46003], "writer": [writer, 46004]}
        target = {
            "database_oid": scope["database_oid"], "executor_oid": 46005,
            "direct": "127.0.0.1", "direct_port": 15432,
            "routed": "127.0.0.1", "routed_port": 25432,
            "owner": "trial_fixture_owner", "executor": "trial_fixture_executor",
            "reader": reader, "writer": writer,
        }
        env = {preflight.SCOPE_ENV: json.dumps(scope)}
        for role, route in lifecycle.ROUTES.items():
            env[f"PRIVATE_TRIAL_{role}_URL"] = (
                f"postgresql://{target[role.lower()]}:synthetic@{target[route]}:"
                f"{target[f'{route}_port']}/{preflight.DATABASE}"
            )
        argv = ["staging_trial_role_lifecycle.py", "grant", "--isolated",
                "--fixture-only-oid", str(scope["database_oid"])]

        def dispatch(config):
            output, errors = StringIO(), StringIO()
            with patch.dict(os.environ, env | {lifecycle.CONFIG_ENV: json.dumps(config)}, clear=True), \
                    patch("sys.argv", argv), redirect_stdout(output), redirect_stderr(errors), \
                    patch.object(lifecycle.subprocess, "run", return_value=subprocess.CompletedProcess(
                        args=["psql"], returncode=0, stdout="", stderr="",
                    )) as psql:
                code = lifecycle.main()
            return code, output.getvalue().splitlines(), errors.getvalue().strip(), psql

        code, stages, error, psql = dispatch(target)
        self.assertEqual((code, error), (0, ""))
        self.assertEqual(stages, [
            "isolated executor authority passed", "isolated owner authority passed",
            "staging CONNECT passed", "application grants passed", "exact application ACL passed",
            "routed reader passed", "routed writer passed",
        ])
        self.assertEqual([call.kwargs["input"].splitlines()[0] for call in psql.call_args_list], [
            "BEGIN READ ONLY;", "BEGIN READ ONLY;", "BEGIN READ WRITE;", "BEGIN READ WRITE;",
            "BEGIN READ ONLY;", "BEGIN READ ONLY;", "BEGIN READ ONLY;",
        ])

        # Keep the scope, fixture pin, endpoints and all four logins fixed.
        code, stages, error, psql = dispatch(target | {"database_oid": 46006})
        self.assertEqual((code, stages, error), (1, [], "Trial role lifecycle refused"))
        psql.assert_not_called()

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
    def test_pinned_binding_readback_passes_and_detects_wrong_wiring(self):
        build, version = "a" * 40, "12345678-0000-4000-8000-000000000001"
        pins = {binding: (role, role_oid) for role_oid, (binding, role) in
                enumerate(BINDING_ROLES.items(), 40201)}
        scope = dict(build_sha=build, version_id=version, database_oid=40200, role_pins=pins)
        connected = pins.copy()
        calls = []

        def query(build_sha, version_id, binding, sql):
            calls.append((build_sha, version_id, binding, sql))
            role = connected[binding]
            row = (preflight.DATABASE, 40200, *role, *role)
            return preflight.BindingReadback(build, version, binding, (row,))

        passed = {binding: {"role": role, "database": preflight.DATABASE, "verdict": "pass"}
                  for binding, role in BINDING_ROLES.items()}
        self.assertEqual(preflight.read_binding_identities(**scope, query=query), passed)
        self.assertEqual(calls, [(build, version, binding, preflight.BINDING_IDENTITY_SQL)
                                 for binding in BINDING_ROLES])

        _, auth, publish, _ = BINDING_ROLES
        connected[auth] = connected[publish]
        self.assertEqual(preflight.read_binding_identities(**scope, query=query),
                         {**passed, auth: {**passed[auth], "verdict": "pending"}})

        pending = {binding: {**evidence, "verdict": "pending"} for binding, evidence in passed.items()}
        self.assertEqual(preflight.read_binding_identities(**scope), pending)
        calls.clear()
        for invalid in ({"version_id": "latest"}, {"build_sha": "main"}, {"database_oid": 0}):
            with self.subTest(invalid=invalid):
                self.assertEqual(preflight.read_binding_identities(**{**scope, **invalid}, query=query), pending)

        unpinned = {binding: {**evidence, "role": None} for binding, evidence in pending.items()}
        for case, invalid_pins in (
            ("missing role pin", {binding: pin for binding, pin in pins.items() if binding != auth}),
            ("duplicate role oid", {**pins, auth: (pins[auth][0], pins[publish][1])}),
            ("public role", {**pins, auth: ("public", pins[auth][1])}),
        ):
            with self.subTest(case=case):
                self.assertEqual(preflight.read_binding_identities(**{**scope, "role_pins": invalid_pins},
                                                                 query=query), unpinned)
        self.assertEqual(calls, [])

    def test_binding_readback_leaves_unverified_results_pending(self):
        build, version = "a" * 40, "12345678-0000-4000-8000-000000000001"
        pins = {binding: (role, role_oid) for role_oid, (binding, role) in
                enumerate(BINDING_ROLES.items(), 40201)}
        upload, auth, *_ = BINDING_ROLES
        receipts = {binding: preflight.BindingReadback(build, version, binding,
                    ((preflight.DATABASE, 40200, *pin, *pin),)) for binding, pin in pins.items()}
        receipt = receipts[auth]
        expected = {binding: {"role": role, "database": preflight.DATABASE,
                              "verdict": "pending" if binding == auth else "pass"}
                    for binding, role in BINDING_ROLES.items()}

        for case, response in (
            ("stale build", receipt._replace(build_sha="b" * 40)),
            ("stale version", receipt._replace(version_id="12345678-0000-4000-8000-000000000002")),
            ("wrong binding", receipt._replace(binding=upload)),
            ("duplicate rows", receipt._replace(rows=receipt.rows * 2)),
            ("missing rows", receipt._replace(rows=())),
            ("query error", OSError("synthetic connection details")),
        ):
            def query(build_sha, version_id, binding, sql):
                if binding != auth:
                    return receipts[binding]
                if isinstance(response, OSError):
                    raise response
                return response

            with self.subTest(case=case):
                self.assertEqual(preflight.read_binding_identities(
                    build_sha=build, version_id=version, database_oid=40200, role_pins=pins, query=query), expected)

    def test_timed_out_binding_preserves_sibling_evidence_and_keeps_preflight_pending(self):
        build, version = "a" * 40, "12345678-0000-4000-8000-000000000001"
        database_oid = 40200
        pins = {binding: (role, role_oid) for role_oid, (binding, role) in
                enumerate(BINDING_ROLES.items(), database_oid + 1)}
        auth = "PRIVATE_TRIAL_AUTH_DB"
        expected_calls = [(build, version, binding, preflight.BINDING_IDENTITY_SQL)
                          for binding in BINDING_ROLES]
        complete_tls = dict.fromkeys(BINDING_ROLES, True)

        for timed_out, overall in ((None, "qualified"), (auth, "pending")):
            with self.subTest(timed_out=timed_out):
                calls = []

                def query(build_sha, version_id, binding, sql):
                    calls.append((build_sha, version_id, binding, sql))
                    if binding == timed_out:
                        raise subprocess.TimeoutExpired(
                            ["synthetic-reader", "synthetic-connection-details"], 1,
                            output=b"synthetic-partial-row", stderr=b"synthetic-reader-diagnostics")
                    pin = pins[binding]
                    row = (preflight.DATABASE, database_oid, *pin, *pin)
                    return preflight.BindingReadback(build, version, binding, (row,))

                with redirect_stdout(StringIO()) as stdout, redirect_stderr(StringIO()) as stderr:
                    evidence = preflight.read_binding_identities(
                        build_sha=build, version_id=version, database_oid=database_oid,
                        role_pins=pins, query=query)

                self.assertEqual(calls, expected_calls)
                self.assertEqual(evidence, {
                    binding: {"role": role, "database": preflight.DATABASE,
                              "verdict": "pending" if binding == timed_out else "pass"}
                    for binding, role in BINDING_ROLES.items()
                })
                self.assertEqual((stdout.getvalue(), stderr.getvalue()), ("", ""))
                # The connection check expects the underlying identity verdict vocabulary.
                identities = {binding: "matched" if result["verdict"] == "pass" else "pending"
                              for binding, result in evidence.items()}
                self.assertEqual(preflight.binding_connection_verdict(identities, complete_tls), overall)

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
                pending_identity = {**identity_verdicts, binding: "pending"}
                self.assertEqual(preflight.binding_connection_verdict(pending_identity, complete_tls), "pending")


if __name__ == "__main__":
    require_disposable_runner()
    binding_logins = "\n".join(f"CREATE ROLE {role} LOGIN NOINHERIT PASSWORD 'ci-throwaway';"
                               for role in BINDING_ROLES.values())
    run_psql(ADMIN, f"""BEGIN;
        CREATE ROLE {READER} LOGIN NOINHERIT;
        CREATE ROLE {WRITER} LOGIN NOINHERIT;
        CREATE ROLE {ISSUER} CREATEROLE NOINHERIT;
        {binding_logins}
        REVOKE CONNECT ON DATABASE postgres, nntune_ci FROM PUBLIC;
        COMMIT;""")
    try:
        result = unittest.main(exit=False).result
    finally:
        run_psql(ADMIN, f"""BEGIN;
            GRANT CONNECT ON DATABASE postgres, nntune_ci TO PUBLIC;
            DROP ROLE {READER}, {WRITER}, {ISSUER}, {', '.join(BINDING_ROLES.values())};
            COMMIT;""")
    raise SystemExit(not result.wasSuccessful())
