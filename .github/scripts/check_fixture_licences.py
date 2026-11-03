#!/usr/bin/env python3
"""Fixture licence guard: all dev/test audio is CC0 (or reviewed-equal).

Owner rule (2026-11-03, stated in docs/fixture-audio-licences.md):
every audio file used by dev, tests, CI or staging must be CC0 1.0 or
an equally permissive, redistribution-allowed licence, recorded in a
committed manifest. No commercial music and no commercial NZBs. This
script enforces the mechanically checkable half; adding a fixture with
a new licence still needs the review the doc describes.

Rules (rule id -> shape):

  tracked-audio-unlisted
    A git-tracked audio/NZB file (AUDIO_EXTENSIONS) that is not listed
    by a valid licence manifest in its own fixture directory — and any
    tracked `.nzb` at all, listed or not (NZBs get no exception). In
    practice fixture audio lives in R2 and only manifests are
    committed, so any tracked audio file should make a reviewer ask
    why; a small CC0 clip may be committed only when its manifest
    lists it by exact relative path.
  manifest-licence
    A `fixtures/**/manifest.json` (repo-root fixtures tree) whose
    `license` is missing or not in ALLOWED_LICENSES, or that lacks a
    non-empty `license_url` or `source`. ALLOWED_LICENSES starts as
    exactly {"CC0-1.0"}; widening it is a licence decision, not a
    drive-by edit (see the doc).
  manifest-shape
    A fixture manifest that does not parse, has no `tracks` list, or
    has a track without `file`, a positive integer `bytes`, or a
    64-hex `sha256` — the fetch helper verifies downloads against
    these fields, so a manifest without them cannot be trusted.
  audio-key-unlisted (best effort)
    A code file (.rs/.ts/.js/.mjs/.py/.toml/.yml/.yaml) that contains
    an R2-style key `fixtures/cc0/.../<file>.<audio ext>` which no
    valid manifest lists (manifest dir prefix + track `file`). Test
    code must address fixture audio through the manifests, never by
    inventing keys.

Scope notes: runtime-synthesized audio (generated tones/WAV bytes in
test code, e.g. crates/nntune-test-nntp) is original test data, not a
tracked audio file, and is out of scope here; the doc covers it.
Filename *literals* like "track.flac" inside parser tests are not R2
keys and do not match audio-key-unlisted.

Reports print file:line + rule id only. An input that cannot be read
fails the run: an incomplete scan is never a successful check.

Usage:
  python3 .github/scripts/check_fixture_licences.py [--root PATH]
  No --root: the repo containing this script.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ALLOWED_LICENSES = {"CC0-1.0"}

AUDIO_EXTENSIONS = {
    ".aac",
    ".aif",
    ".aiff",
    ".alac",
    ".ape",
    ".flac",
    ".m4a",
    ".mp3",
    ".nzb",
    ".oga",
    ".ogg",
    ".opus",
    ".wav",
    ".wma",
}

CODE_EXTENSIONS = {".rs", ".ts", ".js", ".mjs", ".py", ".toml", ".yml", ".yaml"}

# This script and its tests must name the shapes they ban. Exact paths,
# exempt from the best-effort key scan only (their own manifests, if
# any, would still be checked).
KEY_SCAN_EXCLUSIONS = {
    ".github/scripts/check_fixture_licences.py",
    ".github/scripts/test_check_fixture_licences.py",
}

SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
# Longest extension first: `aif` must not shadow `aiff` (a listed
# `x.aiff` key would otherwise match as `x.aif` and false-positive).
AUDIO_KEY_RE = re.compile(
    r"fixtures/cc0/[A-Za-z0-9._\-/ ()]+\.(?:"
    + "|".join(
        sorted((ext[1:] for ext in AUDIO_EXTENSIONS), key=lambda e: (-len(e), e))
    )
    + r")(?![A-Za-z0-9])",
    re.IGNORECASE,
)
MANIFEST_RE = re.compile(r"^fixtures/(?:.+/)?manifest\.json$")


def _is_positive_int(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def _is_safe_track_file(file) -> bool:
    """A track `file` is a relative sub-path of its manifest directory.

    Absolute paths, backslashes and empty/`.`/`..` segments could make
    the fetch helper write outside its cache, so they are shape errors.
    """
    if not isinstance(file, str) or not file or "\\" in file:
        return False
    if file.startswith("/"):
        return False
    return all(part not in ("", ".", "..") for part in file.split("/"))


def check_manifest(path: str, text: str) -> tuple[list[tuple[int, str]], set[str]]:
    """Validate one fixture manifest. Returns (violations, listed R2 keys).

    Listed keys are `<manifest dir>/<track file>` for every well-formed
    track, returned only when the licence itself is allowed — an
    unlicensed manifest lists nothing.
    """
    violations: list[tuple[int, str]] = []
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return [(1, "manifest-shape")], set()
    if not isinstance(data, dict):
        return [(1, "manifest-shape")], set()

    licence_ok = data.get("license") in ALLOWED_LICENSES
    if not licence_ok:
        violations.append((1, "manifest-licence"))
    for field in ("license_url", "source"):
        value = data.get(field)
        if not isinstance(value, str) or not value.strip():
            violations.append((1, "manifest-licence"))

    tracks = data.get("tracks")
    if not isinstance(tracks, list) or not tracks:
        violations.append((1, "manifest-shape"))
        return violations, set()

    prefix = path.rsplit("/", 1)[0]
    keys: set[str] = set()
    for track in tracks:
        if not isinstance(track, dict):
            violations.append((1, "manifest-shape"))
            continue
        file = track.get("file")
        ok = (
            _is_safe_track_file(file)
            and _is_positive_int(track.get("bytes"))
            and isinstance(track.get("sha256"), str)
            and bool(SHA256_RE.match(track["sha256"]))
        )
        if not ok:
            violations.append((1, "manifest-shape"))
            continue
        keys.add(f"{prefix}/{file}")
    return violations, (keys if licence_ok else set())


def check_tracked_files(
    tracked: list[str], read_text
) -> list[tuple[str, int, str]]:
    """Run every rule. `read_text(path) -> str` may raise OSError."""
    violations: list[tuple[str, int, str]] = []

    listed_keys: set[str] = set()
    for path in tracked:
        if MANIFEST_RE.match(path):
            found, keys = check_manifest(path, read_text(path))
            violations.extend((path, line, rule) for line, rule in found)
            listed_keys |= keys

    for path in tracked:
        suffix = Path(path).suffix.lower()
        # NZB fixtures get no manifest exception (see the doc): a
        # tracked .nzb fails even when a valid manifest lists it.
        if suffix in AUDIO_EXTENSIONS and (
            suffix == ".nzb" or path not in listed_keys
        ):
            violations.append((path, 1, "tracked-audio-unlisted"))

    for path in tracked:
        if Path(path).suffix.lower() not in CODE_EXTENSIONS:
            continue
        if path in KEY_SCAN_EXCLUSIONS:
            continue
        for lineno, line in enumerate(read_text(path).splitlines(), start=1):
            for match in AUDIO_KEY_RE.finditer(line):
                if match.group(0) not in listed_keys:
                    violations.append((path, lineno, "audio-key-unlisted"))

    return sorted(set(violations))


def git_tracked(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        capture_output=True,
        check=True,
    )
    return [
        name
        for name in result.stdout.decode("utf-8", "surrogateescape").split("\0")
        if name
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", help="repo root (default: this script's repo)")
    args = parser.parse_args(argv)
    root = (
        Path(args.root).resolve()
        if args.root
        else Path(__file__).resolve().parents[2]
    )

    def read_text(path: str) -> str:
        # Binary tracked files only reach the extension rule, which does
        # not read them; code/manifest reads decode leniently so a stray
        # non-UTF8 byte cannot hide a key (nor crash the scan).
        return (root / path).read_bytes().decode("utf-8", "replace")

    try:
        tracked = git_tracked(root)
        violations = check_tracked_files(tracked, read_text)
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"check_fixture_licences: scan failed: {exc}", file=sys.stderr)
        return 1

    for path, line, rule in violations:
        print(f"{path}:{line}: {rule}")
    if violations:
        print(
            f"check_fixture_licences: {len(violations)} violation(s); "
            "see docs/fixture-audio-licences.md",
            file=sys.stderr,
        )
        return 1
    print("check_fixture_licences: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
