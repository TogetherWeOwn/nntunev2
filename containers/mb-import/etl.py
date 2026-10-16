#!/usr/bin/env python3
"""Project official MusicBrainz COPY files into curated, disk-backed TSVs."""

from __future__ import annotations

import filecmp
import hashlib
import os
import sqlite3
import sys
import tempfile
import uuid
from datetime import datetime
from pathlib import Path

# These 1-based positions and field counts follow the official CreateTables.sql:
# https://github.com/metabrainz/musicbrainz-server/blob/master/admin/sql/CreateTables.sql
# COPY files are headerless, tab-delimited, and use \\N for SQL NULL. The input
# extract must use the schema revision matching these pinned declarations.
ARTIST_ID, ARTIST_GID, ARTIST_NAME, ARTIST_SORT_NAME = 1, 2, 3, 4
ARTIST_TYPE_ID, ARTIST_AREA_ID = 11, 12
ARTIST_TYPE_LOOKUP_ID, ARTIST_TYPE_NAME = 1, 2
AREA_ID = 1
ISO_AREA_ID, ISO_CODE = 1, 2
CREDIT_ID, CREDIT_POSITION, CREDIT_ARTIST_ID = 1, 2, 3
RELEASE_ID, RELEASE_GID, RELEASE_NAME = 1, 2, 3
RELEASE_CREDIT_ID, RELEASE_GROUP_ID = 4, 5
GROUP_ID, GROUP_TYPE_ID = 1, 5
PRIMARY_TYPE_ID, PRIMARY_TYPE_NAME = 1, 2
RELEASE_COUNTRY_RELEASE_ID, RELEASE_COUNTRY_YEAR = 1, 3
RECORDING_ID, RECORDING_GID, RECORDING_NAME, RECORDING_LENGTH = 1, 2, 3, 5
MEDIUM_ID, MEDIUM_RELEASE_ID, MEDIUM_POSITION = 1, 2, 3
TRACK_ID, TRACK_RECORDING_ID, TRACK_MEDIUM_ID, TRACK_POSITION = 1, 3, 4, 5

FIELD_COUNTS = {
    "artist": 19,
    "artist_type": 6,
    "area": 14,
    "iso_3166_1": 2,
    "artist_credit_name": 5,
    "release": 14,
    "release_group": 8,
    "release_group_primary_type": 6,
    "release_country": 5,
    "recording": 9,
    "medium": 9,
    "track": 12,
}
NULL = r"\N"
BATCH_SIZE = 5000


def get(row: list[str], position: int) -> str:
    return row[position - 1]


def value(field: str) -> str | None:
    return None if field == NULL else field


def required_int(field: str, context: str) -> int:
    parsed = value(field)
    if parsed is None:
        raise ValueError(f"{context}: required integer is NULL")
    try:
        return int(parsed)
    except ValueError as exc:
        raise ValueError(f"{context}: invalid integer {parsed!r}") from exc


def rows(path: Path, expected: int):
    if not path.is_file():
        raise ValueError(f"missing input file: {path}")
    with path.open("r", encoding="utf-8", newline="") as source:
        for line_number, line in enumerate(source, 1):
            if line.endswith("\n"):
                line = line[:-1]
            if line.endswith("\r"):
                line = line[:-1]
            fields = line.split("\t")
            if len(fields) != expected:
                raise ValueError(
                    f"{path}: row {line_number}: expected {expected} fields, got {len(fields)}"
                )
            yield fields


def load(db: sqlite3.Connection, path: Path, expected: int, select_positions: tuple[int, ...], table: str, columns: str, convert=None) -> int:
    count = 0
    batch = []
    statement = f"INSERT INTO {table} ({columns}) VALUES ({','.join('?' for _ in select_positions)})"
    for row in rows(path, expected):
        selected = tuple(None if get(row, position) == NULL else get(row, position) for position in select_positions)
        if convert is not None:
            selected = convert(selected, count + 1)
        batch.append(selected)
        count += 1
        if len(batch) >= BATCH_SIZE:
            db.executemany(statement, batch)
            batch.clear()
    if batch:
        db.executemany(statement, batch)
    return count


def as_ints(context: str, integer_fields: tuple[int, ...], nullable_fields: tuple[int, ...] = ()):
    def convert(values, line_number):
        converted = list(values)
        for index in integer_fields:
            item = converted[index]
            if item is None and index not in nullable_fields:
                raise ValueError(f"{context} row {line_number}: required integer is NULL")
            if item is not None:
                converted[index] = required_int(item, f"{context} row {line_number}")
        return tuple(converted)
    return convert


def validate_timestamp(timestamp: str) -> str:
    try:
        parsed = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("DUMP_TS must be an ISO-8601 timestamp with Z or an explicit UTC offset") from exc
    if parsed.tzinfo is None:
        raise ValueError("DUMP_TS must be an ISO-8601 timestamp with Z or an explicit UTC offset")
    return timestamp


def insert_select(db: sqlite3.Connection, query: str, output_path: Path, updated_at: str, row_transform=None) -> int:
    count = 0
    with output_path.open("w", encoding="utf-8", newline="") as output:
        for row in db.execute(query):
            fields = row_transform(row) if row_transform else row
            output.write("\t".join(NULL if item is None else str(item) for item in (*fields, updated_at)) + "\n")
            count += 1
    return count


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] in ("-h", "--help"):
        print("Usage: DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>", file=sys.stderr)
        return 0 if len(sys.argv) == 2 else 2

    dump_dir = Path(sys.argv[1])
    try:
        updated_at = validate_timestamp(os.environ.get("DUMP_TS", ""))
        if not dump_dir.is_dir():
            raise ValueError(f"input directory does not exist: {dump_dir}")
        curated_link = dump_dir / "curated"
        if curated_link.exists() and not curated_link.is_symlink():
            raise ValueError(f"refusing to replace non-symlink output path: {curated_link}")

        with tempfile.TemporaryDirectory(prefix="mb-etl-", dir=dump_dir) as work:
            work_dir = Path(work)
            db_path = work_dir / "projection.sqlite3"
            output_dir = work_dir / "curated"
            output_dir.mkdir()
            db = sqlite3.connect(db_path)
            try:
                db.executescript("""
                    CREATE TABLE artist (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, sort_name TEXT NOT NULL, type_id INTEGER, area_id INTEGER);
                    CREATE TABLE artist_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE area (id INTEGER PRIMARY KEY);
                    CREATE TABLE iso_3166_1 (area INTEGER PRIMARY KEY, code TEXT);
                    CREATE TABLE artist_credit_name (credit INTEGER NOT NULL, position INTEGER NOT NULL, artist INTEGER NOT NULL);
                    CREATE INDEX artist_credit_name_order ON artist_credit_name(credit, position);
                    CREATE TABLE release_group_primary_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE release_group (id INTEGER PRIMARY KEY, type_id INTEGER);
                    CREATE TABLE release_country (release INTEGER NOT NULL, year INTEGER);
                    CREATE INDEX release_country_id ON release_country(release);
                    CREATE TABLE release (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, credit INTEGER NOT NULL, group_id INTEGER NOT NULL);
                    CREATE TABLE recording (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, length INTEGER);
                    CREATE TABLE medium (id INTEGER PRIMARY KEY, release INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE TABLE track (id INTEGER PRIMARY KEY, recording INTEGER NOT NULL, medium INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE INDEX track_recording_id ON track(recording);
                """)

                counts = {}
                counts["artist_type"] = load(db, dump_dir / "artist_type", FIELD_COUNTS["artist_type"], (ARTIST_TYPE_LOOKUP_ID, ARTIST_TYPE_NAME), "artist_type", "id,name", as_ints("artist_type.id", (0,)))
                counts["area"] = load(db, dump_dir / "area", FIELD_COUNTS["area"], (AREA_ID,), "area", "id", as_ints("area.id", (0,)))
                counts["iso_3166_1"] = load(db, dump_dir / "iso_3166_1", FIELD_COUNTS["iso_3166_1"], (ISO_AREA_ID, ISO_CODE), "iso_3166_1", "area,code", as_ints("iso_3166_1.area", (0,)))
                counts["artist"] = load(db, dump_dir / "artist", FIELD_COUNTS["artist"], (ARTIST_ID, ARTIST_GID, ARTIST_NAME, ARTIST_SORT_NAME, ARTIST_TYPE_ID, ARTIST_AREA_ID), "artist", "id,gid,name,sort_name,type_id,area_id", as_ints("artist.id", (0, 4, 5), (4, 5)))
                counts["artist_credit_name"] = load(db, dump_dir / "artist_credit_name", FIELD_COUNTS["artist_credit_name"], (CREDIT_ID, CREDIT_POSITION, CREDIT_ARTIST_ID), "artist_credit_name", "credit,position,artist", as_ints("artist_credit_name", (0, 1, 2)))
                counts["release_group_primary_type"] = load(db, dump_dir / "release_group_primary_type", FIELD_COUNTS["release_group_primary_type"], (PRIMARY_TYPE_ID, PRIMARY_TYPE_NAME), "release_group_primary_type", "id,name", as_ints("release_group_primary_type.id", (0,)))
                counts["release_group"] = load(db, dump_dir / "release_group", FIELD_COUNTS["release_group"], (GROUP_ID, GROUP_TYPE_ID), "release_group", "id,type_id", as_ints("release_group.id", (0, 1), (1,)))
                counts["release_country"] = load(db, dump_dir / "release_country", FIELD_COUNTS["release_country"], (RELEASE_COUNTRY_RELEASE_ID, RELEASE_COUNTRY_YEAR), "release_country", "release,year", as_ints("release_country", (0, 1), (1,)))
                counts["release"] = load(db, dump_dir / "release", FIELD_COUNTS["release"], (RELEASE_ID, RELEASE_GID, RELEASE_NAME, RELEASE_CREDIT_ID, RELEASE_GROUP_ID), "release", "id,gid,name,credit,group_id", as_ints("release.id", (0, 3, 4)))
                counts["recording"] = load(db, dump_dir / "recording", FIELD_COUNTS["recording"], (RECORDING_ID, RECORDING_GID, RECORDING_NAME, RECORDING_LENGTH), "recording", "id,gid,name,length", as_ints("recording.id", (0, 3), (3,)))
                counts["medium"] = load(db, dump_dir / "medium", FIELD_COUNTS["medium"], (MEDIUM_ID, MEDIUM_RELEASE_ID, MEDIUM_POSITION), "medium", "id,release,position", as_ints("medium.id", (0, 1, 2)))
                counts["track"] = load(db, dump_dir / "track", FIELD_COUNTS["track"], (TRACK_ID, TRACK_RECORDING_ID, TRACK_MEDIUM_ID, TRACK_POSITION), "track", "id,recording,medium,position", as_ints("track.id", (0, 1, 2, 3)))
                db.commit()

                if counts["recording"] and counts["track"] == 0:
                    raise ValueError("recording input is non-empty but track input is empty")
                bad_type = db.execute("SELECT a.id FROM artist a LEFT JOIN artist_type t ON t.id=a.type_id WHERE a.type_id IS NOT NULL AND t.id IS NULL LIMIT 1").fetchone()
                if bad_type:
                    raise ValueError(f"artist {bad_type[0]} references missing artist_type {db.execute('SELECT type_id FROM artist WHERE id=?', bad_type).fetchone()[0]}")
                bad_artist_area = db.execute("SELECT x.id,x.area_id FROM artist x LEFT JOIN area a ON a.id=x.area_id WHERE x.area_id IS NOT NULL AND a.id IS NULL LIMIT 1").fetchone()
                if bad_artist_area:
                    raise ValueError(f"artist {bad_artist_area[0]} references missing area {bad_artist_area[1]}")
                bad_country_area = db.execute("SELECT i.area FROM iso_3166_1 i LEFT JOIN area a ON a.id=i.area WHERE a.id IS NULL LIMIT 1").fetchone()
                if bad_country_area:
                    raise ValueError(f"iso_3166_1 references missing area {bad_country_area[0]}")
                bad_credit_artist = db.execute("SELECT c.credit,c.artist FROM artist_credit_name c LEFT JOIN artist a ON a.id=c.artist WHERE a.id IS NULL LIMIT 1").fetchone()
                if bad_credit_artist:
                    raise ValueError(f"artist credit {bad_credit_artist[0]} references missing artist {bad_credit_artist[1]}")
                bad_group_type = db.execute("SELECT g.id,g.type_id FROM release_group g LEFT JOIN release_group_primary_type t ON t.id=g.type_id WHERE g.type_id IS NOT NULL AND t.id IS NULL LIMIT 1").fetchone()
                if bad_group_type:
                    raise ValueError(f"release group {bad_group_type[0]} references missing primary type {bad_group_type[1]}")
                bad_release = db.execute("SELECT r.id FROM release r LEFT JOIN release_group g ON g.id=r.group_id WHERE g.id IS NULL LIMIT 1").fetchone()
                if bad_release:
                    raise ValueError(f"release {bad_release[0]} references a missing release group")
                bad_credit = db.execute("SELECT r.id FROM release r LEFT JOIN artist_credit_name c ON c.credit=r.credit WHERE c.credit IS NULL LIMIT 1").fetchone()
                if bad_credit:
                    raise ValueError(f"release {bad_credit[0]} has no artist-credit-name rows")
                bad_year = db.execute("SELECT c.release FROM release_country c LEFT JOIN release r ON r.id=c.release WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_year:
                    raise ValueError(f"release_country references missing release {bad_year[0]}")
                bad_track = db.execute("SELECT t.id FROM track t LEFT JOIN medium m ON m.id=t.medium LEFT JOIN recording r ON r.id=t.recording WHERE m.id IS NULL OR r.id IS NULL LIMIT 1").fetchone()
                if bad_track:
                    raise ValueError(f"track {bad_track[0]} references a missing medium or recording")
                bad_medium_release = db.execute("SELECT m.id FROM medium m LEFT JOIN release r ON r.id=m.release WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_medium_release:
                    raise ValueError(f"medium {bad_medium_release[0]} references a missing release")

                artist_query = """
                    SELECT a.gid,a.name,a.sort_name,t.name,i.code
                    FROM artist a
                    LEFT JOIN artist_type t ON t.id=a.type_id
                    LEFT JOIN iso_3166_1 i ON i.area=a.area_id
                    LEFT JOIN area ar ON ar.id=i.area
                    ORDER BY a.id
                """
                artist_count = insert_select(db, artist_query, output_dir / "mbdump-artist.tsv", updated_at)
                release_query = """
                    SELECT r.gid,a.gid,r.name,pt.name,
                           (SELECT MIN(rc.year) FROM release_country rc WHERE rc.release=r.id AND rc.year IS NOT NULL)
                    FROM release r
                    JOIN release_group g ON g.id=r.group_id
                    LEFT JOIN release_group_primary_type pt ON pt.id=g.type_id
                    LEFT JOIN artist_credit_name acn ON acn.credit=r.credit
                    LEFT JOIN artist a ON a.id=acn.artist
                    WHERE acn.position=(SELECT MIN(acn2.position) FROM artist_credit_name acn2 WHERE acn2.credit=r.credit)
                    ORDER BY r.id
                """
                release_count = insert_select(
                    db,
                    release_query,
                    output_dir / "mbdump-release.tsv",
                    updated_at,
                    lambda row: (*row, f"https://coverartarchive.org/release/{row[0]}/front-250"),
                )
                recording_query = """
                    WITH ranked_track AS (
                        SELECT t.recording,t.position,m.release,
                               ROW_NUMBER() OVER (
                                   PARTITION BY t.recording
                                   ORDER BY m.release,m.position,t.position,t.id
                               ) AS rank
                        FROM track t JOIN medium m ON m.id=t.medium
                    )
                    SELECT rec.gid,rel.gid,rec.name,rt.position,rec.length
                    FROM recording rec
                    LEFT JOIN ranked_track rt ON rt.recording=rec.id AND rt.rank=1
                    LEFT JOIN release rel ON rel.id=rt.release
                    ORDER BY rec.id
                """
                missing_release = db.execute("""
                    WITH ranked_track AS (
                        SELECT t.recording,m.release,
                               ROW_NUMBER() OVER (PARTITION BY t.recording ORDER BY m.release,m.position,t.position,t.id) AS rank
                        FROM track t JOIN medium m ON m.id=t.medium
                    )
                    SELECT rt.recording,rt.release FROM ranked_track rt
                    LEFT JOIN release r ON r.id=rt.release
                    WHERE rt.rank=1 AND r.id IS NULL LIMIT 1
                """).fetchone()
                if missing_release:
                    raise ValueError(f"recording {missing_release[0]} resolves to missing release {missing_release[1]}")
                recording_count = insert_select(db, recording_query, output_dir / "mbdump-recording.tsv", updated_at)

                for table, input_count, output_count in (
                    ("artist", counts["artist"], artist_count),
                    ("release", counts["release"], release_count),
                    ("recording", counts["recording"], recording_count),
                ):
                    if input_count and output_count == 0:
                        raise ValueError(f"{table} input is non-empty but curated output is empty")
                    if input_count != output_count:
                        raise ValueError(f"{table} input/output row count mismatch: {input_count} != {output_count}")
            finally:
                db.close()

            # Publish a complete snapshot by swapping one symlink; immutable
            # snapshots avoid a killed ETL leaving a mixed set of TSV versions.
            # Reuse the snapshot for the same dump timestamp only if every output
            # is byte-identical; changed inputs require a new DUMP_TS.
            timestamp_key = hashlib.sha256(updated_at.encode("utf-8")).hexdigest()[:20]
            snapshot_name = f".curated-{timestamp_key}"
            snapshot = dump_dir / snapshot_name
            if snapshot.exists():
                for table in ("artist", "release", "recording"):
                    name = f"mbdump-{table}.tsv"
                    if not filecmp.cmp(output_dir / name, snapshot / name, shallow=False):
                        raise ValueError("this DUMP_TS already has different curated output; use a new timestamp")
            else:
                os.replace(output_dir, snapshot)
            link_tmp = dump_dir / f".curated-link-{uuid.uuid4().hex}"
            os.symlink(snapshot_name, link_tmp)
            os.replace(link_tmp, curated_link)

        print(f"etl: published {snapshot_name} via curated (artist={artist_count}, release={release_count}, recording={recording_count})")
        return 0
    except (OSError, sqlite3.Error, ValueError) as exc:
        print(f"etl: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
