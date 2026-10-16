#!/usr/bin/env python3
"""Archive reservation guard: no Worker may bind an archive R2 bucket.

Parses every ``wrangler.toml`` and ``wrangler.toml.example`` under
``workers/`` and ``apps/`` and fails if any R2 binding (top-level or a
per-environment override) points at an archive bucket. Wrangler JSON and
JSONC manifests are rejected until the guard can parse them. Archive buckets
are reserved capacity with no Worker consumer (see docs/cache-policy.md).

Name-based on purpose: the match is ``archive`` appearing anywhere in a
``bucket_name`` (case-insensitive), so per-env renames of the reserved
buckets are still caught. Commented-out example bindings are ignored
because TOML parsing skips comments — only effective bindings fail.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

MANIFEST_NAMES = ("wrangler.toml", "wrangler.toml.example")
UNSUPPORTED_MANIFEST_NAMES = ("wrangler.json", "wrangler.jsonc")
SCAN_ROOTS = ("workers", "apps")


class ManifestError(Exception):
    """The repository scan could not safely inspect its Wrangler manifests."""


def archive_bindings(repo_root: Path) -> list[tuple[Path, str]]:
    """Return ``(manifest, bucket_name)`` pairs targeting archive buckets."""
    hits: list[tuple[Path, str]] = []
    manifest_count = 0
    for root in SCAN_ROOTS:
        base = repo_root / root
        if not base.is_dir():
            raise ManifestError(f"required scan root is missing: {base}")
        unsupported = sorted(
            manifest
            for name in UNSUPPORTED_MANIFEST_NAMES
            for manifest in base.rglob(name)
        )
        if unsupported:
            paths = ", ".join(str(path) for path in unsupported)
            raise ManifestError(
                f"unsupported Wrangler JSON manifest(s): {paths}; "
                "use TOML until JSON/JSONC scanning is supported"
            )
        manifests = sorted(
            manifest for name in MANIFEST_NAMES for manifest in base.rglob(name)
        )
        manifest_count += len(manifests)
        for manifest in manifests:
            hits.extend(
                (manifest, bucket)
                for bucket in _bucket_names(manifest)
                if "archive" in bucket.lower()
            )
    if not manifest_count:
        raise ManifestError("no Wrangler TOML manifests were scanned")
    return hits


def _bucket_names(manifest: Path) -> list[str]:
    try:
        with manifest.open("rb") as fh:
            data = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise ManifestError(
            f"cannot parse Wrangler manifest {manifest}: {exc}"
        ) from exc
    found: list[str] = []

    def walk(node: object) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                if key == "bucket_name" and isinstance(value, str):
                    found.append(value)
                else:
                    walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(data)
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
