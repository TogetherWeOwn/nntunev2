#!/usr/bin/env python3
"""Unit tests for check_archive_bindings.py. Run: python3 <this file>."""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_archive_bindings import ManifestError, archive_bindings, main

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

ARCHIVE_ENV_OVERRIDE = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-audio-staging"

[[env.production.r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-archive-prod"
"""

PREVIEW_ARCHIVE_MANIFEST = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-audio-staging"
preview_bucket_name = "nntunev2-archive-staging"
"""


def _repo_with(manifests: dict[str, str]) -> Path:
    tmp = Path(tempfile.mkdtemp())
    for root in ("workers", "apps"):
        (tmp / root).mkdir()
    for rel, body in manifests.items():
        path = tmp / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
    return tmp


def test_clean_manifests_pass():
    repo = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    assert archive_bindings(repo) == [], "clean manifest must pass"


def test_archive_binding_fails():
    repo = _repo_with({"workers/stream/wrangler.toml": ARCHIVE_MANIFEST})
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_archive_env_override_fails():
    repo = _repo_with({"workers/stream/wrangler.toml": ARCHIVE_ENV_OVERRIDE})
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-prod"


def test_preview_archive_binding_fails():
    repo = _repo_with({"workers/stream/wrangler.toml": PREVIEW_ARCHIVE_MANIFEST})
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_staged_manifest_archive_binding_fails():
    repo = _repo_with(
        {"workers/stream/wrangler.staging.toml": ARCHIVE_MANIFEST}
    )
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_unrelated_bucket_name_is_ignored():
    manifest = CLEAN_MANIFEST + '\n[vars]\nbucket_name = "archive notes"\n'
    repo = _repo_with({"apps/web/wrangler.toml": manifest})
    assert archive_bindings(repo) == [], "unrelated variable must not fail"


def test_commented_example_binding_ignored():
    body = (
        "# [[r2_buckets]]\n"
        '# binding = "ART_CACHE"\n'
        '# bucket_name = "nntunev2-archive-staging"\n'
    )
    repo = _repo_with({"workers/mb/wrangler.toml.example": body})
    assert archive_bindings(repo) == [], "commented bindings must not fail"


def _assert_rejected(repo: Path, message: str) -> None:
    try:
        archive_bindings(repo)
    except ManifestError as exc:
        assert message in str(exc), f"expected {message!r} in {exc!r}"
    else:
        raise AssertionError(f"expected scan to fail with {message!r}")


def test_malformed_manifest_fails_closed():
    repo = _repo_with({"workers/stream/wrangler.toml": "[[r2_buckets]\n"})
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert main(repo) == 1, "malformed manifest must fail the guard"
    assert "FAIL: cannot parse Wrangler manifest" in output.getvalue()


def test_missing_scan_root_fails():
    repo = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    (repo / "apps").rmdir()
    _assert_rejected(repo, "required scan root is missing")


def test_empty_scan_fails():
    repo = _repo_with({})
    _assert_rejected(repo, "no Wrangler TOML manifests were scanned")


def test_json_manifests_are_rejected():
    for name in ("wrangler.json", "wrangler.jsonc"):
        repo = _repo_with({f"workers/stream/{name}": '{"name":"stream"}'})
        _assert_rejected(repo, "unsupported Wrangler manifest")


def test_current_repo_manifests_pass():
    repo = Path(__file__).resolve().parent.parent.parent
    hits = archive_bindings(repo)
    assert hits == [], f"current manifests must pass, got {hits}"


if __name__ == "__main__":
    test_clean_manifests_pass()
    test_archive_binding_fails()
    test_archive_env_override_fails()
    test_preview_archive_binding_fails()
    test_staged_manifest_archive_binding_fails()
    test_unrelated_bucket_name_is_ignored()
    test_commented_example_binding_ignored()
    test_malformed_manifest_fails_closed()
    test_missing_scan_root_fails()
    test_empty_scan_fails()
    test_json_manifests_are_rejected()
    test_current_repo_manifests_pass()
    print("check_archive_bindings: all tests passed")
