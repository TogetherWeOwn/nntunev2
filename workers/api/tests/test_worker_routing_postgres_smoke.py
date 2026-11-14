#!/usr/bin/env python3
"""Pure-helper tests: python3 -I workers/api/tests/test_worker_routing_postgres_smoke.py"""
import importlib.util, sys, tomllib, unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "smoke", Path(__file__).resolve().parent / "worker_routing_postgres_smoke.py")
smoke = importlib.util.module_from_spec(spec)
sys.modules["smoke"] = smoke
spec.loader.exec_module(smoke)
SHA = "0123456789abcdef0123456789abcdef01234567"
ENV = {"NNTUNE_SMOKE_PG_PASSWORD": "p@ss/w"}


class Helpers(unittest.TestCase):
    def test_config_is_toml_with_quoted_local_hyperdrive(self):
        s = smoke.pg_settings(ENV)
        cfg = tomllib.loads(smoke.wrangler_toml(smoke.conn_string(s, "nntune_smoke_ab"), SHA))
        self.assertIn("p%40ss%2Fw", cfg["hyperdrive"][0]["localConnectionString"])
        self.assertNotIn("build", cfg)

    def test_fail_closed_settings(self):
        for env in ({}, {**ENV, "NNTUNE_SMOKE_PG_HOST": "example.com"},
                    {**ENV, "NNTUNE_SMOKE_PG_ADMIN_DB": "x;drop"}):
            with self.assertRaises(smoke.Failure):
                smoke.pg_settings(env)
        with self.assertRaises(smoke.Failure):
            smoke.wrangler_toml("postgresql://a:b@127.0.0.1:1/d", "abc")

    def test_classification(self):
        c = smoke.classify
        self.assertEqual(c(404, "text/plain; charset=UTF-8", "Not Found"), "subsonic-text-404")
        self.assertEqual(c(200, "application/json", '{"subsonic-response":{}}'), "subsonic-envelope")
        self.assertEqual(c(404, "application/json", '{"code":"not_found"}'), smoke.NOT_FOUND)
        self.assertEqual(c(500, "application/json", '{"error":"internal error"}'), "other")
        for status in (200, 500):
            self.assertEqual(c(status, "application/json", '{"code":"not_found"}'), "other")
        for body in ('{"error":"subsonic-response unavailable"}',
                     '{"subsonic-response":null}', '{"subsonic-response":[]}',
                     '[{"subsonic-response":{}}]', 'subsonic-response'):
            self.assertEqual(c(200, "application/json", body), "other")
        self.assertEqual(c(500, "application/json", '{"subsonic-response":{}}'), "other")

    def test_ping_requires_failure_on_the_successful_http_response(self):
        ok = '{"subsonic-response":{"status":"ok"}}'
        failed = '{"subsonic-response":{"status":"failed"}}'
        smoke.check_ping(200, "application/json", failed)
        for status, body in ((200, ok), (500, failed)):
            with self.assertRaises(smoke.Failure):
                smoke.check_ping(status, "application/json", body)

    def test_cleanup_failures_fail_main(self):
        settings = smoke.pg_settings(ENV)
        for drop_error, remove_error in ((None, PermissionError("denied")),
                                         (smoke.subprocess.TimeoutExpired("psql", 300), None)):
            with self.subTest(drop_error=drop_error, remove_error=remove_error), \
                 patch.object(smoke, "prerequisites", return_value=(settings, Path("wrangler"), SHA)), \
                 patch.object(smoke.tempfile, "mkdtemp", return_value="/tmp/fake"), \
                 patch.object(smoke, "psql", side_effect=[None, None, drop_error]), \
                 patch.object(smoke, "app_migrations", return_value=[]), \
                 patch.object(Path, "write_text"), patch.object(Path, "read_text", return_value=""), \
                 patch.object(Path, "mkdir"), \
                 patch.object(smoke.socket, "socket") as sock, patch("builtins.open"), \
                 patch.object(smoke.subprocess, "Popen") as popen, patch.object(smoke.process_group, "shutdown_process_group"), \
                 patch.object(smoke, "http", return_value=(200, "", "")), patch.object(smoke, "smoke") as run_smoke, \
                 patch.object(smoke.shutil, "rmtree", side_effect=remove_error) as remove, \
                 patch.object(smoke.sys, "stderr"):
                sock.return_value.__enter__.return_value.getsockname.return_value = ("localhost", 1234)
                popen.return_value.poll.return_value = None
                self.assertEqual(smoke.main([]), 1)
                run_smoke.assert_called_once()
                remove.assert_called_once_with(Path("/tmp/fake"))

    def test_migrations_exist(self):
        for name in smoke.app_migrations():
            self.assertTrue((smoke.ROOT / "migrations" / name).is_file(), name)


if __name__ == "__main__":
    unittest.main()
