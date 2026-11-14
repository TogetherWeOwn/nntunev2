#!/usr/bin/env python3
"""Pure-helper tests: python3 -I workers/api/tests/test_worker_routing_postgres_smoke.py"""
import importlib.util, sys, tomllib, unittest
from pathlib import Path

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

    def test_migrations_exist(self):
        for name in smoke.app_migrations():
            self.assertTrue((smoke.ROOT / "migrations" / name).is_file(), name)


if __name__ == "__main__":
    unittest.main()
