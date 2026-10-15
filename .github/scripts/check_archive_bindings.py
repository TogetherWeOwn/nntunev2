#!/usr/bin/env python3
"""Archive reservation guard: no Worker may bind an archive R2 bucket.

Parses every ``wrangler.toml`` and ``wrangler.toml.example`` under
``workers/`` and ``apps/`` and fails if any R2 binding (top-level or a
per-environment override) points at an archive bucket. Archive buckets
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
SCAN_ROOTS = ("workers", "apps")


def archive_bindings(repo_root: Path) -> list[tuple[Path, str]]:
    """Return ``(manifest, bucket_name)`` pairs targeting archive buckets."""
    hits: list[tuple[Path, str]] = []
    for root in SCAN_ROOTS:
        base = repo_root / root
        if not base.is_dir():
            continue
        for name in MANIFEST_NAMES:
            for manifest in sorted(base.rglob(name)):
                hits.extend(
                    (manifest, bucket)
                    for bucket in _bucket_names(manifest)
                    if "archive" in bucket.lower()
                )
    return hits


def _bucket_names(manifest: Path) -> list[str]:
    try:
        with manifest.open("rb") as fh:
            data = tomllib.load(fh)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        print(f"{manifest}: WARNING: unparsable manifest ({exc})", flush=True)
        return []
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


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent.parent
    hits = archive_bindings(repo_root)
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
