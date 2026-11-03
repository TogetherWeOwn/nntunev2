"""Tests for fetch_fixture.py (the CC0 fixture fetch-by-key helper).

Everything runs offline: a loopback HTTP server stands in for the
unsigned base-URL transport, and the S3 signer is checked by inspecting
the request it builds. No R2 credentials are needed or used.
"""

import hashlib
import http.server
import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.parse
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import fetch_fixture
from fetch_fixture import EXIT_FAIL, EXIT_OK, EXIT_SKIP, main, s3_get_url

FILE = "flac/01 - tone.flac"
BODY = b"fLaC" + bytes(range(256)) * 64


def write_manifest(root: Path, body: bytes = BODY) -> Path:
    album = root / "fixtures/cc0/test-album"
    album.mkdir(parents=True)
    manifest = {
        "license": "CC0-1.0",
        "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
        "source": "https://example.org/test-album",
        "tracks": [
            {
                "position": 1,
                "file": FILE,
                "bytes": len(body),
                "sha256": hashlib.sha256(body).hexdigest(),
            }
        ],
    }
    path = album / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    return path


class Handler(http.server.BaseHTTPRequestHandler):
    hits = 0
    body = BODY

    def do_GET(self):  # noqa: N802 (stdlib handler name)
        type(self).hits += 1
        key = urllib.parse.unquote(self.path.lstrip("/"))
        if key != f"fixtures/cc0/test-album/{FILE}":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(type(self).body)))
        self.end_headers()
        self.wfile.write(type(self).body)

    def log_message(self, *args):  # keep test output quiet
        pass


class FetchFixtureTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.manifest = write_manifest(self.root)
        self.cache = self.root / "cache"
        self._real_root = fetch_fixture.repo_root
        fetch_fixture.repo_root = lambda: self.root
        Handler.hits = 0
        Handler.body = BODY
        self.server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        fetch_fixture.repo_root = self._real_root
        self.server.shutdown()
        self.tmp.cleanup()

    def env(self, **overrides):
        env = {"NNTUNE_FIXTURE_CACHE_DIR": str(self.cache)}
        env.update(overrides)
        return env

    def args(self, *extra):
        return ["--manifest", str(self.manifest), *extra]

    def test_skip_when_no_transport_configured(self):
        self.assertEqual(EXIT_SKIP, main(self.args("--position", "1"), env=self.env()))

    def test_fetch_by_position_via_base_url(self):
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_OK, main(self.args("--position", "1"), env=env))
        cached = self.cache / f"fixtures/cc0/test-album/{FILE}"
        self.assertEqual(BODY, cached.read_bytes())
        self.assertEqual(1, Handler.hits)

    def test_cache_hit_does_not_redownload(self):
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_OK, main(self.args(FILE), env=env))
        self.assertEqual(EXIT_OK, main(self.args(FILE), env=env))
        self.assertEqual(1, Handler.hits)

    def test_hash_mismatch_fails_and_caches_nothing(self):
        Handler.body = b"not the fixture"
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_FAIL, main(self.args("--position", "1"), env=env))
        self.assertFalse((self.cache / f"fixtures/cc0/test-album/{FILE}").exists())

    def test_unlisted_file_is_refused_before_any_request(self):
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_FAIL, main(self.args("flac/99 - other.flac"), env=env))
        self.assertEqual(0, Handler.hits)

    def test_unknown_position_fails(self):
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_FAIL, main(self.args("--position", "9"), env=env))

    def test_corrupt_cache_is_replaced_by_fresh_download(self):
        dest = self.cache / f"fixtures/cc0/test-album/{FILE}"
        dest.parent.mkdir(parents=True)
        dest.write_bytes(b"stale")
        env = self.env(NNTUNE_FIXTURE_BASE_URL=self.base_url)
        self.assertEqual(EXIT_OK, main(self.args("--position", "1"), env=env))
        self.assertEqual(BODY, dest.read_bytes())


class S3SignerTest(unittest.TestCase):
    def test_signed_request_shape(self):
        req = s3_get_url(
            "https://acct.r2.cloudflarestorage.com",
            "nntunev2-audio-staging",
            f"fixtures/cc0/test-album/{FILE}",
            "AKIDEXAMPLE",
            "secret",
        )
        self.assertEqual(
            "https://acct.r2.cloudflarestorage.com/nntunev2-audio-staging/"
            "fixtures/cc0/test-album/flac/01%20-%20tone.flac",
            req.full_url,
        )
        auth = req.get_header("Authorization")
        self.assertTrue(auth.startswith("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"))
        self.assertIn("/auto/s3/aws4_request", auth)
        self.assertIn("SignedHeaders=host;x-amz-content-sha256;x-amz-date", auth)
        self.assertEqual(
            hashlib.sha256(b"").hexdigest(), req.get_header("X-amz-content-sha256")
        )
        self.assertTrue(req.get_header("X-amz-date").endswith("Z"))

    def test_s3_transport_selected_when_creds_present(self):
        req = fetch_fixture.build_request(
            f"fixtures/cc0/test-album/{FILE}",
            {
                "NNTUNE_FIXTURE_R2_ACCESS_KEY_ID": "k",
                "NNTUNE_FIXTURE_R2_SECRET_ACCESS_KEY": "s",
                "NNTUNE_FIXTURE_R2_ACCOUNT_ID": "acct123",
            },
        )
        self.assertTrue(
            req.full_url.startswith(
                "https://acct123.r2.cloudflarestorage.com/nntunev2-audio-staging/"
            )
        )


class CommittedManifestTest(unittest.TestCase):
    """The real album manifest in this directory must load and list 4 tracks."""

    def test_committed_manifest_loads(self):
        manifest, prefix = fetch_fixture.load_manifest(None)
        self.assertEqual(
            "fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593", prefix
        )
        self.assertEqual("CC0-1.0", manifest["license"])
        self.assertEqual(4, len(manifest["tracks"]))
        track = fetch_fixture.select_track(manifest, None, 1)
        self.assertEqual("flac/01 - 1 (jazz).flac", track["file"])


if __name__ == "__main__":
    unittest.main()
