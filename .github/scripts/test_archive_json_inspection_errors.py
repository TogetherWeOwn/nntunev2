#!/usr/bin/env python3
"""Pin archive-guard JSON/JSONC inspection failures. Run: python3 -B <this file>.

Pins ``check_archive_bindings.py`` JSON candidate inspection failing closed:
``_is_unsupported_json_candidate`` wraps ``OSError``/``UnicodeError`` in
``ManifestError`` naming the candidate (``cannot inspect JSON config``).
Every fixture keeps an otherwise-clean tracked TOML manifest so an
empty-scan failure can never satisfy these assertions.
"""

from __future__ import annotations

import contextlib
import io
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_archive_bindings import ManifestError, archive_bindings, main

CLEAN_MANIFEST = """\
name = "nntune-stream"

[[r2_buckets]]
binding = "AUDIO_CACHE"
bucket_name = "nntunev2-audio-staging"
"""

JSON_CONFIG = '{"name":"stream","compatibility_date":"2025-01-01"}'
JSONC_CONFIG = '// Wrangler config\n{"name":"stream","r2_buckets":[]}'


def _repo_with(entries: dict[str, str | bytes]) -> Path:
    tmp = Path(tempfile.mkdtemp())
    subprocess.run(["git", "init", "--quiet", str(tmp)], check=True)
    for root in ("workers", "apps"):
        (tmp / root).mkdir()
    for rel, body in entries.items():
        path = tmp / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(body, bytes):
            path.write_bytes(body)
        else:
            path.write_text(body)
    subprocess.run(["git", "-C", str(tmp), "add", "--all"], check=True)
    return tmp


def _assert_inspection_failure(repo: Path, candidate: str) -> None:
    try:
        archive_bindings(repo)
    except ManifestError as exc:
        message = str(exc)
        assert "cannot inspect JSON config" in message, f"unexpected error: {exc!r}"
        assert candidate in message, f"candidate not named: {exc!r}"
    else:
        raise AssertionError(f"expected JSON inspection failure for {candidate}")


def _selective_oserror(repo: Path, candidate_rel: str):
    """Fail read_text only for one tracked JSON candidate."""
    target = repo / candidate_rel
    original = Path.read_text

    def failing_read_text(self, *args, **kwargs):
        if self == target:
            raise OSError("injected inspection failure")
        return original(self, *args, **kwargs)

    return mock.patch.object(Path, "read_text", failing_read_text)


def test_tracked_json_inspection_oserror_fails_closed():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "config/worker.json": JSON_CONFIG,
        }
    )
    with _selective_oserror(repo, "config/worker.json"):
        _assert_inspection_failure(repo, "worker.json")


def test_tracked_jsonc_inspection_oserror_fails_closed():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "config/worker.jsonc": JSONC_CONFIG,
        }
    )
    with _selective_oserror(repo, "config/worker.jsonc"):
        _assert_inspection_failure(repo, "worker.jsonc")


def test_invalid_utf8_json_bytes_fail_closed():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "workers/stream/wrangler.json": b"\xff\xfe\x00not-utf8 \x80\x81",
        }
    )
    _assert_inspection_failure(repo, "wrangler.json")


def test_main_reports_guard_failure_not_success():
    repo = _repo_with(
        {
            "workers/stream/wrangler.toml": CLEAN_MANIFEST,
            "workers/stream/wrangler.json": b"\xff\xfe\x00not-utf8 \x80\x81",
        }
    )
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        status = main(repo)
    assert status == 1, f"expected exit 1, got {status}"
    text = output.getvalue()
    assert "archive guard: FAIL:" in text, f"expected guard failure, got {text!r}"
    assert "cannot inspect JSON config" in text, f"expected cause, got {text!r}"
    assert "no Worker R2 binding targets an archive bucket" not in text, (
        f"clean-success message must not appear, got {text!r}"
    )
    # Control: the same clean TOML manifest without the bad JSON passes.
    control = _repo_with({"workers/stream/wrangler.toml": CLEAN_MANIFEST})
    control_output = io.StringIO()
    with contextlib.redirect_stdout(control_output):
        control_status = main(control)
    assert control_status == 0, f"expected exit 0, got {control_status}"
    assert "no Worker R2 binding targets an archive bucket" in control_output.getvalue()


if __name__ == "__main__":
    test_tracked_json_inspection_oserror_fails_closed()
    test_tracked_jsonc_inspection_oserror_fails_closed()
    test_invalid_utf8_json_bytes_fail_closed()
    test_main_reports_guard_failure_not_success()
    print("archive_json_inspection_errors: all tests passed")
