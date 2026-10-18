#!/usr/bin/env python3
"""Byte-path regression tests for check_archive_bindings.py.

Run: python3 -B .github/scripts/test_archive_manifest_byte_paths.py

Proves a Wrangler TOML manifest staged under a directory containing
non-UTF8 filesystem bytes is still discovered by the archive guard:
a clean effective manifest yields no hits, and an archive-targeting
binding in the same byte path yields the expected hit and guard
failure -- never a silent omission. Manifest identity is asserted with
an os.fsencode/filesystem-byte round-trip, not lossy text replacement.
"""

from __future__ import annotations

import contextlib
import io
import os
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_archive_bindings import archive_bindings, main

CLEAN_MANIFEST = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-audio-staging"
"""

ARCHIVE_MANIFEST = """\
name = "nntune-evil"

[[r2_buckets]]
binding = "DUMP"
bucket_name = "nntunev2-archive-staging"
"""

# Non-UTF8 directory fragment: 0xFF is never valid in UTF-8, so the
# on-disk name cannot survive a lossy text decode.
BYTE_DIRNAME = b"\xff-byte-dir"


def _repo_with_byte_manifest(body: str) -> tuple[Path, Path, bytes]:
    """Create a temp git repo with wrangler.toml under a non-UTF8 directory.

    Returns (repo, manifest, rel_bytes) where rel_bytes is the raw
    git-tracked relative path (e.g. b"workers/\\xff-byte-dir/wrangler.toml").
    """
    tmp = Path(tempfile.mkdtemp())
    subprocess.run(["git", "init", "--quiet", str(tmp)], check=True)
    for root in ("workers", "apps"):
        (tmp / root).mkdir()
    repo_bytes = os.fsencode(str(tmp))
    rel_bytes = b"workers/" + BYTE_DIRNAME + b"/wrangler.toml"
    dir_bytes = repo_bytes + b"/workers/" + BYTE_DIRNAME
    os.mkdir(dir_bytes)
    with open(repo_bytes + b"/" + rel_bytes, "wb") as handle:
        handle.write(body.encode("utf-8"))
    subprocess.run(["git", "-C", str(tmp), "add", "--all"], check=True)
    manifest = tmp / Path(os.fsdecode(rel_bytes))
    return tmp, manifest, rel_bytes


def _tracked_entry(repo: Path, rel_bytes: bytes) -> bytes:
    tracked = subprocess.run(
        ["git", "-C", str(repo), "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    assert rel_bytes in tracked, f"byte manifest not tracked: {tracked!r}"
    return rel_bytes


def _assert_byte_identity(repo: Path, manifest: Path, rel_bytes: bytes) -> None:
    """Assert the manifest path round-trips through filesystem bytes."""
    _tracked_entry(repo, rel_bytes)
    assert manifest.is_file(), f"byte manifest must exist: {manifest!r}"
    assert manifest.name == "wrangler.toml"
    # Identity through the same os.fsdecode seam the checker uses.
    assert manifest == repo / Path(os.fsdecode(rel_bytes))
    # Filesystem-byte round-trip: the non-UTF8 byte survives, nothing
    # was replaced with "?" or U+FFFD by a lossy text decode.
    encoded = os.fsencode(str(manifest))
    assert encoded == os.fsencode(str(repo)) + b"/" + rel_bytes
    assert b"\xff" in encoded, f"non-UTF8 byte lost: {encoded!r}"
    assert "?" not in str(manifest).rsplit("/", 1)[-1]
    assert "�" not in str(manifest)


def test_clean_manifest_under_non_utf8_dir_passes():
    repo, manifest, rel_bytes = _repo_with_byte_manifest(CLEAN_MANIFEST)
    _assert_byte_identity(repo, manifest, rel_bytes)
    assert archive_bindings(repo) == [], "clean byte-path manifest must pass"


def test_archive_manifest_under_non_utf8_dir_is_found():
    repo, manifest, rel_bytes = _repo_with_byte_manifest(ARCHIVE_MANIFEST)
    _assert_byte_identity(repo, manifest, rel_bytes)
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][0] == manifest, f"hit must name the byte manifest: {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert main(repo) == 1, "archive binding under byte path must fail guard"
    assert "nntunev2-archive-staging" in output.getvalue()


def test_unrelated_non_utf8_path_preserved_alongside_byte_manifest():
    repo, manifest, rel_bytes = _repo_with_byte_manifest(CLEAN_MANIFEST)
    unrelated = os.fsencode(str(repo)) + b"/workers/\xfe-not-a-config.txt"
    with open(unrelated, "wb") as handle:
        handle.write(b"ordinary tracked file")
    subprocess.run(["git", "-C", str(repo), "add", "--all"], check=True)
    _assert_byte_identity(repo, manifest, rel_bytes)
    assert archive_bindings(repo) == [], "unrelated byte path must not break scan"


if __name__ == "__main__":
    test_clean_manifest_under_non_utf8_dir_passes()
    test_archive_manifest_under_non_utf8_dir_is_found()
    test_unrelated_non_utf8_path_preserved_alongside_byte_manifest()
    print("archive_manifest_byte_paths: all tests passed")
