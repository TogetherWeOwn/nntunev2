import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
MB = ROOT / "containers/mb-import"


def load(path):
    spec = importlib.util.spec_from_file_location(path, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BoundaryTests(unittest.TestCase):
    def shell(self, *args, **env):
        return subprocess.run(["bash", *args], env={"PATH": os.defpath, **env},
                              capture_output=True, text=True, timeout=5)

    def test_import_and_retirement_admission(self):
        for script, args in (("import.sh", []), ("etl.sh", ["--help", "extra"]),
                             ("replicate.sh", ["--help"])):
            with self.subTest(script=script):
                self.assertEqual(self.shell(MB / script, *args).returncode, 2)
        with tempfile.TemporaryDirectory() as tmp:
            result = self.shell(MB / "import.sh", tmp, DATABASE_URL="synthetic",
                                MB_IMPORT_CHUNK_LINES=str(2**64 + 1))
        self.assertIn("MB_IMPORT_CHUNK_LINES", result.stderr)
        self.assertNotEqual(result.returncode, 0)

    def test_pg_uri_refusals(self):
        for url in ("postgres://user:synthetic%xy@host/db",
                    "postgres://user:synthetic@h%2cother/db", "postgres://h/db?sslmode",
                    "postgres://user:synthetic\t@host/db", "postgres://[::1]suffix/db"):
            with self.subTest(url=url):
                result = self.shell("-c", '. "$1"; configure_pg_env_from_database_url',
                                    "pg-env-test", MB / "pg-env.sh", DATABASE_URL=url)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn("synthetic", result.stderr)

    def test_graph_and_override_refusals(self):
        graph = load("containers/genre-mirror/graph.py")
        overrides = load("containers/genre-mirror/overrides.py")
        with self.assertRaises(ValueError):
            graph.Graph({}, (), True, 3)
        action = {"genre_qid": "Q1", "action": "add", "roots": ["a"]}
        with self.assertRaises(ValueError):
            overrides.Overrides({"overrides": [action, action]}, Mock(sort_order={"a": 1}), ())

    def test_unverified_download_keeps_existing_cache(self):
        fetch = load("fixtures/cc0/fetch_fixture.py")
        track = {"file": "track.flac", "position": 1, "bytes": 4, "sha256": hashlib.sha256(b"good").hexdigest()}
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "track.flac"
            dest.write_bytes(b"old!")
            opener = Mock(open=lambda *args, **kwargs: io.BytesIO(b"evil!"))
            env = {fetch.ENV_CACHE_DIR: tmp, fetch.ENV_BASE_URL: "https://fixtures.invalid"}
            with patch.object(fetch, "load_manifest", return_value=({"tracks": [track]}, ".")), patch.object(
                fetch.urllib.request, "build_opener", return_value=opener
            ):
                with self.assertRaises(fetch.FetchError):
                    fetch.fetch(None, 1, None, env)
            self.assertEqual(dest.read_bytes(), b"old!")
            self.assertEqual(list(dest.parent.iterdir()), [dest])

    def test_burst_refuses_bad_origin_before_curl(self):
        script = ROOT / "workers/ingest/scripts/search_rate_limit_burst.sh"
        with tempfile.TemporaryDirectory() as tmp:
            curl = Path(tmp) / "curl"
            curl.write_text("#!/bin/bash\nexit 97\n")
            curl.chmod(0o755)
            result = self.shell(script, INGEST_GRANT="v1.ab." + "ab" * 32,
                                INGEST_URL="file:///tmp", PATH=f"{tmp}:{os.defpath}")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
