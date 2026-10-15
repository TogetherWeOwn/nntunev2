#!/usr/bin/env python3
"""Unit tests for check_archive_bindings.py. Run: python3 <this file>."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_archive_bindings import archive_bindings

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


def _repo_with(manifests: dict[str, str]) -> Path:
    tmp = Path(tempfile.mkdtemp())
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


def test_commented_example_binding_ignored():
    body = (
        "# [[r2_buckets]]\n"
        '# binding = "ART_CACHE"\n'
        '# bucket_name = "nntunev2-archive-staging"\n'
    )
    repo = _repo_with({"workers/mb/wrangler.toml.example": body})
    assert archive_bindings(repo) == [], "commented bindings must not fail"


def test_current_repo_manifests_pass():
    repo = Path(__file__).resolve().parent.parent.parent
    hits = archive_bindings(repo)
    assert hits == [], f"current manifests must pass, got {hits}"


if __name__ == "__main__":
    test_clean_manifests_pass()
    test_archive_binding_fails()
    test_archive_env_override_fails()
    test_commented_example_binding_ignored()
    test_current_repo_manifests_pass()
    print("check_archive_bindings: all tests passed")
