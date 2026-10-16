#!/usr/bin/env python3
"""Archive reservation guard: no Worker may bind an archive R2 bucket.

Parses Wrangler TOML manifests repo-wide and fails
if any R2 binding (top-level or a per-environment override) points at an
archive bucket. Wrangler manifests with unsupported formats are rejected
until the guard can parse them. Archive buckets are reserved capacity with
no Worker consumer (see docs/cache-policy.md).

Name-based on purpose: the match is ``archive`` appearing anywhere in a
``bucket_name`` (case-insensitive), so per-env renames of the reserved
buckets are still caught. Commented-out example bindings are ignored
because TOML parsing skips comments — only effective bindings fail.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

SUPPORTED_MANIFEST_SUFFIXES = (".toml", ".toml.example")
SCAN_ROOTS = ("workers", "apps")


class ManifestError(Exception):
    """The repository scan could not safely inspect its Wrangler manifests."""


def _is_manifest_candidate(path: Path) -> bool:
    name = path.name.lower()
    return name.endswith(SUPPORTED_MANIFEST_SUFFIXES)


def _is_unsupported_json_candidate(path: Path) -> bool:
    name = path.name.lower()
    if not name.endswith((".json", ".jsonc")):
        return False
    try:
        text = path.read_text(encoding="utf-8-sig")
    except (OSError, UnicodeError) as exc:
        raise ManifestError(f"cannot inspect JSON config {path}: {exc}") from exc
    cleaned = _strip_jsonc_comments(text)
    try:
        data = json.loads(cleaned)
    except json.JSONDecodeError:
        markers = re.compile(
            r'"(?:r2_buckets|compatibility_date|workers_dev)"\s*:'
        )
        return markers.search(cleaned) is not None
    if not isinstance(data, dict):
        return False
    strong_config_keys = {"compatibility_date", "r2_buckets", "workers_dev"}
    env = data.get("env")
    has_env_r2 = isinstance(env, dict) and any(
        isinstance(scope, dict) and "r2_buckets" in scope
        for scope in env.values()
    )
    has_top_level_r2 = "r2_buckets" in data
    has_wrangler_markers = bool(strong_config_keys.intersection(data))
    return (name.startswith("wrangler") and has_wrangler_markers) or (
        has_top_level_r2 or has_env_r2
    )


def _has_toml_r2_marker(text: str) -> bool:
    """Detect an R2 key/table in malformed TOML, excluding comments and values."""
    key = re.compile(
        r'''(?:^|\.)(?:"r2_buckets"|'r2_buckets'|r2_buckets)(?=\s*(?:\]|$))'''
    )
    assignment = re.compile(r'''^\s*(?:"r2_buckets"|'r2_buckets'|r2_buckets)\s*=''')
    for line in text.splitlines():
        quote = ""
        escaped = False
        visible: list[str] = []
        for char in line:
            if quote:
                visible.append(char)
                if escaped:
                    escaped = False
                elif quote == '"' and char == "\\":
                    escaped = True
                elif char == quote:
                    quote = ""
            elif char in ('"', "'"):
                quote = char
                visible.append(char)
            elif char == "#":
                break
            else:
                visible.append(char)
        statement = "".join(visible).strip()
        if assignment.match(statement):
            return True
        if statement.startswith("[") and key.search(statement[1:]):
            return True
    return False


def _strip_jsonc_comments(text: str) -> str:
    """Remove JSONC comments without altering comment-like string contents."""
    result: list[str] = []
    in_string = False
    escaped = False
    index = 0
    while index < len(text):
        char = text[index]
        following = text[index + 1] if index + 1 < len(text) else ""
        if in_string:
            result.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            index += 1
        elif char == '"':
            in_string = True
            result.append(char)
            index += 1
        elif char == "/" and following == "/":
            while index < len(text) and text[index] not in "\r\n":
                index += 1
        elif char == "/" and following == "*":
            index += 2
            while index + 1 < len(text) and text[index : index + 2] != "*/":
                index += 1
            index = min(len(text), index + 2)
        else:
            result.append(char)
            index += 1
    return "".join(result)


def archive_bindings(repo_root: Path) -> list[tuple[Path, str]]:
    """Return ``(manifest, bucket_name)`` pairs targeting archive buckets."""
    hits: list[tuple[Path, str]] = []
    for root in SCAN_ROOTS:
        base = repo_root / root
        if not base.is_dir():
            raise ManifestError(f"required scan root is missing: {base}")
    # Scan tracked files repo-wide so dependency and build trees in a working
    # copy cannot introduce false positives or hide effective manifests.
    try:
        tracked = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=repo_root,
            check=True,
            capture_output=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        raise ManifestError(f"cannot list tracked repository files: {exc}") from exc
    try:
        relative_paths = [
            Path(os.fsdecode(entry)) for entry in tracked.split(b"\0") if entry
        ]
    except (OSError, ValueError) as exc:
        raise ManifestError(f"cannot decode tracked repository paths: {exc}") from exc
    candidates = sorted(
        repo_root / relative_path
        for relative_path in relative_paths
        if (
            _is_manifest_candidate(relative_path)
            or _is_unsupported_json_candidate(repo_root / relative_path)
        )
        and (repo_root / relative_path).is_file()
    )
    manifests = [
        path
        for path in candidates
        if path.name.lower().endswith(SUPPORTED_MANIFEST_SUFFIXES)
    ]
    unsupported = [path for path in candidates if path not in manifests]
    if unsupported:
        paths = ", ".join(str(path) for path in unsupported)
        raise ManifestError(
            f"unsupported Wrangler manifest(s): {paths}; "
            "use a TOML manifest until this format is supported"
        )
    scanned = 0
    for manifest in manifests:
        buckets = _bucket_names(manifest)
        if buckets is None:
            continue
        scanned += 1
        hits.extend(
            (manifest, bucket) for bucket in buckets if "archive" in bucket.lower()
        )
    if not scanned:
        raise ManifestError("no Wrangler TOML manifests were scanned")
    return hits


def _bucket_names(manifest: Path) -> list[str] | None:
    try:
        text = manifest.read_text(encoding="utf-8-sig")
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        if not manifest.name.lower().startswith("wrangler"):
            try:
                text = manifest.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as read_exc:
                raise ManifestError(
                    f"cannot inspect TOML config {manifest}: {read_exc}"
                ) from read_exc
            if not _has_toml_r2_marker(text):
                return None
        raise ManifestError(f"cannot parse TOML config {manifest}: {exc}") from exc
    except (OSError, UnicodeError) as exc:
        raise ManifestError(f"cannot read TOML config {manifest}: {exc}") from exc
    found: list[str] = []

    def bindings_from(scope: dict[str, object]) -> list[object]:
        bindings = scope.get("r2_buckets", [])
        if not isinstance(bindings, list):
            raise ManifestError(f"invalid r2_buckets table in {manifest}")
        return bindings

    if not isinstance(data, dict):
        raise ManifestError(f"invalid TOML document in {manifest}")
    envs = data.get("env", {})
    has_env_r2 = isinstance(envs, dict) and any(
        isinstance(env, dict) and "r2_buckets" in env for env in envs.values()
    )
    if (
        "r2_buckets" not in data
        and not has_env_r2
        and not manifest.name.lower().startswith("wrangler")
    ):
        return None
    r2_bindings = bindings_from(data)
    if not isinstance(envs, dict):
        raise ManifestError(f"invalid env table in {manifest}")
    env_scopes = envs.values()
    for env in env_scopes:
        if not isinstance(env, dict):
            raise ManifestError(f"invalid env entry in {manifest}")
        r2_bindings.extend(bindings_from(env))

    for binding in r2_bindings:
        if not isinstance(binding, dict):
            raise ManifestError(f"invalid r2_buckets entry in {manifest}")
        for key in ("bucket_name", "preview_bucket_name"):
            value = binding.get(key)
            if isinstance(value, str):
                found.append(value)
    return found


def main(repo_root: Path | None = None) -> int:
    if repo_root is None:
        repo_root = Path(__file__).resolve().parent.parent.parent
    try:
        hits = archive_bindings(repo_root)
    except ManifestError as exc:
        print(f"archive guard: FAIL: {exc}", flush=True)
        return 1
    if hits:
        for manifest, bucket in hits:
            print(
                f"{manifest}: FAIL: R2 binding targets reserved archive "
                f"bucket {bucket!r} (docs/cache-policy.md)",
                flush=True,
            )
        return 1
    print("archive guard: no Worker R2 binding targets an archive bucket")
    return 0


if __name__ == "__main__":
    sys.exit(main())
