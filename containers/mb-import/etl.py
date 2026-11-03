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
from datetime import datetime, timezone
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
GROUP_ID, GROUP_GID, GROUP_NAME, GROUP_CREDIT_ID, GROUP_TYPE_ID = 1, 2, 3, 4, 5
PRIMARY_TYPE_ID, PRIMARY_TYPE_NAME = 1, 2
RELEASE_COUNTRY_RELEASE_ID, RELEASE_COUNTRY_YEAR = 1, 3
RELEASE_UNKNOWN_COUNTRY_RELEASE_ID, RELEASE_UNKNOWN_COUNTRY_YEAR = 1, 2
RECORDING_ID, RECORDING_GID, RECORDING_NAME, RECORDING_LENGTH = 1, 2, 3, 5
MEDIUM_ID, MEDIUM_RELEASE_ID, MEDIUM_POSITION = 1, 2, 3
TRACK_ID, TRACK_RECORDING_ID, TRACK_MEDIUM_ID, TRACK_POSITION = 1, 3, 4, 5
URL_ID, URL_GID, URL_URL = 1, 2, 3
LINK_TYPE_ID, LINK_TYPE_GID, LINK_TYPE_NAME = 1, 4, 7
LINK_ID, LINK_LINK_TYPE_ID = 1, 2
ATTR_TYPE_ID, ATTR_TYPE_GID = 1, 5
LINK_ATTR_LINK_ID, LINK_ATTR_ATTR_TYPE_ID = 1, 2
L_ARTIST_URL_LINK_ID, L_ARTIST_URL_ARTIST_ID, L_ARTIST_URL_URL_ID = 2, 3, 4
L_RECORDING_URL_LINK_ID, L_RECORDING_URL_RECORDING_ID, L_RECORDING_URL_URL_ID = 2, 3, 4
L_RR_LINK_ID, L_RR_E0_ID, L_RR_E1_ID = 2, 3, 4
ISRC_RECORDING_ID, ISRC_CODE = 2, 3

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
    "release_unknown_country": 4,
    "recording": 9,
    "medium": 9,
    "track": 12,
    "url": 5,
    "link_type": 16,
    "link": 11,
    "link_attribute_type": 8,
    "link_attribute": 3,
    "l_artist_url": 9,
    "l_recording_url": 9,
    "l_recording_recording": 9,
    "isrc": 5,
}

# Allowlisted relationship link-type GIDs (pinned from the MusicBrainz
# relationship-type documentation; the ETL resolves them to table ids and
# refuses a dump where any of them is absent). Only links of these types
# reach the curated relationship files; everything else in the link tables
# (including the NC-licensed Live Data Feed's supplementary associations,
# which live in tag/genre/rating/annotation tables the ETL never reads)
# is ignored.
ARTIST_URL_YOUTUBE_GID = "6a540e5b-58c6-4192-b6ba-dbc71ec8fcf0"
ARTIST_URL_YOUTUBE_MUSIC_GID = "631712a0-7525-42ba-b7a3-605aa7238c4"
ARTIST_URL_FREE_STREAMING_GID = "769085a1-c2f7-4c24-a532-2375a77693bd"
ARTIST_URL_STREAMING_GID = "63cc5d1f-f096-4c94-a43f-ecb32ea94161"
RECORDING_URL_FREE_STREAMING_GID = "7e41ef12-a124-4324-afdb-fdbae687a89c"
MUSIC_VIDEO_LINK_GID = "ce3de655-7451-44d1-9224-87eb948c205d"
# The "video" link attribute: a free-streaming recording URL carrying it
# is the music-video -> stream join edge (ADR 0012 section 2).
VIDEO_ATTR_GID = "112054d5-e706-4dd8-99ea-09aabee36cd6"
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


def load_filtered(db: sqlite3.Connection, path: Path, expected: int, select_positions: tuple[int, ...], table: str, columns: str, keep, convert=None) -> int:
    """Like `load`, but only rows where `keep(selected)` is true are stored.

    `keep` sees the selected raw strings (None for \\N) before conversion,
    so giant upstream tables (link, link_attribute, l_*, url) can be
    streamed once while only the allowlisted relationship slice lands in
    the disk-backed projection. Field counts are still validated for
    every row by `rows()`; `convert` runs only on kept rows.
    """
    count = 0
    batch = []
    statement = f"INSERT INTO {table} ({columns}) VALUES ({','.join('?' for _ in select_positions)})"
    for row in rows(path, expected):
        selected = tuple(None if get(row, position) == NULL else get(row, position) for position in select_positions)
        if not keep(selected):
            continue
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
    # Python accepts more ISO forms than the importer (separators, omitted
    # seconds and second-resolution or large offsets). Publish one UTC shape.
    try:
        return parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")
    except OverflowError as exc:
        raise ValueError("DUMP_TS must be representable as a UTC timestamp") from exc


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
                    CREATE TABLE release_group (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, credit INTEGER, type_id INTEGER);
                    CREATE TABLE release_country (release INTEGER NOT NULL, year INTEGER);
                    CREATE INDEX release_country_id ON release_country(release);
                    CREATE TABLE release_unknown_country (release INTEGER NOT NULL, year INTEGER);
                    CREATE INDEX release_unknown_country_id ON release_unknown_country(release);
                    CREATE TABLE release (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, credit INTEGER NOT NULL, group_id INTEGER NOT NULL);
                    CREATE TABLE recording (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, length INTEGER);
                    CREATE TABLE medium (id INTEGER PRIMARY KEY, release INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE TABLE track (id INTEGER PRIMARY KEY, recording INTEGER NOT NULL, medium INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE INDEX track_recording_id ON track(recording);
                    CREATE TABLE url (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, url TEXT NOT NULL);
                    CREATE TABLE link_type (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL);
                    CREATE TABLE link (id INTEGER PRIMARY KEY, link_type INTEGER NOT NULL);
                    CREATE TABLE link_attribute_type (id INTEGER PRIMARY KEY, gid TEXT NOT NULL);
                    CREATE TABLE link_attribute (link INTEGER NOT NULL, attribute_type INTEGER NOT NULL);
                    CREATE INDEX link_attribute_link ON link_attribute(link);
                    CREATE TABLE l_artist_url (link INTEGER NOT NULL, artist INTEGER NOT NULL, url INTEGER NOT NULL);
                    CREATE INDEX l_artist_url_artist ON l_artist_url(artist);
                    CREATE TABLE l_recording_url (link INTEGER NOT NULL, recording INTEGER NOT NULL, url INTEGER NOT NULL);
                    CREATE INDEX l_recording_url_recording ON l_recording_url(recording);
                    CREATE TABLE l_recording_recording (link INTEGER NOT NULL, e0 INTEGER NOT NULL, e1 INTEGER NOT NULL);
                    CREATE TABLE isrc (recording INTEGER NOT NULL, code TEXT NOT NULL);
                    CREATE INDEX isrc_recording ON isrc(recording);
                """)

                counts = {}
                counts["artist_type"] = load(db, dump_dir / "artist_type", FIELD_COUNTS["artist_type"], (ARTIST_TYPE_LOOKUP_ID, ARTIST_TYPE_NAME), "artist_type", "id,name", as_ints("artist_type.id", (0,)))
                counts["area"] = load(db, dump_dir / "area", FIELD_COUNTS["area"], (AREA_ID,), "area", "id", as_ints("area.id", (0,)))
                counts["iso_3166_1"] = load(db, dump_dir / "iso_3166_1", FIELD_COUNTS["iso_3166_1"], (ISO_AREA_ID, ISO_CODE), "iso_3166_1", "area,code", as_ints("iso_3166_1.area", (0,)))
                counts["artist"] = load(db, dump_dir / "artist", FIELD_COUNTS["artist"], (ARTIST_ID, ARTIST_GID, ARTIST_NAME, ARTIST_SORT_NAME, ARTIST_TYPE_ID, ARTIST_AREA_ID), "artist", "id,gid,name,sort_name,type_id,area_id", as_ints("artist.id", (0, 4, 5), (4, 5)))
                counts["artist_credit_name"] = load(db, dump_dir / "artist_credit_name", FIELD_COUNTS["artist_credit_name"], (CREDIT_ID, CREDIT_POSITION, CREDIT_ARTIST_ID), "artist_credit_name", "credit,position,artist", as_ints("artist_credit_name", (0, 1, 2)))
                counts["release_group_primary_type"] = load(db, dump_dir / "release_group_primary_type", FIELD_COUNTS["release_group_primary_type"], (PRIMARY_TYPE_ID, PRIMARY_TYPE_NAME), "release_group_primary_type", "id,name", as_ints("release_group_primary_type.id", (0,)))
                counts["release_group"] = load(db, dump_dir / "release_group", FIELD_COUNTS["release_group"], (GROUP_ID, GROUP_GID, GROUP_NAME, GROUP_CREDIT_ID, GROUP_TYPE_ID), "release_group", "id,gid,name,credit,type_id", as_ints("release_group.id", (0, 3, 4), (3, 4)))
                counts["release_country"] = load(db, dump_dir / "release_country", FIELD_COUNTS["release_country"], (RELEASE_COUNTRY_RELEASE_ID, RELEASE_COUNTRY_YEAR), "release_country", "release,year", as_ints("release_country", (0, 1), (1,)))
                counts["release_unknown_country"] = load(db, dump_dir / "release_unknown_country", FIELD_COUNTS["release_unknown_country"], (RELEASE_UNKNOWN_COUNTRY_RELEASE_ID, RELEASE_UNKNOWN_COUNTRY_YEAR), "release_unknown_country", "release,year", as_ints("release_unknown_country", (0, 1), (1,)))
                counts["release"] = load(db, dump_dir / "release", FIELD_COUNTS["release"], (RELEASE_ID, RELEASE_GID, RELEASE_NAME, RELEASE_CREDIT_ID, RELEASE_GROUP_ID), "release", "id,gid,name,credit,group_id", as_ints("release.id", (0, 3, 4)))
                counts["recording"] = load(db, dump_dir / "recording", FIELD_COUNTS["recording"], (RECORDING_ID, RECORDING_GID, RECORDING_NAME, RECORDING_LENGTH), "recording", "id,gid,name,length", as_ints("recording.id", (0, 3), (3,)))
                counts["medium"] = load(db, dump_dir / "medium", FIELD_COUNTS["medium"], (MEDIUM_ID, MEDIUM_RELEASE_ID, MEDIUM_POSITION), "medium", "id,release,position", as_ints("medium.id", (0, 1, 2)))
                counts["track"] = load(db, dump_dir / "track", FIELD_COUNTS["track"], (TRACK_ID, TRACK_RECORDING_ID, TRACK_MEDIUM_ID, TRACK_POSITION), "track", "id,recording,medium,position", as_ints("track.id", (0, 1, 2, 3)))
                # Relationship slice: resolve the allowlisted link-type and
                # attribute GIDs to table ids (fail closed when a pinned GID
                # is absent), then stream the giant link tables once, keeping
                # only allowlisted links. The kept-link id set is small
                # (allowlisted types only), so the downstream membership
                # filters are set lookups, not joins.
                counts["link_type"] = load(db, dump_dir / "link_type", FIELD_COUNTS["link_type"], (LINK_TYPE_ID, LINK_TYPE_GID, LINK_TYPE_NAME), "link_type", "id,gid,name", as_ints("link_type.id", (0,)))
                counts["link_attribute_type"] = load(db, dump_dir / "link_attribute_type", FIELD_COUNTS["link_attribute_type"], (ATTR_TYPE_ID, ATTR_TYPE_GID), "link_attribute_type", "id,gid", as_ints("link_attribute_type.id", (0,)))

                def link_type_id(gid: str, what: str) -> int:
                    found = db.execute("SELECT id FROM link_type WHERE gid=?", (gid,)).fetchone()
                    if found is None:
                        raise ValueError(f"pinned link type {what} ({gid}) absent from link_type input")
                    return found[0]

                allowlisted_type_ids = {
                    link_type_id(ARTIST_URL_YOUTUBE_GID, "artist-url youtube"),
                    link_type_id(ARTIST_URL_YOUTUBE_MUSIC_GID, "artist-url youtube music"),
                    link_type_id(ARTIST_URL_FREE_STREAMING_GID, "artist-url free streaming"),
                    link_type_id(ARTIST_URL_STREAMING_GID, "artist-url streaming"),
                    link_type_id(RECORDING_URL_FREE_STREAMING_GID, "recording-url free streaming"),
                    link_type_id(MUSIC_VIDEO_LINK_GID, "recording-recording music video"),
                }
                video_attr = db.execute("SELECT id FROM link_attribute_type WHERE gid=?", (VIDEO_ATTR_GID,)).fetchone()
                if video_attr is None:
                    raise ValueError(f'pinned link attribute "video" ({VIDEO_ATTR_GID}) absent from link_attribute_type input')
                video_attr_id = video_attr[0]

                kept_link_ids: set[int] = set()
                referenced_url_ids: set[int] = set()

                def keep_allowlisted_link(selected) -> bool:
                    link_id = required_int(selected[0], "link.id")
                    if required_int(selected[1], "link.link_type") in allowlisted_type_ids:
                        kept_link_ids.add(link_id)
                        return True
                    return False

                counts["link"] = load_filtered(
                    db, dump_dir / "link", FIELD_COUNTS["link"], (LINK_ID, LINK_LINK_TYPE_ID),
                    "link", "id,link_type", keep_allowlisted_link,
                    as_ints("link.id", (0, 1)),
                )
                if counts["link"] == 0:
                    raise ValueError("link input projects zero allowlisted links; refusing an empty relationship slice")

                def keep_kept_link(selected) -> bool:
                    return required_int(selected[0], "link_attribute.link") in kept_link_ids

                counts["link_attribute"] = load_filtered(
                    db, dump_dir / "link_attribute", FIELD_COUNTS["link_attribute"],
                    (LINK_ATTR_LINK_ID, LINK_ATTR_ATTR_TYPE_ID),
                    "link_attribute", "link,attribute_type", keep_kept_link,
                    as_ints("link_attribute", (0, 1)),
                )

                def keep_l_artist_url(selected) -> bool:
                    if required_int(selected[0], "l_artist_url.link") not in kept_link_ids:
                        return False
                    referenced_url_ids.add(required_int(selected[2], "l_artist_url.url"))
                    return True

                def keep_l_recording_url(selected) -> bool:
                    if required_int(selected[0], "l_recording_url.link") not in kept_link_ids:
                        return False
                    referenced_url_ids.add(required_int(selected[2], "l_recording_url.url"))
                    return True

                def keep_l_recording_recording(selected) -> bool:
                    return required_int(selected[0], "l_recording_recording.link") in kept_link_ids

                counts["l_artist_url"] = load_filtered(
                    db, dump_dir / "l_artist_url", FIELD_COUNTS["l_artist_url"],
                    (L_ARTIST_URL_LINK_ID, L_ARTIST_URL_ARTIST_ID, L_ARTIST_URL_URL_ID),
                    "l_artist_url", "link,artist,url", keep_l_artist_url,
                    as_ints("l_artist_url", (0, 1, 2)),
                )
                counts["l_recording_url"] = load_filtered(
                    db, dump_dir / "l_recording_url", FIELD_COUNTS["l_recording_url"],
                    (L_RECORDING_URL_LINK_ID, L_RECORDING_URL_RECORDING_ID, L_RECORDING_URL_URL_ID),
                    "l_recording_url", "link,recording,url", keep_l_recording_url,
                    as_ints("l_recording_url", (0, 1, 2)),
                )
                counts["l_recording_recording"] = load_filtered(
                    db, dump_dir / "l_recording_recording", FIELD_COUNTS["l_recording_recording"],
                    (L_RR_LINK_ID, L_RR_E0_ID, L_RR_E1_ID),
                    "l_recording_recording", "link,e0,e1", keep_l_recording_recording,
                    as_ints("l_recording_recording", (0, 1, 2)),
                )
                # Only URLs targeted by a kept relationship row are stored:
                # the upstream url table is far larger than the allowlisted
                # slice the mirror serves.
                counts["url"] = load_filtered(
                    db, dump_dir / "url", FIELD_COUNTS["url"], (URL_ID, URL_GID, URL_URL),
                    "url", "id,gid,url",
                    lambda selected: required_int(selected[0], "url.id") in referenced_url_ids,
                    as_ints("url.id", (0,)),
                )
                # ISRCs load whole (like track/medium): the recording join in
                # the projection drops codes for unknown recordings, and the
                # validator rejects malformed codes row by row downstream.
                counts["isrc"] = load(
                    db, dump_dir / "isrc", FIELD_COUNTS["isrc"], (ISRC_RECORDING_ID, ISRC_CODE),
                    "isrc", "recording,code", as_ints("isrc.recording", (0,)),
                )
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
                bad_unknown_country = db.execute("SELECT c.release FROM release_unknown_country c LEFT JOIN release r ON r.id=c.release WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_unknown_country:
                    raise ValueError(f"release_unknown_country references missing release {bad_unknown_country[0]}")
                bad_track = db.execute("SELECT t.id FROM track t LEFT JOIN medium m ON m.id=t.medium LEFT JOIN recording r ON r.id=t.recording WHERE m.id IS NULL OR r.id IS NULL LIMIT 1").fetchone()
                if bad_track:
                    raise ValueError(f"track {bad_track[0]} references a missing medium or recording")
                bad_medium_release = db.execute("SELECT m.id FROM medium m LEFT JOIN release r ON r.id=m.release WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_medium_release:
                    raise ValueError(f"medium {bad_medium_release[0]} references a missing release")
                bad_link_attr_type = db.execute("SELECT la.link FROM link_attribute la LEFT JOIN link_attribute_type t ON t.id=la.attribute_type WHERE t.id IS NULL LIMIT 1").fetchone()
                if bad_link_attr_type:
                    raise ValueError(f"kept link {bad_link_attr_type[0]} carries an attribute of a missing link_attribute_type")
                bad_l_artist = db.execute("SELECT l.artist FROM l_artist_url l LEFT JOIN artist a ON a.id=l.artist WHERE a.id IS NULL LIMIT 1").fetchone()
                if bad_l_artist:
                    raise ValueError(f"artist-url link references missing artist {bad_l_artist[0]}")
                bad_l_recording = db.execute("SELECT l.recording FROM l_recording_url l LEFT JOIN recording r ON r.id=l.recording WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_l_recording:
                    raise ValueError(f"recording-url link references missing recording {bad_l_recording[0]}")
                bad_l_rr = db.execute("SELECT l.e0,l.e1 FROM l_recording_recording l LEFT JOIN recording r0 ON r0.id=l.e0 LEFT JOIN recording r1 ON r1.id=l.e1 WHERE r0.id IS NULL OR r1.id IS NULL LIMIT 1").fetchone()
                if bad_l_rr:
                    raise ValueError(f"music-video link references missing recording ({bad_l_rr[0]}, {bad_l_rr[1]})")
                bad_isrc_recording = db.execute("SELECT i.recording FROM isrc i LEFT JOIN recording r ON r.id=i.recording WHERE r.id IS NULL LIMIT 1").fetchone()
                if bad_isrc_recording:
                    raise ValueError(f"isrc references missing recording {bad_isrc_recording[0]}")

                artist_query = """
                    SELECT a.gid,a.name,a.sort_name,t.name,i.code
                    FROM artist a
                    LEFT JOIN artist_type t ON t.id=a.type_id
                    LEFT JOIN iso_3166_1 i ON i.area=a.area_id
                    LEFT JOIN area ar ON ar.id=i.area
                    ORDER BY a.id
                """
                artist_count = insert_select(db, artist_query, output_dir / "mbdump-artist.tsv", updated_at)
                release_group_query = """
                    SELECT g.gid,g.name,a.gid,pt.name
                    FROM release_group g
                    LEFT JOIN artist_credit_name acn ON acn.credit=g.credit
                        AND acn.position=(SELECT MIN(acn2.position) FROM artist_credit_name acn2 WHERE acn2.credit=g.credit)
                    LEFT JOIN artist a ON a.id=acn.artist
                    LEFT JOIN release_group_primary_type pt ON pt.id=g.type_id
                    ORDER BY g.id
                """
                release_group_count = insert_select(db, release_group_query, output_dir / "mbdump-release_group.tsv", updated_at)
                release_query = """
                    SELECT r.gid,a.gid,r.name,pt.name,
                           (SELECT MIN(year) FROM (SELECT rc.year FROM release_country rc WHERE rc.release=r.id UNION ALL SELECT ruc.year FROM release_unknown_country ruc WHERE ruc.release=r.id) WHERE year IS NOT NULL),
                           g.gid
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
                    lambda row: (row[0], row[1], row[2], row[3], row[4], f"https://coverartarchive.org/release/{row[0]}/front-250", row[5]),
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
                # Every track appearance becomes one membership row, unlike
                # the canonical single release on the recording row above:
                # a recording shared with a reissue/compilation is listed
                # under each release, and the medium position keeps
                # multi-medium releases in album order downstream. The
                # joins are total: bad_track/bad_medium_release above
                # already rejected orphans.
                track_query = """
                    SELECT rel.gid,rec.gid,m.position,t.position
                    FROM track t
                    JOIN medium m ON m.id=t.medium
                    JOIN release rel ON rel.id=m.release
                    JOIN recording rec ON rec.id=t.recording
                    ORDER BY rel.id,m.position,t.position,t.id
                """
                track_count = insert_select(db, track_query, output_dir / "mbdump-track.tsv", updated_at)
                # URLs: only the allowlisted relationship targets the `url`
                # load kept.
                url_query = """
                    SELECT gid,url FROM url ORDER BY id
                """
                url_count = insert_select(db, url_query, output_dir / "mbdump-url.tsv", updated_at)
                # Relationship pairs, deduplicated: one curated row per
                # (entity, url, link type). The joins are total: the
                # reference checks above already rejected orphans, and every
                # kept link resolves to an allowlisted link type by
                # construction of the kept-link set.
                artist_url_query = """
                    SELECT DISTINCT a.gid,u.gid,lt.name
                    FROM l_artist_url l
                    JOIN artist a ON a.id=l.artist
                    JOIN url u ON u.id=l.url
                    JOIN link k ON k.id=l.link
                    JOIN link_type lt ON lt.id=k.link_type
                    ORDER BY a.gid,u.gid,lt.name
                """
                artist_url_count = insert_select(db, artist_url_query, output_dir / "mbdump-artist_url.tsv", updated_at)
                # is_video marks links carrying the "video" attribute: the
                # music-video -> stream join follows only those URLs. One row
                # per (recording, url, link type): when the same triple is
                # linked twice with and without the attribute, ANY video flag
                # wins, so the curated key can never duplicate (the importer
                # refuses duplicate keys within a dump).
                recording_url_query = """
                    SELECT rec.gid,u.gid,lt.name,
                           MAX(CASE WHEN la.attribute_type IS NOT NULL THEN 1 ELSE 0 END)
                    FROM l_recording_url l
                    JOIN recording rec ON rec.id=l.recording
                    JOIN url u ON u.id=l.url
                    JOIN link k ON k.id=l.link
                    JOIN link_type lt ON lt.id=k.link_type
                    LEFT JOIN link_attribute la ON la.link=l.link AND la.attribute_type=?
                    GROUP BY rec.gid,u.gid,lt.name
                    ORDER BY rec.gid,u.gid,lt.name
                """
                recording_url_count = 0
                with (output_dir / "mbdump-recording_url.tsv").open("w", encoding="utf-8", newline="") as output:
                    for row in db.execute(recording_url_query, (video_attr_id,)):
                        is_video = "true" if row[3] else "false"
                        output.write("\t".join(NULL if item is None else str(item) for item in (*row[:3], is_video, updated_at)) + "\n")
                        recording_url_count += 1
                # Music-video pairs, canonically ordered by (lowercased) GID
                # so one undirected link is one row whichever side the dump
                # lists first. The read join treats both sides
                # symmetrically.
                music_video_query = """
                    SELECT DISTINCT
                           CASE WHEN lower(r0.gid)<lower(r1.gid) THEN r0.gid ELSE r1.gid END,
                           CASE WHEN lower(r0.gid)<lower(r1.gid) THEN r1.gid ELSE r0.gid END
                    FROM l_recording_recording l
                    JOIN recording r0 ON r0.id=l.e0
                    JOIN recording r1 ON r1.id=l.e1
                    WHERE r0.id != r1.id
                    ORDER BY 1,2
                """
                music_video_count = insert_select(db, music_video_query, output_dir / "mbdump-recording_music_video.tsv", updated_at)
                # ISRCs, deduplicated and uppercased (the mirror CHECK
                # requires the canonical 12-character uppercase form).
                isrc_query = """
                    SELECT DISTINCT rec.gid,UPPER(i.code)
                    FROM isrc i
                    JOIN recording rec ON rec.id=i.recording
                    ORDER BY rec.gid,2
                """
                isrc_count = insert_select(db, isrc_query, output_dir / "mbdump-recording_isrc.tsv", updated_at)

                for table, input_count, output_count in (
                    ("artist", counts["artist"], artist_count),
                    ("release_group", counts["release_group"], release_group_count),
                    ("release", counts["release"], release_count),
                    ("recording", counts["recording"], recording_count),
                    ("track", counts["track"], track_count),
                    ("url", counts["url"], url_count),
                ):
                    if input_count and output_count == 0:
                        raise ValueError(f"{table} input is non-empty but curated output is empty")
                    if input_count != output_count:
                        raise ValueError(f"{table} input/output row count mismatch: {input_count} != {output_count}")
                # Deduplicated projections: only the empty-output guard
                # applies (dedup legitimately shrinks the row count).
                for table, input_count, output_count in (
                    ("artist_url", counts["l_artist_url"], artist_url_count),
                    ("recording_url", counts["l_recording_url"], recording_url_count),
                    ("recording_music_video", counts["l_recording_recording"], music_video_count),
                    ("recording_isrc", counts["isrc"], isrc_count),
                ):
                    if input_count and output_count == 0:
                        raise ValueError(f"{table} input is non-empty but curated output is empty")
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
                for table in ("artist", "release_group", "release", "recording", "track", "url", "artist_url", "recording_url", "recording_music_video", "recording_isrc"):
                    name = f"mbdump-{table}.tsv"
                    if not filecmp.cmp(output_dir / name, snapshot / name, shallow=False):
                        raise ValueError("this DUMP_TS already has different curated output; use a new timestamp")
            else:
                os.replace(output_dir, snapshot)
            link_tmp = dump_dir / f".curated-link-{uuid.uuid4().hex}"
            os.symlink(snapshot_name, link_tmp)
            os.replace(link_tmp, curated_link)

        print(f"etl: published {snapshot_name} via curated (artist={artist_count}, release_group={release_group_count}, release={release_count}, recording={recording_count}, track={track_count}, url={url_count}, artist_url={artist_url_count}, recording_url={recording_url_count}, recording_music_video={music_video_count}, recording_isrc={isrc_count})")
        return 0
    except (OSError, sqlite3.Error, ValueError) as exc:
        print(f"etl: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
