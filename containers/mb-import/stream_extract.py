#!/usr/bin/env python3
"""Stream the CC0 core dump tar and keep only the tables etl.py reads.

Usage: lbzip2 -dc mbdump.tar.bz2 | ./stream_extract.py <out-dir>

Reads the uncompressed `mbdump.tar` stream on stdin in one pass. Each
member that etl.py needs (`mbdump/<table>`, the keys of
`etl.FIELD_COUNTS`) is written to `<out-dir>/<table>.gz` (gzip level 1,
so the raw dump never lands on disk uncompressed); every other member is
skipped unread. The dump's `TIMESTAMP`, `SCHEMA_SEQUENCE` and
`REPLICATION_SEQUENCE` go to `<out-dir>/dump-meta.json`. A missing
required table fails the run (exit 1) after the stream ends.

Only the CC0 core archive (`mbdump.tar.bz2`) is ever fed here: the
NC-licensed archives (`mbdump-derived`, `mbdump-edit`, ...) and
replication packets are never downloaded by the spike workflow, and no
table outside the curated allowlist is written even if one appeared.
"""

from __future__ import annotations

import gzip
import json
import shutil
import sys
import tarfile
import time
from pathlib import Path

# Must equal etl.FIELD_COUNTS keys (pinned by tests/test_stream_extract.py).
REQUIRED_TABLES = (
    "artist", "artist_type", "area", "iso_3166_1", "artist_credit_name",
    "release", "release_group", "release_group_primary_type", "release_country",
    "release_unknown_country", "recording", "medium", "track", "url",
    "link_type", "link", "link_attribute_type", "link_attribute",
    "l_artist_url", "l_recording_url", "l_recording_recording", "isrc",
)
META_FILES = ("TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE")
CHUNK = 1 << 20


def main() -> int:
    if len(sys.argv) != 2:
        print("Usage: lbzip2 -dc mbdump.tar.bz2 | stream_extract.py <out-dir>", file=sys.stderr)
        return 2
    out_dir = Path(sys.argv[1])
    out_dir.mkdir(parents=True, exist_ok=True)
    wanted = {f"mbdump/{table}": table for table in REQUIRED_TABLES}
    written: dict[str, dict[str, float | int]] = {}
    meta: dict[str, str] = {}
    skipped = 0
    started = time.monotonic()
    with tarfile.open(fileobj=sys.stdin.buffer, mode="r|") as archive:
        for member in archive:
            name = member.name.removeprefix("./")
            if not member.isfile():
                continue
            if name in META_FILES:
                source = archive.extractfile(member)
                meta[name] = source.read(4096).decode("utf-8", "replace").strip() if source else ""
                continue
            table = wanted.get(name)
            if table is None:
                skipped += 1
                continue
            if table in written:
                raise SystemExit(f"stream_extract: {name}: duplicate member")
            source = archive.extractfile(member)
            if source is None:
                raise SystemExit(f"stream_extract: {name}: unreadable member")
            member_started = time.monotonic()
            with gzip.open(out_dir / f"{table}.gz", "wb", compresslevel=1) as target:
                shutil.copyfileobj(source, target, CHUNK)
            written[table] = {
                "bytes": member.size,
                "gz_bytes": (out_dir / f"{table}.gz").stat().st_size,
                "seconds": round(time.monotonic() - member_started, 1),
            }
            print(f"stream_extract: {table} {member.size} bytes", file=sys.stderr, flush=True)
    missing = [table for table in REQUIRED_TABLES if table not in written]
    (out_dir / "dump-meta.json").write_text(json.dumps({
        "meta": meta,
        "tables": written,
        "skipped_members": skipped,
        "seconds": round(time.monotonic() - started, 1),
        "missing": missing,
    }, indent=2, sort_keys=True))
    if missing:
        print(f"stream_extract: missing required tables: {', '.join(missing)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
