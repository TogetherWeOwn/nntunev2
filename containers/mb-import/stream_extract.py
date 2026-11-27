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
import sys
import tarfile
import time
from contextlib import ExitStack
from pathlib import Path

# Must equal etl.FIELD_COUNTS keys (pinned by tests/test_stream_extract.py).
REQUIRED_TABLES = (
    "artist", "artist_alias", "artist_type", "area", "iso_3166_1", "artist_credit_name",
    "release", "release_alias", "release_group", "release_group_alias", "release_group_primary_type",
    "release_country", "release_unknown_country", "recording", "recording_alias", "medium", "track", "url",
    "link_type", "link", "link_attribute_type", "link_attribute",
    "l_artist_recording", "l_artist_release", "l_artist_url", "l_artist_work",
    "l_recording_url", "l_recording_recording", "l_recording_work",
    "isrc", "work", "work_alias", "work_type", "label", "label_alias", "label_type", "release_label",
)
META_FILES = ("TIMESTAMP", "SCHEMA_SEQUENCE", "REPLICATION_SEQUENCE")
CHUNK = 1 << 20
BLOCK = tarfile.BLOCKSIZE
MAX_ARCHIVE_BYTES = 128 << 30
MAX_MEMBERS = 4096
MAX_META_BYTES = 4096


def read_exact(source, size):
    data = source.read(size)
    if len(data) != size:
        raise ValueError("truncated archive")
    return data


def members(source):
    consumed = 0
    for index in range(MAX_MEMBERS + 1):
        header = read_exact(source, BLOCK)
        if not any(header):
            trailer = read_exact(source, BLOCK) + source.read(CHUNK + 1)
            if len(trailer) > BLOCK + CHUNK or any(trailer) or len(trailer) % BLOCK:
                raise ValueError("invalid archive trailer")
            return
        if index == MAX_MEMBERS:
            raise ValueError("archive member count limit exceeded")
        try:
            member = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
        except (tarfile.HeaderError, UnicodeError) as exc:
            raise ValueError("invalid header") from exc
        # Core dumps use short regular names and directories. Reject extension
        # headers and links before any member data is read or expanded.
        if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
            raise ValueError("unsupported member type")
        if member.size < 0 or (member.isdir() and member.size):
            raise ValueError("invalid member size")
        padding = -member.size % BLOCK
        consumed += BLOCK + member.size + padding
        if consumed > MAX_ARCHIVE_BYTES:
            raise ValueError("archive size limit exceeded")
        yield member
        if any(read_exact(source, padding)):
            raise ValueError("invalid member padding")


def copy_member(source, size, target=None):
    while size:
        data = read_exact(source, min(size, CHUNK))
        if target is not None:
            target.write(data)
        size -= len(data)


def extract(source, out_dir, rollback) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    report = out_dir / "dump-meta.json"
    if report.exists() or report.is_symlink():
        raise ValueError("output already contains dump metadata")
    wanted = {f"mbdump/{table}": table for table in REQUIRED_TABLES}
    written: dict[str, dict[str, float | int]] = {}
    meta: dict[str, str] = {}
    skipped = 0
    started = time.monotonic()
    for member in members(source):
        name = member.name.removeprefix("./")
        if member.isdir() and name == ".":
            continue
        if not name.isprintable() or "\\" in name or {"", ".", ".."}.intersection(name.split("/")):
            raise ValueError("unsafe path in archive")
        if member.isdir():
            if name in META_FILES or name in wanted:
                raise ValueError("expected a regular file at a required member path")
            continue
        if name in META_FILES:
            if name in meta:
                raise ValueError(f"{name}: duplicate member")
            if member.size > MAX_META_BYTES:
                raise ValueError(f"{name}: metadata size limit exceeded")
            raw = read_exact(source, member.size).strip()
            if not raw or not raw.isascii() or not raw.decode("ascii").isprintable():
                raise ValueError(f"{name}: invalid metadata")
            meta[name] = raw.decode("ascii")
            continue
        table = wanted.get(name)
        if table is None:
            skipped += 1
            copy_member(source, member.size)
            continue
        if table in written:
            raise ValueError(f"{name}: duplicate member")
        member_started = time.monotonic()
        path = out_dir / f"{table}.gz"
        with path.open("xb") as packed:
            rollback.callback(path.unlink)
            with gzip.GzipFile(fileobj=packed, mode="wb", compresslevel=1) as target:
                copy_member(source, member.size, target)
        written[table] = {
            "bytes": member.size,
            "gz_bytes": path.stat().st_size,
            "seconds": round(time.monotonic() - member_started, 1),
        }
        print(f"stream_extract: {table} {member.size} bytes", file=sys.stderr, flush=True)
    missing = [table for table in REQUIRED_TABLES if table not in written]
    with report.open("x", encoding="utf-8") as target:
        json.dump({
            "meta": meta,
            "tables": written,
            "skipped_members": skipped,
            "seconds": round(time.monotonic() - started, 1),
            "missing": missing,
        }, target, indent=2, sort_keys=True)
    if missing:
        print(f"stream_extract: missing required tables: {', '.join(missing)}", file=sys.stderr)
        return 1
    rollback.pop_all()
    return 0


def main() -> int:
    if len(sys.argv) != 2:
        print("Usage: lbzip2 -dc mbdump.tar.bz2 | stream_extract.py <out-dir>", file=sys.stderr)
        return 2
    try:
        with ExitStack() as rollback:
            return extract(sys.stdin.buffer, Path(sys.argv[1]), rollback)
    except (OSError, ValueError) as exc:
        print(f"stream_extract: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
