#!/usr/bin/env python3
"""Secretless negative tests for the actual-staging schema deployment gate."""
import contextlib
import copy
import io
import pathlib
import subprocess
import unittest
from unittest.mock import patch

import staging_api_schema_gate as gate

A_SHA, B_SHA = "a" * 64, "b" * 64
# Renames are byte-identical, so the alias carries the entry's own sha.
EXPECTED = [
    ("0002_alpha.sql", A_SHA, ()),
    ("0003_beta.sql", B_SHA, (("0001_beta_old.sql", B_SHA),)),
]


def rows_json(*pairs):
    import json
    return json.dumps([list(pair) for pair in pairs]) + "\n"


ROWS = rows_json(("0002_alpha.sql", A_SHA), ("0003_beta.sql", B_SHA))


class SchemaGateTests(unittest.TestCase):
    def setUp(self):
        self.env = {
            "PATH": "/usr/bin", "PGSERVICE": "must-not-be-inherited",
            "CLOUDFLARE_API_TOKEN": "synthetic-provider-token",
            "CLOUDFLARE_ACCOUNT_ID": "a" * 32,
            "HYPERDRIVE_ID_STAGING": "b" * 32,
            "PLANETSCALE_MIGRATOR_URL_STAGING":
                "postgresql://migrator:synthetic-password@database.example.test:5432/nntunev2_staging?sslmode=verify-full",
        }
        self.payload = {"success": True, "result": {"origin": {
            "scheme": "postgres", "host": "database.example.test", "port": 5432,
            "database": "nntunev2_staging",
        }}}
        self.calls = []
        self.rows_output = ROWS

    def run_psql(self, args, **kwargs):
        self.calls.append((args, kwargs))
        output = "t\n" if kwargs["input"] == gate.GUARD_SQL else self.rows_output
        return subprocess.CompletedProcess(args, 0, output, "")

    def check(self, **kwargs):
        gate.check(self.env, fetch=lambda env: self.payload,
                   run=kwargs.get("run", self.run_psql), expected=EXPECTED)

    def assertCategory(self, category, **kwargs):
        with self.assertRaises(gate.CategorizedFailure) as caught:
            self.check(**kwargs)
        self.assertEqual(caught.exception.category, category)

    def test_success_is_catalog_only_bounded_and_secretless_argv(self):
        self.check()
        args, options = self.calls[0]
        self.assertNotIn("synthetic-password", " ".join(args))
        self.assertNotIn("PLANETSCALE_MIGRATOR_URL_STAGING", options["env"])
        self.assertNotIn("CLOUDFLARE_API_TOKEN", options["env"])
        self.assertNotIn("PGSERVICE", options["env"])
        self.assertEqual(options["env"]["PGDATABASE"], "nntunev2_staging")
        self.assertEqual(options["env"]["PGPASSWORD"], "synthetic-password")
        self.assertIn("default_transaction_read_only=on", options["env"]["PGOPTIONS"])
        self.assertIn("statement_timeout=5000", options["env"]["PGOPTIONS"])
        self.assertEqual(options["timeout"], 15)
        self.assertTrue(options["capture_output"])
        self.assertTrue(options["input"].startswith("BEGIN READ ONLY;"))
        self.assertEqual(options["input"], gate.GUARD_SQL)
        self.assertIn("schema_migrations", options["input"])
        # No other schema may shadow the checked public tracking table.
        self.assertIn("n.nspname NOT IN ('public','pg_catalog','information_schema')",
                      options["input"])
        # The second (last) query reads the tracking rows for comparison
        # against the manifest, again in a read-only transaction.
        rows_options = self.calls[-1][1]
        self.assertEqual(rows_options["input"], gate.ROWS_SQL)
        # The guard fails closed while a non-owner role holds a write
        # grant on the tracking table (its rows are deploy proof).
        self.assertIn("aclexplode(c.relacl)", options["input"])
        self.assertIn("a.grantee <> c.relowner", options["input"])
        for options in (options, rows_options):
            # Strip string literals first: the guard names privilege
            # types ('INSERT', ...) as data, which is not a mutation.
            import re
            statements = re.sub(r"'[^']*'", "''", options["input"])
            for mutation in ("ALTER", "INSERT", "UPDATE", "DELETE", "DROP"):
                self.assertNotIn(mutation, statements)

    def test_tls_verifies_hostname_with_the_system_ca_bundle(self):
        base = self.env["PLANETSCALE_MIGRATOR_URL_STAGING"].split("?", 1)[0]
        self.env["PGSSLROOTCERT"] = "/untrusted/ambient-ca.pem"
        for suffix in ("", "?sslmode=verify-full", "?sslmode=verify-full&sslrootcert=system",
                       "?sslrootcert=system"):
            with self.subTest(suffix=suffix):
                self.env["PLANETSCALE_MIGRATOR_URL_STAGING"] = base + suffix
                self.check()
                child = self.calls[-1][1]["env"]
                self.assertEqual(child["PGSSLMODE"], "verify-full")
                self.assertEqual(child["PGSSLROOTCERT"], "/etc/ssl/certs/ca-certificates.crt")

    def test_unverified_tls_modes_reject_before_password_is_used(self):
        base = self.env["PLANETSCALE_MIGRATOR_URL_STAGING"].split("?", 1)[0]
        for mode in ("require", "verify-ca", "prefer", "allow", "disable"):
            with self.subTest(mode=mode):
                self.env["PLANETSCALE_MIGRATOR_URL_STAGING"] = base + "?sslmode=" + mode
                self.assertCategory("config")
        self.assertEqual(self.calls, [])

    def test_only_the_system_root_cert_option_is_accepted(self):
        base = self.env["PLANETSCALE_MIGRATOR_URL_STAGING"].split("?", 1)[0] + "?sslmode=verify-full"
        for extra in ("&sslrootcert=/tmp/attacker-ca.pem",
                      "&sslrootcert=system&sslrootcert=system", "&sslrootcert=System",
                      "&sslcert=/tmp/client.pem", "&options=-csearch_path%3Devil"):
            with self.subTest(extra=extra):
                self.env["PLANETSCALE_MIGRATOR_URL_STAGING"] = base + extra
                self.assertCategory("config")
        self.assertEqual(self.calls, [])

    def test_missing_provider_credentials_fail_before_network(self):
        with self.assertRaises(gate.GateError):
            gate.fetch_config({})

    def test_target_mismatch_prevents_database_connection(self):
        for key, value in (("host", "other.example.test"), ("port", 6432),
                           ("database", "production"), ("scheme", "mysql")):
            with self.subTest(key=key):
                saved = copy.deepcopy(self.payload)
                self.payload["result"]["origin"][key] = value
                self.assertCategory("binding")
                self.payload = saved
        self.assertEqual(self.calls, [])

    def test_missing_database_secret_and_production_target_fail_closed(self):
        for value in ("", "postgresql://u:p@database.example.test/production",
                      "postgresql://u:p@database.example.test/nntunev2_staging?sslmode=disable"):
            self.env["PLANETSCALE_MIGRATOR_URL_STAGING"] = value
            self.assertCategory("config")
        self.assertEqual(self.calls, [])

    def test_provider_failure_prevents_database_connection(self):
        self.payload["success"] = False
        self.assertCategory("binding")
        self.assertEqual(self.calls, [])

    def test_fetch_exception_is_binding_and_skips_database(self):
        def boom(env):
            raise OSError("synthetic-private-provider-error")
        with self.assertRaises(gate.CategorizedFailure) as caught:
            gate.check(self.env, fetch=boom, run=self.run_psql)
        self.assertEqual(caught.exception.category, "binding")
        self.assertEqual(self.calls, [])

    def test_invalid_provider_inputs_are_config_before_network(self):
        fetched = []
        for key in ("CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID", "HYPERDRIVE_ID_STAGING"):
            with self.subTest(key=key):
                env = dict(self.env, **{key: ""})
                with self.assertRaises(gate.CategorizedFailure) as caught:
                    gate.check(env, fetch=fetched.append, run=self.run_psql)
                self.assertEqual(caught.exception.category, "config")
        self.assertEqual(fetched, [])
        self.assertEqual(self.calls, [])

    def test_empty_false_extra_and_error_results_never_pass(self):
        for code, output, category in ((0, "", "schema"), (0, "f\n", "schema"),
                                       (0, "t\nt\n", "schema"), (1, "t\n", "query"),
                                       (3, "t\n", "query"), (2, "t\n", "connect")):
            with self.subTest(code=code, output=output):
                self.assertCategory(
                    category,
                    run=lambda args, **kw: subprocess.CompletedProcess(args, code, output, "private"))

    def test_rows_behind_manifest_fail_as_schema(self):
        for rows in (rows_json(("0002_alpha.sql", A_SHA)),  # 0003 never applied
                     rows_json(("0002_alpha.sql", A_SHA), ("0003_beta.sql", B_SHA),
                               ("0004_extra.sql", "d" * 64)),  # unlisted row
                     rows_json(("0002_alpha.sql", "e" * 64), ("0003_beta.sql", B_SHA)),
                     "not-a-row\n"):
            with self.subTest(rows=rows):
                self.rows_output = rows
                self.assertCategory("schema")

    def test_renamed_from_row_satisfies_the_gate(self):
        self.rows_output = rows_json(("0001_beta_old.sql", B_SHA),
                                     ("0002_alpha.sql", A_SHA))
        self.check()

    def test_timeouts_and_missing_client_fail_as_connect(self):
        def timeout(args, **kwargs):
            raise subprocess.TimeoutExpired(args, 15)
        def missing(args, **kwargs):
            raise FileNotFoundError("psql")
        for run in (timeout, missing):
            self.assertCategory("connect", run=run)

    def test_main_never_renders_private_exception_details(self):
        output = io.StringIO()
        with patch.object(gate, "check", side_effect=RuntimeError("synthetic-private-password")):
            with contextlib.redirect_stderr(output):
                self.assertEqual(gate.main(), 1)
        self.assertNotIn("synthetic-private-password", output.getvalue())
        self.assertIn("staging-api-schema: FAIL category=unknown", output.getvalue())

    def test_main_prints_only_the_fixed_category(self):
        for category in gate.CATEGORIES:
            with self.subTest(category=category):
                output = io.StringIO()
                failure = gate.CategorizedFailure(category)
                failure.__cause__ = RuntimeError("synthetic-private-password")
                with patch.object(gate, "check", side_effect=failure):
                    with contextlib.redirect_stderr(output):
                        self.assertEqual(gate.main(), 1)
                hint = ("staging schema behind repo migrations — see docs/ops.md staging migrations"
                        if category == "schema"
                        else "configuration, target or schema not verified")
                self.assertEqual(
                    output.getvalue(),
                    f"staging-api-schema: FAIL category={category} ({hint})\n")
                self.assertNotIn("synthetic-private-password", output.getvalue())

    def test_schema_hint_points_to_an_existing_ops_section(self):
        ops = (pathlib.Path(__file__).parents[2] / "docs/ops.md").read_text()
        self.assertIn("\n### Staging migrations\n", ops)
        self.assertEqual(set(gate.HINTS), {"schema"})

    def test_staging_workflow_requires_executed_schema_success(self):
        workflow = (pathlib.Path(__file__).parents[1] / "workflows/deploy.yml").read_text()
        check = workflow.index("- name: Verify actual staging migration schema (read-only)")
        deploy = workflow.index("- name: Deploy api to staging")
        self.assertLess(check, deploy)
        self.assertIn("steps.schema.outcome == 'success'", workflow[deploy:deploy + 250])
        self.assertIn('SCHEMA_OUTCOME: ${{ steps.schema.outcome }}', workflow)
        self.assertIn('[ "${SCHEMA_OUTCOME:-}" = success ]', workflow)
        client = workflow.split('- name: Ensure staging schema-check client', 1)[1].split('- name:', 1)[0]
        self.assertIn('postgresql-client ca-certificates', client)
        self.assertIn('test -r /etc/ssl/certs/ca-certificates.crt', client)


class CompareTests(unittest.TestCase):
    def test_exact_rows_pass_in_any_order(self):
        self.assertTrue(gate.compare(EXPECTED, [("0003_beta.sql", B_SHA),
                                                 ("0002_alpha.sql", A_SHA)]))

    def test_double_apply_under_old_and_new_names_fails(self):
        rows = [("0002_alpha.sql", A_SHA), ("0003_beta.sql", B_SHA),
                ("0001_beta_old.sql", B_SHA)]
        self.assertFalse(gate.compare(EXPECTED, rows))

    def test_duplicate_file_rows_fail(self):
        rows = [("0002_alpha.sql", A_SHA), ("0002_alpha.sql", A_SHA),
                ("0003_beta.sql", B_SHA)]
        self.assertFalse(gate.compare(EXPECTED, rows))

    def test_overlapping_manifest_pairs_fail(self):
        # The same name+sha claimed by two entries (here as the second
        # entry's rename history) can never be satisfied unambiguously.
        overlapping = [("0002_alpha.sql", A_SHA, ()),
                       ("0003_beta.sql", B_SHA, (("0002_alpha.sql", A_SHA),))]
        self.assertFalse(gate.compare(
            overlapping, [("0002_alpha.sql", A_SHA), ("0003_beta.sql", B_SHA)]))


class LoadExpectedTests(unittest.TestCase):
    MANIFEST = f"""
[[migration]]
file = "0001_mirror.sql"
target = "mirror"
sha256 = "{'f' * 64}"

[[migration]]
file = "0003_beta.sql"
target = "app"
sha256 = "{B_SHA}"
renamed_from = [{{ file = "0001_beta_old.sql", sha256 = "{B_SHA}" }}]
"""

    def load(self, text):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "manifest.toml"
            path.write_text(text, encoding="utf-8")
            return gate.load_expected(path)

    def test_app_only_with_rename_history(self):
        self.assertEqual(self.load(self.MANIFEST),
                         [("0003_beta.sql", B_SHA, (("0001_beta_old.sql", B_SHA),))])

    def test_real_manifest_loads(self):
        expected = gate.load_expected()
        self.assertTrue(expected)
        self.assertTrue(all(name.endswith(".sql") for name, _, _ in expected))

    def test_malformed_entries_raise_gate_error(self):
        for text in ("", "[[migration]]\n",
                     self.MANIFEST.replace(B_SHA, "xyz"),
                     self.MANIFEST.replace('"app"', '"sideways"'),
                     # A rename alias whose sha differs from the entry's
                     # would certify bytes that were never applied.
                     self.MANIFEST.replace(
                         f'file = "0001_beta_old.sql", sha256 = "{B_SHA}"',
                         f'file = "0001_beta_old.sql", sha256 = "{"c" * 64}"')):
            with self.subTest(text=text[:30]):
                with self.assertRaises(gate.GateError):
                    self.load(text)


class ParseRowsTests(unittest.TestCase):
    def test_rejects_malformed_and_oversized_output(self):
        for output in ("not json\n",
                       f"0002_alpha.sql\t{A_SHA}\n",  # old delimiter framing
                       '{"0002_alpha.sql": "' + A_SHA + '"}\n',
                       '[["0002_alpha.sql"]]\n',
                       '[["0002_alpha.sql", "' + A_SHA + '", "extra"]]\n',
                       '[["../../etc/passwd", "' + A_SHA + '"]]\n',
                       # A tab/newline inside a stored value cannot
                       # frame-shift rows: it fails the filename shape.
                       '[["0002_alpha.sql\\t0003_beta.sql", "' + A_SHA + '"]]\n',
                       "x" * (gate.MAX_ROWS_OUTPUT + 1)):
            with self.subTest(output=output[:30]):
                with self.assertRaises(gate.CategorizedFailure) as caught:
                    gate.parse_rows(output)
                self.assertEqual(caught.exception.category, "schema")

    def test_empty_table_is_an_empty_json_array(self):
        self.assertEqual(gate.parse_rows("[]\n"), [])


if __name__ == "__main__":
    unittest.main()
