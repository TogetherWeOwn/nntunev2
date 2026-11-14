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
                 patch.object(smoke, "psql", side_effect=[None, None, None, drop_error]), \
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

    def test_fixture_is_synthetic_disposable_and_inert(self):
        fx = smoke.fixture.build()
        sql = smoke.fixture.seed_sql(fx, mirror=True)
        self.assertEqual(sql.count("INSERT INTO users"), 6)
        self.assertEqual(sql.count("INSERT INTO subsonic_credentials"), 6)
        self.assertIn("deleting_at", sql)
        self.assertIn("mb_track", sql)
        self.assertIn(smoke.fixture.OTHER_REL, sql)
        self.assertNotEqual(smoke.fixture.REL, smoke.fixture.OTHER_REL)
        self.assertNotIn("mb_track", smoke.fixture.seed_sql(fx))
        for secret in fx.keys.values():
            self.assertNotIn(secret, sql)  # only the peppered HMAC is stored
        self.assertEqual(len(set(fx.keys.values())), len(fx.keys))
        self.assertTrue(all(v.startswith("ci-synthetic-") or v.startswith("not a") for v in fx.keys.values()))
        self.assertRegex(smoke.fixture.key_hmac(fx, "active"), r"^[0-9a-f]{64}$")
        self.assertNotEqual(smoke.fixture.key_hmac(fx, "active"), smoke.fixture.key_hmac(fx, "revoked"))
        self.assertIn("SUBSONIC_INDEX_PEPPER=", smoke.dev_vars(fx))
        self.assertNotIn("SUBSONIC_INDEX_PEPPER", smoke.dev_vars())
        self.assertEqual({sc.kind for sc in smoke.fixture.SCENARIOS},
                         {"verify", "denial", "terminal", "mutation", "stream", "failure"})
        for sc in smoke.fixture.SCENARIOS:
            self.assertTrue(smoke.fixture.request_path(fx, sc).startswith("/rest/"), sc.name)

    def test_snapshot_uses_current_play_ledger(self):
        sql = smoke.fixture.snapshot_sql()
        self.assertIn("FROM stream_play_ledger t", sql)
        self.assertNotIn("FROM usage_ledger t", sql)

    def test_fixture_inert_rejects_successful_authenticated_ping(self):
        fx = smoke.fixture.build()
        def query(sql):
            if sql.startswith("SELECT count(*)"):
                return "10"
            return "snapshot"
        with patch.object(smoke, "http", return_value=(200, "application/json",
                '{"subsonic-response":{"status":"ok"}}')):
            with self.assertRaises(smoke.Failure):
                smoke.fixture_inert("http://localhost", query, fx, True)

    def test_mirror_fixture_binds_mb_db_to_the_disposable_database(self):
        conn = smoke.conn_string(smoke.pg_settings(ENV), "nntune_smoke_ab")
        names = lambda m: [h["binding"] for h in tomllib.loads(smoke.wrangler_toml(conn, SHA, m))["hyperdrive"]]
        self.assertEqual(names(False), ["HYPERDRIVE"])
        self.assertEqual(names(True), ["HYPERDRIVE", "MB_DB"])

    def test_mirror_files_match_manifest_mirror_targets(self):
        with open(smoke.MANIFEST, "rb") as fh:
            mirror = [m["file"] for m in tomllib.load(fh)["migration"] if m["target"] == "mirror"]
        self.assertEqual(list(smoke.fixture.mirror_files()), mirror)

    def test_fixture_refuses_unsafe_flags_and_external_targets(self):
        with patch.object(smoke, "prerequisites") as pre, patch.object(smoke.sys, "stderr"):
            self.assertEqual(smoke.main(["--no-fixture", "--mirror-fixture"]), 1)
            pre.assert_not_called()
        with patch.dict(smoke.os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                                           "DATABASE_URL": "postgresql://x"}):
            with self.assertRaises(smoke.Failure):
                smoke.prerequisites(type("A", (), {"wrangler": None})())

    def test_migrations_exist(self):
        for name in smoke.app_migrations():
            self.assertTrue((smoke.ROOT / "migrations" / name).is_file(), name)


if __name__ == "__main__":
    unittest.main()
