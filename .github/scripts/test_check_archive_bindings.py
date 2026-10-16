#!/usr/bin/env python3
"""Unit tests for check_archive_bindings.py. Run: python3 <this file>."""

from __future__ import annotations

import contextlib
import io
import os
import subprocess
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
    subprocess.run(["git", "init", "--quiet", str(tmp)], check=True)
    for root in ("workers", "apps"):
        (tmp / root).mkdir()
    for rel, body in manifests.items():
        path = tmp / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body)
    subprocess.run(["git", "-C", str(tmp), "add", "--all"], check=True)
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


def test_custom_toml_config_filename_is_scanned():
    repo = _repo_with({"infra/archive.toml": ARCHIVE_MANIFEST})
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_bom_prefixed_toml_is_scanned():
    repo = _repo_with({"workers/stream/wrangler.toml": "\ufeff" + CLEAN_MANIFEST})
    assert archive_bindings(repo) == []


def test_custom_json_path_with_archive_binding_is_rejected():
    body = '{"r2_buckets":[{"bucket_name":"nntunev2-archive-prod"}]}'
    repo = _repo_with({"config/worker.json": body})
    _assert_rejected(repo, "unsupported Wrangler manifest")


def test_non_worker_toml_without_r2_binding_is_ignored():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "workers/tool/deny.toml": 'env = "staging"',
            "infra/malformed.toml": (
                'description = "r2_buckets" # not a binding\n'
                "not = [valid TOML"
            ),
        }
    )
    assert archive_bindings(repo) == []


def test_malformed_custom_toml_with_binding_table_fails_closed():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "infra/malformed-binding.toml": (
                '[[env."pro#d".r2_buckets]\nbucket_name = \'archive\''
            ),
        }
    )
    _assert_rejected(repo, "cannot parse TOML config")


def test_uppercase_toml_extension_is_scanned():
    repo = _repo_with({"workers/stream/wrangler.TOML": ARCHIVE_MANIFEST})
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_manifest_outside_scan_roots_fails():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "wrangler.toml": ARCHIVE_MANIFEST,
        }
    )
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"
    assert hits[0][0] == repo / "wrangler.toml"


def test_nested_manifest_outside_scan_roots_fails():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "crates/x/wrangler.toml": ARCHIVE_MANIFEST,
        }
    )
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][1] == "nntunev2-archive-staging"


def test_manifest_in_git_named_directory_is_scanned():
    repo = _repo_with(
        {
            "workers/foo.git/wrangler.toml": ARCHIVE_MANIFEST,
            "apps/web/wrangler.toml": CLEAN_MANIFEST,
        }
    )
    hits = archive_bindings(repo)
    assert len(hits) == 1, f"expected one hit, got {hits}"
    assert hits[0][0] == repo / "workers/foo.git/wrangler.toml"


def test_untracked_dependency_manifest_is_ignored():
    repo = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    dependency = repo / "node_modules/wrangler.toml"
    dependency.parent.mkdir(parents=True)
    dependency.write_text(ARCHIVE_MANIFEST)
    assert archive_bindings(repo) == [], "untracked dependency manifest must be ignored"


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
    assert "FAIL: cannot parse TOML config" in output.getvalue()


def test_missing_scan_root_fails():
    repo = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    (repo / "apps").rmdir()
    _assert_rejected(repo, "required scan root is missing")


def test_empty_scan_fails():
    repo = _repo_with({})
    _assert_rejected(repo, "no Wrangler TOML manifests were scanned")


def test_json_manifests_are_rejected():
    for name in ("wrangler.json", "wrangler.jsonc"):
        repo = _repo_with(
            {f"workers/stream/{name}": '{"name":"stream","compatibility_date":"2025-01-01"}'}
        )
        _assert_rejected(repo, "unsupported Wrangler manifest")


def test_jsonc_manifest_with_leading_comment_is_rejected():
    body = '// Wrangler config\n{"name":"stream","r2_buckets":[]}'
    repo = _repo_with({"workers/stream/wrangler.jsonc": body})
    _assert_rejected(repo, "unsupported Wrangler manifest")


def test_malformed_wrangler_named_document_without_config_markers_is_ignored():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "docs/wrangler-notes.json": '{"note": "documentation only",',
            "docs/wrangler.json": '{"note": "documentation only",',
        }
    )
    assert archive_bindings(repo) == []


def test_json_env_archive_binding_is_rejected_as_unsupported():
    body = (
        '{"name":"stream","env":{"production":{"r2_buckets":'
        '[{"bucket_name":"nntunev2-archive-prod"}]}}}'
    )
    repo = _repo_with({"workers/stream/wrangler-staging.json": body})
    _assert_rejected(repo, "unsupported Wrangler manifest")


def test_nonstandard_json_manifest_is_rejected():
    repo = _repo_with(
        {"workers/stream/wrangler-staging.json": '{"name":"stream","compatibility_date":"2025-01-01"}'}
    )
    _assert_rejected(repo, "unsupported Wrangler manifest")


def test_nonstandard_json_config_outside_worker_roots_is_rejected():
    repo = _repo_with({"crates/worker/wrangler.json": '{"name":"worker","compatibility_date":"2025-01-01"}'})
    _assert_rejected(repo, "unsupported Wrangler manifest")


def test_wrangler_named_document_is_not_treated_as_a_manifest():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "docs/wrangler-notes.md": "archive references are documentation, not bindings",
            "docs/wrangler-notes.json": '{"note":"not a Wrangler config"}',
            "docs/wrangler.toml": CLEAN_MANIFEST,
            "workers/config/wrangler.schema.json": '{"type":"object"}',
            "workers/config/wrangler-notes.json": '{"note":"archive bucket examples","main":"entry notes"}',
            "workers/config/wrangler-list.json": '[{"name":"not a config"}]',
        }
    )
    assert archive_bindings(repo) == []


def test_array_environment_table_fails_closed():
    manifest = """\
[[env]]
[[env.r2_buckets]]
binding = "ARCHIVE"
bucket_name = "nntunev2-archive-staging"
"""
    repo = _repo_with({"workers/stream/wrangler.toml": manifest})
    _assert_rejected(repo, "invalid env table")


def test_invalid_env_table_fails_closed():
    repo = _repo_with({"workers/stream/wrangler.toml": 'env = "oops"'})
    _assert_rejected(repo, "invalid env table")


def test_non_utf8_unrelated_path_does_not_break_scan():
    repo = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    bad_path = os.fsencode(repo) + b"/workers/\xff-not-a-config.txt"
    with open(bad_path, "wb") as unrelated:
        unrelated.write(b"ordinary tracked file")
    subprocess.run(["git", "-C", str(repo), "add", "--all"], check=True)
    assert archive_bindings(repo) == []


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
    test_custom_toml_config_filename_is_scanned()
    test_bom_prefixed_toml_is_scanned()
    test_custom_json_path_with_archive_binding_is_rejected()
    test_non_worker_toml_without_r2_binding_is_ignored()
    test_malformed_custom_toml_with_binding_table_fails_closed()
    test_uppercase_toml_extension_is_scanned()
    test_manifest_outside_scan_roots_fails()
    test_nested_manifest_outside_scan_roots_fails()
    test_manifest_in_git_named_directory_is_scanned()
    test_untracked_dependency_manifest_is_ignored()
    test_unrelated_bucket_name_is_ignored()
    test_commented_example_binding_ignored()
    test_malformed_manifest_fails_closed()
    test_missing_scan_root_fails()
    test_empty_scan_fails()
    test_json_manifests_are_rejected()
    test_jsonc_manifest_with_leading_comment_is_rejected()
    test_malformed_wrangler_named_document_without_config_markers_is_ignored()
    test_json_env_archive_binding_is_rejected_as_unsupported()
    test_nonstandard_json_manifest_is_rejected()
    test_nonstandard_json_config_outside_worker_roots_is_rejected()
    test_wrangler_named_document_is_not_treated_as_a_manifest()
    test_array_environment_table_fails_closed()
    test_invalid_env_table_fails_closed()
    test_non_utf8_unrelated_path_does_not_break_scan()
    test_current_repo_manifests_pass()
    print("check_archive_bindings: all tests passed")
