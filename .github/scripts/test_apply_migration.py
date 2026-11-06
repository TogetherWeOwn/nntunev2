#!/usr/bin/env python3
"""Dry-run tests for the migrator apply script (no database)."""
import hashlib
import os
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import apply_migration


class ComposeTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.dir.name)

    def tearDown(self):
        self.dir.cleanup()

    def fixture(self, name="0002_alpha.sql", body="CREATE TABLE t (id int);\n",
                alias=None, sha_override=None):
        (self.root / name).write_text(body, encoding="utf-8")
        sha = sha_override or hashlib.sha256(body.encode()).hexdigest()
        alias_line = ""
        if alias:
            alias_name, alias_body = alias
            alias_sha = hashlib.sha256(alias_body.encode()).hexdigest()
            alias_line = f'renamed_from = [{{ file = "{alias_name}", sha256 = "{alias_sha}" }}]\n'
        (self.root / "manifest.toml").write_text(
            f'[[migration]]\nfile = "{name}"\ntarget = "app"\n'
            f'sha256 = "{sha}"\n{alias_line}', encoding="utf-8")

    def compose(self, name="0002_alpha.sql", **kwargs):
        kwargs.setdefault("expect_database", "nntune_apply_test")
        return apply_migration.compose(
            self.root / name, self.root / "manifest.toml", "operator (test)", **kwargs)

    def test_apply_composes_guard_body_and_tracking_row(self):
        self.fixture()
        sql, filename, sha, target = self.compose()
        self.assertEqual((filename, target), ("0002_alpha.sql", "app"))
        # The DDL runs separately; the composed transaction starts with
        # the advisory lock + already-recorded guard, then body + insert.
        self.assertIn("pg_advisory_xact_lock", sql)
        # The expected database is pinned inside the transaction.
        self.assertIn("current_database() <> 'nntune_apply_test'", sql)
        self.assertIn(apply_migration.WRONG_DATABASE, sql)
        self.assertIn(apply_migration.ALREADY_SAME, sql)
        self.assertIn(apply_migration.ALREADY_DIFFERENT, sql)
        self.assertIn("CREATE TABLE t (id int);", sql)
        self.assertIn(f"VALUES (\n    '0002_alpha.sql',\n    '{sha}',\n    now(),\n    'operator (test)'\n)", sql)
        # Plain insert: the in-transaction guard already refused any
        # recorded identity, so ON CONFLICT would only mask a race.
        self.assertNotIn("ON CONFLICT", sql)

    def test_sha_mismatch_refuses(self):
        self.fixture(sha_override="0" * 64)
        with self.assertRaises(apply_migration.ApplyError):
            self.compose()

    def test_unlisted_file_refuses(self):
        self.fixture()
        (self.root / "0007_stray.sql").write_text("SELECT 1;\n")
        with self.assertRaises(apply_migration.ApplyError):
            self.compose("0007_stray.sql")

    def test_renamed_from_alias_applies_under_original_name(self):
        # Renames are byte-identical: the alias file has the same bytes
        # (hence the same sha) as the current name.
        body = "CREATE TABLE t (id int);\n"
        self.fixture(alias=("0001_alpha_old.sql", body))
        (self.root / "0001_alpha_old.sql").write_text(body, encoding="utf-8")
        sql, filename, _, _ = self.compose("0001_alpha_old.sql")
        self.assertEqual(filename, "0001_alpha_old.sql")
        self.assertIn("VALUES (\n    '0001_alpha_old.sql',", sql)
        # The guard checks the whole identity, new name included.
        self.assertIn("'0002_alpha.sql'", sql)

    def test_own_transaction_control_requires_record_only(self):
        self.fixture(body="BEGIN;\nCREATE TABLE t (id int);\nCOMMIT;\n")
        with self.assertRaises(apply_migration.ApplyError):
            self.compose()
        sql, *_ = self.compose(record_only=True)
        self.assertNotIn("CREATE TABLE t", sql)
        self.assertIn("INSERT INTO public.schema_migrations", sql)

    def test_mid_line_transaction_control_is_detected(self):
        self.fixture(body="CREATE TABLE t (id int); COMMIT;\n")
        with self.assertRaises(apply_migration.ApplyError):
            self.compose()

    def test_psql_meta_command_is_detected(self):
        self.fixture(body="CREATE TABLE t (id int);\n\\i other.sql\n")
        with self.assertRaises(apply_migration.ApplyError):
            self.compose()

    def test_plpgsql_begin_inside_dollar_quotes_is_not_txn_control(self):
        body = ("-- this migration must not COMMIT; early\n"
                "DO $$\nBEGIN\n    PERFORM 1;\nEND\n$$;\n")
        self.fixture(body=body)
        sql, *_ = self.compose()
        self.assertIn("PERFORM 1;", sql)

    def test_applied_by_is_quoted(self):
        self.assertEqual(apply_migration.quote_literal("o'brien"), "'o''brien'")
        with self.assertRaises(apply_migration.ApplyError):
            apply_migration.quote_literal("bad\nvalue")

    def test_connection_env_requires_verified_tls_off_localhost(self):
        with self.assertRaises(apply_migration.ApplyError):
            apply_migration.connection_env("postgresql://u:p@db.example.test/app")
        child = apply_migration.connection_env(
            "postgresql://u:p@db.example.test/app?sslmode=verify-full")
        self.assertEqual(child["PGPASSWORD"], "p")
        self.assertNotIn("DATABASE_URL", child)
        apply_migration.connection_env("postgresql://u:p@localhost/app")

    def test_expect_database_is_required_and_plain(self):
        self.fixture()
        with self.assertRaises(TypeError):
            apply_migration.compose(
                self.root / "0002_alpha.sql", self.root / "manifest.toml", "op")
        for bad in ("", "db; DROP TABLE t", "db-name", "db name"):
            with self.assertRaises(apply_migration.ApplyError):
                self.compose(expect_database=bad)

    def test_record_only_also_pins_database(self):
        self.fixture(body="BEGIN;\nCREATE TABLE t (id int);\nCOMMIT;\n")
        sql, *_ = self.compose(record_only=True)
        self.assertIn("current_database() <> 'nntune_apply_test'", sql)

    def test_guard_requires_earlier_same_target_entries(self):
        bodies = {
            "0001_mirror_first.sql": ("mirror", "CREATE TABLE m1 (id int);\n"),
            "0002_alpha.sql": ("app", "CREATE TABLE a (id int);\n"),
            "0003_beta.sql": ("app", "CREATE TABLE b (id int);\n"),
        }
        manifest = ""
        for name, (target, body) in bodies.items():
            (self.root / name).write_text(body, encoding="utf-8")
            sha = hashlib.sha256(body.encode()).hexdigest()
            manifest += (f'[[migration]]\nfile = "{name}"\ntarget = "{target}"\n'
                         f'sha256 = "{sha}"\n')
        (self.root / "manifest.toml").write_text(manifest, encoding="utf-8")
        sql, *_ = self.compose("0003_beta.sql")
        # 0002 (same target) must be recorded first; the earlier mirror
        # entry is a different target and is not this guard's business.
        self.assertIn(apply_migration.MISSING_PRIOR, sql)
        self.assertIn("'0002_alpha.sql'", sql)
        self.assertNotIn("'0001_mirror_first.sql'", sql)
        # The first app entry has no same-target priors to require.
        sql, *_ = self.compose("0002_alpha.sql")
        self.assertNotIn(apply_migration.MISSING_PRIOR, sql)

    def write_manifest(self, entries):
        """entries: (name, target, body, [alias names], [(old, new) edges]).

        Aliases share the entry's bytes; edges are nested under their entry.
        """
        manifest = ""
        for name, target, body, aliases, *rest in entries:
            (self.root / name).write_text(body, encoding="utf-8")
            sha = hashlib.sha256(body.encode()).hexdigest()
            alias_toml = ", ".join(
                f'{{ file = "{a}", sha256 = "{sha}" }}' for a in aliases)
            manifest += (f'[[migration]]\nfile = "{name}"\ntarget = "{target}"\n'
                         f'sha256 = "{sha}"\n')
            if aliases:
                manifest += f"renamed_from = [{alias_toml}]\n"
            for old, new in (rest[0] if rest else []):
                manifest += ("[[migration.source_rename_history]]\n"
                             f'parent = "{"a" * 40}"\ncommit = "{"b" * 40}"\n'
                             f'old_path = "migrations/{old}"\n'
                             f'new_path = "migrations/{new}"\n'
                             f'old_sha256 = "{"c" * 64}"\nnew_sha256 = "{"d" * 64}"\n')
        (self.root / "manifest.toml").write_text(manifest, encoding="utf-8")

    def test_manifest_validation_is_strict_for_every_entry(self):
        body = "CREATE TABLE t (id int);\n"
        good = ("0001_mirror.sql", "mirror", "SELECT 1;\n", [])
        for label, bad in {
            "bad target": ("0002_alpha.sql", "web", body, []),
            "bad name": ("alpha.sql", "app", body, []),
            "duplicate name": ("0001_mirror.sql", "app", body, []),
            "alias equals current": ("0002_alpha.sql", "app", body, ["0002_alpha.sql"]),
            "alias collides with other entry": ("0002_alpha.sql", "app", body,
                                                ["0001_mirror.sql"]),
        }.items():
            with self.subTest(label):
                self.write_manifest([good, bad])
                (self.root / "0002_alpha.sql").write_text(body, encoding="utf-8")
                with self.assertRaises(apply_migration.ApplyError):
                    self.compose("0002_alpha.sql")

    def test_manifest_alias_must_be_byte_equal_and_hex(self):
        body = "CREATE TABLE t (id int);\n"
        self.write_manifest([("0002_alpha.sql", "app", body, ["0001_old.sql"])])
        text = (self.root / "manifest.toml").read_text()
        sha = hashlib.sha256(body.encode()).hexdigest()
        for label, bad_sha in (("different bytes", "0" * 64),
                               ("uppercase", sha.upper()), ("short", sha[:10])):
            with self.subTest(label):
                (self.root / "manifest.toml").write_text(
                    text.replace(f'file = "0001_old.sql", sha256 = "{sha}"',
                                 f'file = "0001_old.sql", sha256 = "{bad_sha}"'))
                with self.assertRaises(apply_migration.ApplyError):
                    self.compose("0002_alpha.sql")
        # A mirror entry is validated the same way as an app entry.
        self.write_manifest([("0001_m.sql", "mirror", body, ["0001_m.sql"]),
                             ("0002_alpha.sql", "app", body, [])])
        with self.assertRaises(apply_migration.ApplyError):
            self.compose("0002_alpha.sql")

    def test_resolve_entry_keeps_return_shape(self):
        sha = "e" * 64
        entries = [
            {"file": "0001_a.sql", "target": "app", "sha256": sha},
            {"file": "0002_b.sql", "target": "app", "sha256": sha,
             "renamed_from": [{"file": "0002_old.sql", "sha256": sha}]},
        ]
        target, identity, priors = apply_migration.resolve_entry(
            entries, "0002_old.sql", sha)
        self.assertEqual(target, "app")
        self.assertEqual(identity, [("0002_b.sql", sha), ("0002_old.sql", sha)])
        self.assertEqual(priors, [[("0001_a.sql", sha)]])

    def test_invalid_history_refuses(self):
        self.write_manifest([("0002_alpha.sql", "app", "SELECT 1;\n", [],
                              [("0001_x.sql", "../etc.sql")])])
        with self.assertRaises(apply_migration.ApplyError):
            self.compose("0002_alpha.sql")

    def deny_list(self, sql):
        deny = sql[sql.index("WHERE file IN ("):]
        return deny[:deny.index(")")]

    def test_history_names_feed_the_deny_check_only(self):
        body = "CREATE TABLE t (id int);\n"
        self.write_manifest([
            ("0001_first.sql", "app", "SELECT 1;\n", ["0001_first_alias.sql"],
             [("0001_ancient.sql", "0001_first.sql")]),
            ("0002_alpha.sql", "app", body, [],
             [("0002_prehistoric.sql", "0002_alpha.sql")])])
        sql, *_ = self.compose("0002_alpha.sql")
        self.assertIn(apply_migration.CONFLICTING_HISTORY, sql)
        deny = self.deny_list(sql)
        # History of the entry and of its earlier prior, plus the
        # prior's own current and alias names, are denied.
        for name in ("0002_prehistoric.sql", "0001_ancient.sql",
                     "0001_first.sql", "0001_first_alias.sql"):
            self.assertIn(f"'{name}'", deny)
        # History is never positive identity.
        for name in ("0002_prehistoric.sql", "0001_ancient.sql"):
            self.assertNotRegex(sql, rf"\('{name}', '[0-9a-f]{{64}}'\)")
        # Denial precedes ALREADY_SAME and the prior checks, and each
        # prior is also checked for duplicate accepted rows first.
        same = sql.index(f"RAISE EXCEPTION '{apply_migration.ALREADY_SAME}'")
        self.assertLess(sql.index(apply_migration.CONFLICTING_HISTORY), same)
        self.assertEqual(sql[:same].count(") > 1 THEN"), 2)
        self.assertGreater(sql.index(f"RAISE EXCEPTION '{apply_migration.MISSING_PRIOR}'"), same)

    def test_multi_hop_history_on_final_entry_and_mirror_excluded(self):
        # A chain a -> b -> c with no intermediate aliases is denied in
        # full because every edge is attached to the final entry.
        self.write_manifest([
            ("0001_mirror.sql", "mirror", "SELECT 1;\n", [],
             [("0001_mirror_old.sql", "0001_mirror.sql")]),
            ("0002_final.sql", "app", "SELECT 2;\n", [],
             [("0002_hop_a.sql", "0002_hop_b.sql"),
              ("0002_hop_b.sql", "0002_hop_c.sql"),
              ("0002_hop_c.sql", "0002_final.sql")])])
        sql, *_ = self.compose("0002_final.sql")
        deny = self.deny_list(sql)
        for name in ("0002_hop_a.sql", "0002_hop_b.sql", "0002_hop_c.sql",
                     "0002_final.sql"):
            self.assertIn(f"'{name}'", deny)
        self.assertNotIn("0001_mirror", sql)

    def test_guard_rejects_more_than_one_accepted_row(self):
        sql = apply_migration.guard_sql(
            [("0002_a.sql", "1" * 64), ("0001_a.sql", "1" * 64)], "db")
        self.assertIn(") > 1 THEN", sql)
        self.assertLess(sql.index(") > 1 THEN"),
                        sql.index(f"RAISE EXCEPTION '{apply_migration.ALREADY_SAME}'"))

    def test_psql_maps_conflicting_history(self):
        class Result:
            returncode, stdout = 3, ""
            stderr = f"ERROR: {apply_migration.CONFLICTING_HISTORY}"
        real = apply_migration.subprocess.run
        apply_migration.subprocess.run = lambda *a, **k: Result()
        try:
            with self.assertRaises(apply_migration.ApplyError):
                apply_migration.psql({}, "SELECT 1;")
        finally:
            apply_migration.subprocess.run = real


    def test_ddl_pins_write_privileges_and_database(self):
        self.assertIn("REVOKE INSERT, UPDATE, DELETE", apply_migration.DDL)
        self.assertIn("aclexplode(c.relacl)", apply_migration.DDL)
        # Column-level grants (attacl) bypass relacl entirely and must
        # be revoked per column too.
        self.assertIn("aclexplode(t.attacl)", apply_migration.DDL)
        self.assertIn("REVOKE INSERT (%I), UPDATE (%I), REFERENCES (%I)",
                      apply_migration.DDL)
        self.assertIn("a.grantee <> c.relowner", apply_migration.DDL)
        ddl = apply_migration.ddl_sql("nntune_apply_test")
        # The database check precedes CREATE TABLE so a wrong-database
        # run leaves no tracking table behind.
        self.assertLess(ddl.index(apply_migration.WRONG_DATABASE),
                        ddl.index("CREATE TABLE"))

    def test_main_refuses_url_database_mismatch_before_connecting(self):
        self.fixture()
        old = os.environ.get("DATABASE_URL")
        os.environ["DATABASE_URL"] = "postgresql://u:p@localhost/other_db"
        try:
            code = apply_migration.main([
                "--file", str(self.root / "0002_alpha.sql"),
                "--manifest", str(self.root / "manifest.toml"),
                "--applied-by", "operator (test)",
                "--expect-database", "nntune_apply_test",
            ])
        finally:
            if old is None:
                del os.environ["DATABASE_URL"]
            else:
                os.environ["DATABASE_URL"] = old
        self.assertEqual(code, 1)

    def test_connection_env_sslrootcert_must_be_system(self):
        base = "postgresql://u:p@db.example.test/app?sslmode=verify-full"
        child = apply_migration.connection_env(base + "&sslrootcert=system")
        self.assertEqual(child["PGSSLROOTCERT"],
                         "/etc/ssl/certs/ca-certificates.crt")
        for bad in ("&sslrootcert=/tmp/attacker-ca.pem",
                    "&sslrootcert=system&sslrootcert=system",
                    "&sslcert=/tmp/client.pem"):
            with self.assertRaises(apply_migration.ApplyError):
                apply_migration.connection_env(base + bad)


if __name__ == "__main__":
    unittest.main()
