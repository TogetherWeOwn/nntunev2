"""psql integration tests for schema.sql and a generated refresh.sql (stdlib only).

DESTRUCTIVE: runs only when GENRE_TEST_DATABASE_URL is explicitly set, and that
must name a disposable CI-only Postgres database (its name must contain a "test" or "ci"
word). Each test drops and recreates the public genre tables and a synthetic
mb_artist table carrying a marker comment; a pre-existing mb_artist without that
marker aborts the run. Never point this at a real database.
"""

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_materialize as tm  # noqa: E402

SCHEMA = HERE.parent / "schema.sql"
URL = os.environ.get("GENRE_TEST_DATABASE_URL")
MARKER = "nntune-genre-mirror-synthetic-test-table"

DROP = """
DROP VIEW IF EXISTS public.artist_genre_root;
DROP TABLE IF EXISTS public.genre_snapshot, public.genre_override, public.genre_rollup,
    public.artist_genre, public.genre_root, public.genre CASCADE;
DROP TABLE IF EXISTS public.mb_artist CASCADE;
"""
SETUP = DROP + """
CREATE TABLE public.mb_artist (mbid uuid PRIMARY KEY);
COMMENT ON TABLE public.mb_artist IS '%s';
INSERT INTO public.mb_artist(mbid) VALUES ('%s'), ('%s');
""" % (MARKER, tm.A, tm.B)
DUMP = """
SELECT x FROM (
  SELECT 'genre|' || t::text AS x FROM public.genre t
  UNION ALL SELECT 'root|' || t::text FROM public.genre_root t
  UNION ALL SELECT 'artist_genre|' || t::text FROM public.artist_genre t
  UNION ALL SELECT 'rollup|' || t::text FROM public.genre_rollup t
  UNION ALL SELECT 'override|' || t::text FROM public.genre_override t
  UNION ALL SELECT 'snapshot|' || t::text FROM public.genre_snapshot t
) s ORDER BY x
"""
VIEW = "SELECT artist_mbid::text || '|' || root_id FROM public.artist_genre_root ORDER BY 1"


@unittest.skipUnless(URL, "GENRE_TEST_DATABASE_URL not set (disposable CI Postgres only)")
class SchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory(prefix="genre-schema-test-")
        root = Path(cls._tmp.name)
        cls.home = root / "home"
        cls.home.mkdir()
        extract = root / "extract"
        done = tm.run_py(tm.EXTRACT, "--input", tm.FIXTURE, "--output", extract, "--dump-version", "synthetic-v1")
        assert done.returncode == 0, "fixture extraction failed"
        tm.write_json(root / "config.json", tm.config())
        tm.write_json(root / "cohort.json", tm.cohort())
        out = root / "out"
        done = tm.run_py(tm.MATERIALIZE, "--extract", extract, "--config", root / "config.json",
                         "--cohort", root / "cohort.json", "--output", out)
        assert done.returncode == 0, "fixture materialization failed"
        cls.refresh = out / "refresh.sql"
        cls.root = root
        name = cls.psql("-c", "SELECT current_database()").strip()
        if not re.search(r"(^|[_-])(test|ci)([_-]|$)", name, re.I):
            raise AssertionError("refusing destructive schema test: database is not named as disposable")
        marker = cls.psql("-c", "SELECT coalesce(obj_description(to_regclass('public.mb_artist'), 'pg_class'), '')"
                          " WHERE to_regclass('public.mb_artist') IS NOT NULL").strip()
        exists = cls.psql("-c", "SELECT to_regclass('public.mb_artist') IS NOT NULL").strip() == "t"
        if exists and marker != MARKER:
            raise AssertionError("refusing destructive schema test: mb_artist is not the synthetic test table")

    @classmethod
    def tearDownClass(cls):
        try:
            cls.psql("-c", DROP)
        finally:
            cls._tmp.cleanup()

    @classmethod
    def run_psql(cls, *args):
        # No shell, fixed environment, fail on first SQL error.
        env = {"PATH": os.environ.get("PATH", ""), "HOME": str(cls.home), "PGCONNECT_TIMEOUT": "10",
               "PGOPTIONS": "-c statement_timeout=30000 -c lock_timeout=10000", "PGCLIENTENCODING": "UTF8"}
        return subprocess.run(["psql", "-X", "-q", "-t", "-A", "-v", "ON_ERROR_STOP=1", "-d", URL, *args],
                              stdin=subprocess.DEVNULL, capture_output=True, timeout=120, env=env)

    @classmethod
    def psql(cls, *args):
        done = cls.run_psql(*args)
        if done.returncode != 0:
            raise AssertionError("psql failed with exit %d" % done.returncode)
        return done.stdout.decode("utf-8")

    def setUp(self):
        self.psql("-c", SETUP)

    def apply_schema(self):
        self.psql("-f", SCHEMA)

    def load(self, path=None):
        self.psql("-f", path or self.refresh)

    def dump(self):
        return self.psql("-c", DUMP)

    def view(self):
        return self.psql("-c", VIEW).splitlines()

    def test_schema_and_refresh_are_idempotent_and_mirror_only(self):
        self.apply_schema()
        self.apply_schema()
        self.load()
        first = self.dump()
        self.load()
        self.assertEqual(self.dump(), first)
        self.apply_schema()
        self.assertEqual(self.dump(), first)
        rows = [r for r in first.splitlines() if r.startswith("artist_genre|")]
        # C is not in the synthetic mb_artist mirror, so its membership is dropped.
        self.assertEqual(sorted(rows), sorted([
            "artist_genre|(%s,Q101,wikidata)" % tm.A,
            "artist_genre|(%s,Q102,wikidata)" % tm.A,
            "artist_genre|(%s,Q104,wikidata)" % tm.B,
        ]))
        self.assertEqual(len([r for r in first.splitlines() if r.startswith("snapshot|")]), 1)
        self.assertIn("nntune.genre-mirror.v1", first)
        self.assertIn("root|(test_old,\"Test Old\",4,t)", first)

    def test_root_view_and_retired_filter(self):
        self.apply_schema()
        self.load()
        self.assertEqual(self.view(), ["%s|test_branch" % tm.A, "%s|test_root" % tm.A])
        self.psql("-c", "UPDATE public.genre_root SET retired = true WHERE root_id = 'test_branch'")
        self.assertEqual(self.view(), ["%s|test_root" % tm.A])

    def test_manual_and_file_tag_rows_survive_refresh(self):
        self.apply_schema()
        self.load()
        self.psql("-c", "INSERT INTO public.artist_genre VALUES ('%s','Q101','file_tag'), ('%s','Q105','manual')"
                  % (tm.B, tm.B))
        before = self.dump()
        self.load()
        self.assertEqual(self.dump(), before)
        self.assertIn("(%s,Q101,file_tag)" % tm.B, before)
        self.assertIn("(%s,Q105,manual)" % tm.B, before)
        # Wikidata rows are replaced, never promoted, and stale ones disappear.
        self.psql("-c", "INSERT INTO public.artist_genre VALUES ('%s','Q100','wikidata')" % tm.B)
        self.load()
        self.assertNotIn("(%s,Q100,wikidata)" % tm.B, self.dump())

    def test_manual_overrides_lower_priority_even_when_unresolved(self):
        self.apply_schema()
        self.load()
        self.psql("-c", "INSERT INTO public.artist_genre VALUES ('%s','Q104','manual')" % tm.A)  # no rollup rows
        self.assertEqual(self.view(), [])
        self.psql("-c", "DELETE FROM public.artist_genre WHERE source = 'manual'")
        self.psql("-c", "INSERT INTO public.artist_genre VALUES ('%s','Q105','manual')" % tm.A)
        self.assertEqual(self.view(), ["%s|test_loop" % tm.A])
        self.psql("-c", "INSERT INTO public.artist_genre VALUES ('%s','Q101','file_tag')" % tm.B)
        self.assertEqual(self.view(), ["%s|test_root" % tm.B, "%s|test_loop" % tm.A])

    def test_schema_constraints_reject_bad_rows(self):
        self.apply_schema()
        self.load()
        for sql in (
            "INSERT INTO public.artist_genre VALUES ('%s','Q101','scraped')" % tm.A,
            "INSERT INTO public.artist_genre VALUES ('%s','Q101','manual')" % tm.C,  # not a mirrored artist
            "INSERT INTO public.genre_root VALUES ('Bad-Id','x',1,false)",
            "INSERT INTO public.genre_rollup VALUES ('Q101','missing_root')",
        ):
            with self.subTest(sql):
                self.assertNotEqual(self.run_psql("-c", sql).returncode, 0)

    def test_failed_refresh_rolls_back_whole_snapshot(self):
        self.apply_schema()
        self.load()
        before = self.dump()
        sql = self.refresh.read_text("utf-8")
        self.assertIn("synthetic root alpha", sql)
        self.assertTrue(sql.rstrip().endswith("COMMIT;"))
        broken = sql.replace("synthetic root alpha", "changed label").rstrip()[: -len("COMMIT;")]
        broken += "SELECT 1/0;\nCOMMIT;\n"
        path = self.root / "broken-refresh.sql"
        path.write_text(broken, encoding="utf-8")
        self.assertNotEqual(self.run_psql("-f", path).returncode, 0)
        self.assertEqual(self.dump(), before)
        self.assertNotIn("changed label", self.dump())


if __name__ == "__main__":
    unittest.main()
