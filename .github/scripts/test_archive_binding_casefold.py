#!/usr/bin/env python3
"""Case-fold pins for check_archive_bindings.py. Run: python3 <this file>.

Uppercase ARCHIVE and mixed-case ArChIvE bucket names must be rejected at
the top level, env override and preview_bucket_name positions; hits retain
the original bucket spelling. Clean non-archive and uppercase AUDIO names
must keep passing. Uses local temporary git repos and synthetic buckets
only; no Cloudflare API, bucket access or network.
"""

from __future__ import annotations

import contextlib
import io
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_archive_bindings import archive_bindings, main

UPPER_BUCKET = "NNTUNEV2-ARCHIVE-STAGING"
MIXED_BUCKET = "nntunev2-ArChIvE-staging"

CLEAN_MANIFEST = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-audio-staging"
"""

UPPER_AUDIO_MANIFEST = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "NNTUNEV2-AUDIO-STAGING"
"""


def _manifest_with(bucket: str, key: str = "bucket_name") -> str:
    return (
        'name = "nntune-stream"\n'
        "\n"
        "[[r2_buckets]]\n"
        'binding = "CACHE"\n'
        f'{key} = "{bucket}"\n'
    )


def _env_manifest_with(bucket: str) -> str:
    return (
        'name = "nntune-stream"\n'
        "\n"
        "[[r2_buckets]]\n"
        'binding = "AUDIO_CACHE"\n'
        'bucket_name = "nntunev2-audio-staging"\n'
        "\n"
        "[[env.production.r2_buckets]]\n"
        'binding = "AUDIO_CACHE"\n'
        f'bucket_name = "{bucket}"\n'
    )


def _repo_with(manifests: dict[str, str]) -> Path:
    tmp = Path(tempfile.mkdtemp())
    subprocess.run(["git", "init", "--quiet", str(tmp)], check=True)
    for root in ("workers", "apps"):
        (tmp / root).mkdir()
    for rel, body in manifests.items():
        path = tmp / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
    subprocess.run(["git", "-C", str(tmp), "add", "--all"], check=True)
    return tmp


def _assert_rejected_with_spelling(bucket: str, manifests: dict[str, str]) -> None:
    repo = _repo_with(manifests)
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit for {bucket!r}, got {hits}"
    assert hits[0][1] == bucket, (
        f"hit must retain original spelling {bucket!r}, got {hits[0][1]!r}"
    )


def test_uppercase_top_level_rejected():
    _assert_rejected_with_spelling(
        UPPER_BUCKET,
        {"workers/stream/wrangler.toml": _manifest_with(UPPER_BUCKET)},
    )


def test_mixed_case_top_level_rejected():
    _assert_rejected_with_spelling(
        MIXED_BUCKET,
        {"workers/stream/wrangler.toml": _manifest_with(MIXED_BUCKET)},
    )


def test_uppercase_env_override_rejected():
    _assert_rejected_with_spelling(
        UPPER_BUCKET,
        {"workers/stream/wrangler.toml": _env_manifest_with(UPPER_BUCKET)},
    )


def test_mixed_case_env_override_rejected():
    _assert_rejected_with_spelling(
        MIXED_BUCKET,
        {"workers/stream/wrangler.toml": _env_manifest_with(MIXED_BUCKET)},
    )


def test_uppercase_preview_bucket_rejected():
    _assert_rejected_with_spelling(
        UPPER_BUCKET,
        {
            "workers/stream/wrangler.toml": _manifest_with(
                UPPER_BUCKET, key="preview_bucket_name"
            )
        },
    )


def test_mixed_case_preview_bucket_rejected():
    _assert_rejected_with_spelling(
        MIXED_BUCKET,
        {
            "workers/stream/wrangler.toml": _manifest_with(
                MIXED_BUCKET, key="preview_bucket_name"
            )
        },
    )


def test_uppercase_main_exits_reserved_bucket():
    repo = _repo_with(
        {"workers/stream/wrangler.toml": _manifest_with(UPPER_BUCKET)}
    )
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert main(repo) == 1, "uppercase archive binding must fail the guard"
    text = output.getvalue()
    assert "reserved archive" in text, f"expected reserved-bucket finding, got {text!r}"
    assert UPPER_BUCKET in text, f"expected original spelling in output, got {text!r}"


def test_clean_and_uppercase_audio_pass():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "apps/web/wrangler.toml": UPPER_AUDIO_MANIFEST,
        }
    )
    assert archive_bindings(repo) == [], "clean and AUDIO names must pass"


if __name__ == "__main__":
    test_uppercase_top_level_rejected()
    test_mixed_case_top_level_rejected()
    test_uppercase_env_override_rejected()
    test_mixed_case_env_override_rejected()
    test_uppercase_preview_bucket_rejected()
    test_mixed_case_preview_bucket_rejected()
    test_uppercase_main_exits_reserved_bucket()
    test_clean_and_uppercase_audio_pass()
    print("archive_binding_casefold: all tests passed")
