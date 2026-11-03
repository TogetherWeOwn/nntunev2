"""Unit tests for check_fixture_licences.

FIXTURE NOTE: this file holds manufactured violations (unlicensed
manifests, unlisted R2 keys) to trial the check. It is excluded from
the guard's key scan via KEY_SCAN_EXCLUSIONS — exact path only.
"""

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_fixture_licences import check_manifest, check_tracked_files

MANIFEST_PATH = "fixtures/cc0/album/manifest.json"
SHA = "a" * 64


def manifest(**overrides):
    data = {
        "license": "CC0-1.0",
        "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
        "source": "https://example.org/album",
        "tracks": [{"file": "flac/01.flac", "bytes": 123, "sha256": SHA}],
    }
    data.update(overrides)
    return json.dumps(data)


def rules(violations):
    return sorted({rule for _, _, rule in violations})


def scan(files):
    """files: {path: text}. Returns violations from check_tracked_files."""
    return check_tracked_files(sorted(files), lambda p: files[p])


class TestManifest(unittest.TestCase):
    def test_valid_manifest_lists_its_keys(self):
        violations, keys = check_manifest(MANIFEST_PATH, manifest())
        self.assertEqual([], violations)
        self.assertEqual({"fixtures/cc0/album/flac/01.flac"}, keys)

    def test_unparseable_manifest(self):
        violations, keys = check_manifest(MANIFEST_PATH, "{nope")
        self.assertEqual([(1, "manifest-shape")], violations)
        self.assertEqual(set(), keys)

    def test_disallowed_licence_lists_nothing(self):
        violations, keys = check_manifest(
            MANIFEST_PATH, manifest(license="CC-BY-NC-SA-4.0")
        )
        self.assertIn((1, "manifest-licence"), violations)
        self.assertEqual(set(), keys)

    def test_missing_licence_fields(self):
        for field in ("license", "license_url", "source"):
            data = json.loads(manifest())
            del data[field]
            violations, _ = check_manifest(MANIFEST_PATH, json.dumps(data))
            self.assertIn((1, "manifest-licence"), violations, field)

    def test_bad_track_shapes(self):
        bad_tracks = [
            [{"bytes": 1, "sha256": SHA}],  # no file
            [{"file": "f.flac", "bytes": 0, "sha256": SHA}],  # zero bytes
            [{"file": "f.flac", "bytes": "1", "sha256": SHA}],  # bytes not int
            [{"file": "f.flac", "bytes": True, "sha256": SHA}],  # bool is not int
            [{"file": "f.flac", "bytes": 1, "sha256": "xyz"}],  # bad sha
        ]
        for tracks in bad_tracks:
            violations, keys = check_manifest(MANIFEST_PATH, manifest(tracks=tracks))
            self.assertIn((1, "manifest-shape"), violations, tracks)
            self.assertEqual(set(), keys, tracks)

    def test_empty_tracks_is_a_shape_error(self):
        violations, _ = check_manifest(MANIFEST_PATH, manifest(tracks=[]))
        self.assertIn((1, "manifest-shape"), violations)


class TestTrackedFiles(unittest.TestCase):
    def test_clean_repo(self):
        files = {
            MANIFEST_PATH: manifest(),
            "crates/x/tests/t.rs": 'let p = "fixtures/cc0/album/flac/01.flac";',
            "crates/x/tests/parsers.rs": 'const NAME: &str = "track.flac";',
        }
        self.assertEqual([], scan(files))

    def test_tracked_audio_outside_manifest_fails(self):
        files = {MANIFEST_PATH: manifest(), "assets/song.mp3": "bytes"}
        self.assertEqual(["tracked-audio-unlisted"], rules(scan(files)))

    def test_tracked_nzb_fails(self):
        files = {MANIFEST_PATH: manifest(), "fixtures/x/release.nzb": "<nzb/>"}
        self.assertEqual(["tracked-audio-unlisted"], rules(scan(files)))

    def test_manifest_listed_audio_may_be_tracked(self):
        files = {
            MANIFEST_PATH: manifest(),
            "fixtures/cc0/album/flac/01.flac": "fLaC",
        }
        self.assertEqual([], scan(files))

    def test_unlicensed_manifest_does_not_cover_its_audio(self):
        files = {
            MANIFEST_PATH: manifest(license="All-Rights-Reserved"),
            "fixtures/cc0/album/flac/01.flac": "fLaC",
        }
        self.assertEqual(
            ["manifest-licence", "tracked-audio-unlisted"], rules(scan(files))
        )

    def test_code_referencing_unlisted_key_fails(self):
        files = {
            MANIFEST_PATH: manifest(),
            "e2e/tests/play.spec.ts": "const k = 'fixtures/cc0/album/flac/99.flac';",
        }
        violations = scan(files)
        self.assertEqual(["audio-key-unlisted"], rules(violations))
        self.assertEqual(("e2e/tests/play.spec.ts", 1), violations[0][:2])

    def test_subsonic_manifest_free_fixtures_untouched(self):
        # Non-audio JSON fixtures and manifests outside fixtures/ are
        # not this guard's business.
        files = {
            "fixtures/subsonic/ping.json": "{}",
            "apps/web/static/manifest.json": "{}",
        }
        self.assertEqual([], scan(files))

    def test_guard_and_its_tests_are_key_scan_exempt(self):
        files = {
            MANIFEST_PATH: manifest(),
            ".github/scripts/check_fixture_licences.py": "# fixtures/cc0/x/y.flac",
            ".github/scripts/test_check_fixture_licences.py": "# fixtures/cc0/x/y.flac",
        }
        self.assertEqual([], scan(files))


class TestCommittedManifest(unittest.TestCase):
    def test_repo_manifest_passes(self):
        root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
        rel = "fixtures/cc0/holiznacc0-busted-guitar-jazz-66ef0593/manifest.json"
        with open(os.path.join(root, rel), encoding="utf-8") as handle:
            violations, keys = check_manifest(rel, handle.read())
        self.assertEqual([], violations)
        self.assertEqual(4, len(keys))


if __name__ == "__main__":
    unittest.main()
