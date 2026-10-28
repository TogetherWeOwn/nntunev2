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

    def run_psql(self, args, **kwargs):
        self.calls.append((args, kwargs))
        return subprocess.CompletedProcess(args, 0, "t\n", "")

    def check(self, **kwargs):
        gate.check(self.env, fetch=lambda env: self.payload, run=kwargs.get("run", self.run_psql))

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
        for column in ("refresh_retry_hash", "refresh_issued_at", "refresh_claimed_at"):
            self.assertIn(column, options["input"])
        for mutation in ("ALTER", "INSERT", "UPDATE", "DELETE", "DROP"):
            self.assertNotIn(mutation, options["input"])

    def test_tls_verifies_hostname_with_the_system_ca_bundle(self):
        base = self.env["PLANETSCALE_MIGRATOR_URL_STAGING"].split("?", 1)[0]
        self.env["PGSSLROOTCERT"] = "/untrusted/ambient-ca.pem"
        for suffix in ("", "?sslmode=verify-full"):
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
                with self.assertRaises(gate.GateError):
                    self.check()
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
                with self.assertRaises(gate.GateError):
                    self.check()
                self.payload = saved
        self.assertEqual(self.calls, [])

    def test_missing_database_secret_and_production_target_fail_closed(self):
        for value in ("", "postgresql://u:p@database.example.test/production",
                      "postgresql://u:p@database.example.test/nntunev2_staging?sslmode=disable"):
            self.env["PLANETSCALE_MIGRATOR_URL_STAGING"] = value
            with self.assertRaises(gate.GateError):
                self.check()
        self.assertEqual(self.calls, [])

    def test_provider_failure_prevents_database_connection(self):
        self.payload["success"] = False
        with self.assertRaises(gate.GateError):
            self.check()
        self.assertEqual(self.calls, [])

    def test_empty_false_extra_and_error_results_never_pass(self):
        for code, output in ((0, ""), (0, "f\n"), (0, "t\nt\n"), (1, "t\n")):
            with self.subTest(code=code, output=output):
                with self.assertRaises(gate.GateError):
                    self.check(run=lambda args, **kw: subprocess.CompletedProcess(args, code, output, "private"))

    def test_timeouts_propagate_as_failure(self):
        def timeout(args, **kwargs):
            raise subprocess.TimeoutExpired(args, 15)
        with self.assertRaises(subprocess.TimeoutExpired):
            self.check(run=timeout)

    def test_main_never_renders_private_exception_details(self):
        output = io.StringIO()
        with patch.object(gate, "check", side_effect=RuntimeError("synthetic-private-password")):
            with contextlib.redirect_stderr(output):
                self.assertEqual(gate.main(), 1)
        self.assertNotIn("synthetic-private-password", output.getvalue())
        self.assertIn("staging-api-schema: FAIL", output.getvalue())

    def test_staging_workflow_requires_executed_schema_success(self):
        workflow = (pathlib.Path(__file__).parents[1] / "workflows/deploy.yml").read_text()
        check = workflow.index("- name: Verify actual staging session schema (read-only)")
        deploy = workflow.index("- name: Deploy api to staging")
        self.assertLess(check, deploy)
        self.assertIn("steps.schema.outcome == 'success'", workflow[deploy:deploy + 250])
        self.assertIn('SCHEMA_OUTCOME: ${{ steps.schema.outcome }}', workflow)
        self.assertIn('[ "${SCHEMA_OUTCOME:-}" = success ]', workflow)
        client = workflow.split('- name: Ensure staging schema-check client', 1)[1].split('- name:', 1)[0]
        self.assertIn('postgresql-client ca-certificates', client)
        self.assertIn('test -r /etc/ssl/certs/ca-certificates.crt', client)


if __name__ == "__main__":
    unittest.main()
