#!/usr/bin/env python3
"""Project official MusicBrainz COPY files into curated, disk-backed TSVs."""

from __future__ import annotations

import filecmp
import gzip
import hashlib
import io
import os
import re
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
ALIAS_ID, ALIAS_ENTITY_ID, ALIAS_NAME, ALIAS_LOCALE, ALIAS_SORT_NAME, ALIAS_PRIMARY = 1, 2, 3, 4, 8, 15
ARTIST_TYPE_LOOKUP_ID, ARTIST_TYPE_NAME = 1, 2
AREA_ID = 1
ISO_AREA_ID, ISO_CODE = 1, 2
CREDIT_ID, CREDIT_POSITION, CREDIT_ARTIST_ID, CREDIT_NAME, CREDIT_JOIN_PHRASE = 1, 2, 3, 4, 5
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
LINK_TYPE_ID, LINK_TYPE_GID, LINK_TYPE_ENTITY0, LINK_TYPE_ENTITY1, LINK_TYPE_NAME = 1, 4, 5, 6, 7
LINK_ID, LINK_LINK_TYPE_ID = 1, 2
ATTR_TYPE_ID, ATTR_TYPE_GID = 1, 5
LINK_ATTR_LINK_ID, LINK_ATTR_ATTR_TYPE_ID = 1, 2
L_ARTIST_URL_LINK_ID, L_ARTIST_URL_ARTIST_ID, L_ARTIST_URL_URL_ID = 2, 3, 4
L_RECORDING_URL_LINK_ID, L_RECORDING_URL_RECORDING_ID, L_RECORDING_URL_URL_ID = 2, 3, 4
L_RR_LINK_ID, L_RR_E0_ID, L_RR_E1_ID = 2, 3, 4
L_RW_LINK_ID, L_RW_RECORDING_ID, L_RW_WORK_ID = 2, 3, 4
ISRC_RECORDING_ID, ISRC_CODE = 2, 3
WORK_ID, WORK_GID, WORK_NAME, WORK_TYPE_ID = 1, 2, 3, 4
WORK_TYPE_LOOKUP_ID, WORK_TYPE_NAME = 1, 2
LABEL_ID, LABEL_GID, LABEL_NAME, LABEL_CODE, LABEL_TYPE_ID = 1, 2, 3, 10, 11
LABEL_TYPE_LOOKUP_ID, LABEL_TYPE_NAME = 1, 2
RELEASE_LABEL_ID, RELEASE_LABEL_RELEASE_ID, RELEASE_LABEL_LABEL_ID, RELEASE_LABEL_CATNO = 1, 2, 3, 4

FIELD_COUNTS = {
    "artist": 19,
    "artist_alias": 16,
    "artist_type": 6,
    "area": 14,
    "iso_3166_1": 2,
    "artist_credit_name": 5,
    "release": 14,
    "release_alias": 16,
    "release_group": 8,
    "release_group_alias": 16,
    "release_group_primary_type": 6,
    "release_country": 5,
    "release_unknown_country": 4,
    "recording": 9,
    "recording_alias": 16,
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
    "l_recording_work": 9,
    # Upstream isrc is (id, recording, isrc, edits_pending, created): five
    # columns since upstream dropped `source` (the real 20261104 dump has
    # five). The curated projection only keeps the (recording, isrc) pair
    # (positions 2, 3), but the row validator must accept the full
    # upstream width or the first real ISRC row aborts the ETL.
    "isrc": 5,
    "work": 7,
    "work_alias": 16,
    "work_type": 6,
    "label": 16,
    "label_type": 6,
    "release_label": 5,
}

# Allowlisted relationship link-type GIDs (pinned from the MusicBrainz
# relationship-type documentation; the ETL resolves them to table ids and
# refuses a dump where any of them is absent). Only links of these types
# reach the URL and music-video projections. The recording-work projection
# resolves its own core-dump links without using this allowlist.
ARTIST_URL_YOUTUBE_GID = "6a540e5b-58c6-4192-b6ba-dbc71ec8fcf0"
# Verified against the live MusicBrainz web service (artist url-rels):
# the "youtube music" relationship type is
# 631712a0-7525-42ba-b7a3-605aa7a238c4. Every pinned GID above is a valid
# UUID shape (8-4-4-4-12); a malformed pin can never match a real link_type
# row and fails the ETL closed, so keep them exact.
ARTIST_URL_YOUTUBE_MUSIC_GID = "631712a0-7525-42ba-b7a3-605aa7a238c4"
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


# Disk-bounded mode (R2 snapshot spike, `stream_extract.py`): inputs may be
# `<table>.gz` instead of `<table>`, MB_ETL_CONSUME_INPUTS=1 deletes each
# input once it is fully loaded, and MB_ETL_GZIP_OUTPUT=1 writes the curated
# files as `mbdump-<table>.tsv.gz`. All three are off by default, so the
# importer's plain-file contract is unchanged.
CONSUME_INPUTS = os.environ.get("MB_ETL_CONSUME_INPUTS") == "1"
GZIP_OUTPUT = os.environ.get("MB_ETL_GZIP_OUTPUT") == "1"
OUTPUT_SUFFIX = ".tsv.gz" if GZIP_OUTPUT else ".tsv"


def open_input(path: Path):
    packed = path.with_name(path.name + ".gz")
    if path.is_file() and packed.is_file():
        raise ValueError(f"ambiguous input: both {path} and {packed} exist")
    if path.is_file():
        return path, path.open("r", encoding="utf-8", newline="")
    if packed.is_file():
        return packed, gzip.open(packed, "rt", encoding="utf-8", newline="")
    raise ValueError(f"missing input file: {path}")


def open_output(path: Path):
    if GZIP_OUTPUT:
        # mtime=0 keeps the bytes reproducible for the same-DUMP_TS reuse check.
        packed = gzip.GzipFile(path, "wb", compresslevel=1, mtime=0)
        return io.TextIOWrapper(packed, encoding="utf-8", newline="")
    return path.open("w", encoding="utf-8", newline="")


def curated_name(table: str) -> str:
    return f"mbdump-{table}{OUTPUT_SUFFIX}"


def rows(path: Path, expected: int):
    actual, source = open_input(path)
    with source:
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
    if CONSUME_INPUTS:
        actual.unlink()


def load(db: sqlite3.Connection, path: Path, expected: int, select_positions: tuple[int, ...], table: str, columns: str, convert=None) -> int:
    count = 0
    batch = []
    statement = f"INSERT INTO {table} ({columns}) VALUES ({','.join('?' for _ in columns.split(','))})"
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


def as_ints(context: str, integer_fields: tuple[int, ...], nullable_fields: tuple[int, ...] = (), *, mark_canonical_id=False):
    def convert(values, line_number):
        converted = list(values)
        for index in integer_fields:
            item = converted[index]
            if item is None and index not in nullable_fields:
                raise ValueError(f"{context} row {line_number}: required integer is NULL")
            if item is not None:
                converted[index] = required_int(item, f"{context} row {line_number}")
        if mark_canonical_id:
            # Keep legacy integer joins; the sidecar already bounds referenced IDs.
            converted.append(str(converted[0]) == values[0])
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
    with open_output(output_path) as output:
        for row in db.execute(query):
            fields = row_transform(row) if row_transform else row
            output.write("\t".join(NULL if item is None else str(item) for item in (*fields, updated_at)) + "\n")
            count += 1
    return count


COPY_ESCAPE = re.compile(rb"\\(x[0-9a-fA-F]{1,2}|[0-7]{1,3}|[bfnrtv\\])?")
COPY_NAMED = {b"b": b"\b", b"f": b"\f", b"n": b"\n", b"r": b"\r",
              b"t": b"\t", b"v": b"\v", b"\\": b"\\"}


def decode_copy_text(text: str) -> str:
    def unescape(match):
        token = match[1]
        if token is None:
            raise ValueError("invalid COPY escape")
        if token in COPY_NAMED:
            return COPY_NAMED[token]
        return bytes([int(token[1:], 16) if token.startswith(b"x") else int(token, 8)])

    # Numeric escapes represent bytes, not code points. Decode only after
    # assembling all bytes, including any literal UTF-8 in the field.
    decoded = COPY_ESCAPE.sub(unescape, text.encode("utf-8"))
    if b"\0" in decoded:
        raise ValueError("NUL in COPY text")
    return decoded.decode("utf-8")


def load_aliases(db: sqlite3.Connection, dump_dir: Path, entity: str) -> int:
    table = f"{entity}_alias"

    def aliases():
        for line_number, row in enumerate(rows(dump_dir / table, FIELD_COUNTS[table]), 1):
            context = f"{table} row {line_number}"
            raw_id, raw_entity = get(row, ALIAS_ID), get(row, ALIAS_ENTITY_ID)
            if entity != "artist":
                alias_id = positive_key(raw_id, context, "id")
                entity_id = positive_key(raw_entity, context, entity)
            else:
                alias_id = required_int(raw_id, context)
                entity_id = required_int(raw_entity, context)
            name = get(row, ALIAS_NAME)
            locale = value(get(row, ALIAS_LOCALE)) or ""
            sort_name = get(row, ALIAS_SORT_NAME)
            primary = get(row, ALIAS_PRIMARY)
            if primary not in ("t", "f"):
                raise ValueError(f"{context}: invalid primary_for_locale")
            if name == NULL or sort_name == NULL:
                raise ValueError(f"{context}: name and sort_name must not be NULL")
            try:
                name_key, locale_key = decode_copy_text(name), decode_copy_text(locale)
            except ValueError as exc:
                raise ValueError(f"{context}: invalid COPY text in name or locale") from exc
            if entity != "artist":
                try:
                    decode_copy_text(sort_name)
                except ValueError as exc:
                    raise ValueError(f"{context}: invalid COPY text in sort_name") from exc
            yield alias_id, entity_id, name, locale, sort_name, primary, name_key, locale_key

    return db.executemany(f"INSERT INTO {table} VALUES (?,?,?,?,?,?,?,?)", aliases()).rowcount


def project_aliases(db: sqlite3.Connection, entity: str, output_dir: Path, updated_at: str) -> int:
    table = f"{entity}_alias"
    # Pick a whole raw COPY row for each decoded key. Prefer a primary
    # alias, then the greatest raw sort name, then the lowest alias ID.
    # Match the corresponding curated entity's MBID spelling.
    gid = "LOWER(e.gid)" if entity == "recording" else "e.gid"
    query = f"""
        WITH ranked AS (
            SELECT {gid} AS gid,al.*,ROW_NUMBER() OVER (
                PARTITION BY {gid},al.name_key,al.locale_key
                ORDER BY al.primary_flag DESC,al.sort_name DESC,al.id
            ) AS rank
            FROM {table} al JOIN {entity} e ON e.id=al.entity
        )
        SELECT gid,name,locale,sort_name,primary_flag FROM ranked
        WHERE rank=1 ORDER BY gid,name_key,locale_key
    """

    mbid_pattern = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}",
                              re.IGNORECASE if entity in ("release", "release_group") else 0)

    def validate_alias_row(row):
        if not mbid_pattern.fullmatch(row[0]):
            raise ValueError(f"{table}: invalid {entity} MBID")
        return row

    return insert_select(db, query, output_dir / curated_name(table), updated_at,
                         validate_alias_row if entity != "artist" else None)


def positive_key(field: str | None, context: str, column: str) -> int:
    # COPY emits positive serial keys in canonical decimal form.
    if field is not None and re.fullmatch(r"[1-9][0-9]{0,9}", field):
        number = int(field)
        if number <= 2**31 - 1:
            return number
    raise ValueError(f"{context}: invalid {column} (expected a positive 32-bit integer)")


def validate_copy_name(name: str | None, context: str) -> str:
    if name is None:
        raise ValueError(f"{context}: name must not be NULL")
    try:
        return decode_copy_text(name)
    except ValueError as exc:
        raise ValueError(f"{context}: invalid COPY text in name") from exc


def work_type_values(values, line_number):
    context = f"work_type row {line_number}"
    row_id, name = values
    validate_copy_name(name, context)
    return positive_key(row_id, context, "id"), name


def work_values(values, line_number):
    context = f"work row {line_number}"
    row_id, gid, name, type_id = values
    if gid is None or not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", gid):
        raise ValueError(f"{context}: invalid work MBID")
    validate_copy_name(name, context)
    return (positive_key(row_id, context, "id"), gid, name,
            None if type_id is None else positive_key(type_id, context, "type_id"))


def recording_work_values(values, line_number):
    context = f"l_recording_work row {line_number}"
    return tuple(positive_key(field, context, column)
                 for field, column in zip(values, ("link", "recording", "work"), strict=True))


def validate_recording_work_row(row):
    if not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", row[0], re.IGNORECASE):
        raise ValueError("l_recording_work: invalid recording MBID")
    return row


def label_type_values(values, line_number):
    context = f"label_type row {line_number}"
    row_id, name = values
    if len(validate_copy_name(name, context)) > 255:
        raise ValueError(f"{context}: name exceeds 255 characters")
    return positive_key(row_id, context, "id"), name


def label_values(values, line_number):
    context = f"label row {line_number}"
    row_id, gid, name, code, type_id = values
    if gid is None or not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", gid):
        raise ValueError(f"{context}: invalid label MBID")
    validate_copy_name(name, context)
    if code is not None:
        if not re.fullmatch(r"0|-?[1-9][0-9]{0,9}", code) or not -2**31 <= int(code) <= 2**31 - 1:
            raise ValueError(f"{context}: invalid label code (expected a 32-bit integer)")
        code = int(code)
    return (positive_key(row_id, context, "id"), gid, name, code,
            None if type_id is None else positive_key(type_id, context, "type_id"))


def release_label_values(values, line_number):
    context = f"release_label row {line_number}"
    row_id, release_id, label_id, catalog_number = values
    row_id = positive_key(row_id, context, "id")
    release_id = positive_key(release_id, context, "release")
    if label_id is not None:
        label_id = positive_key(label_id, context, "label")
    try:
        number_key = decode_copy_text(catalog_number or "")
    except ValueError as exc:
        raise ValueError(f"{context}: invalid COPY text in catalogue number") from exc
    if len(number_key) > 255:
        raise ValueError(f"{context}: catalogue number exceeds 255 characters")
    return row_id, release_id, label_id, number_key


COPY_ENCODE = str.maketrans({raw.decode(): "\\" + escape.decode() for escape, raw in COPY_NAMED.items()})


def release_label_output(row):
    release_gid, label_gid, number_key = row
    if not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", release_gid):
        raise ValueError("release_label: invalid release MBID")
    return release_gid, label_gid, number_key.translate(COPY_ENCODE)


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
                    PRAGMA temp_store = FILE;
                    CREATE TABLE artist (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, sort_name TEXT NOT NULL, type_id INTEGER, area_id INTEGER);
                    CREATE TABLE artist_alias (
                        id INTEGER PRIMARY KEY, entity INTEGER NOT NULL, name TEXT NOT NULL,
                        locale TEXT NOT NULL, sort_name TEXT NOT NULL, primary_flag TEXT NOT NULL,
                        name_key TEXT NOT NULL, locale_key TEXT NOT NULL
                    );
                    CREATE TABLE release_alias (
                        id INTEGER PRIMARY KEY, entity INTEGER NOT NULL, name TEXT NOT NULL,
                        locale TEXT NOT NULL, sort_name TEXT NOT NULL, primary_flag TEXT NOT NULL,
                        name_key TEXT NOT NULL, locale_key TEXT NOT NULL
                    );
                    CREATE TABLE release_group_alias (
                        id INTEGER PRIMARY KEY, entity INTEGER NOT NULL, name TEXT NOT NULL,
                        locale TEXT NOT NULL, sort_name TEXT NOT NULL, primary_flag TEXT NOT NULL,
                        name_key TEXT NOT NULL, locale_key TEXT NOT NULL
                    );
                    CREATE TABLE recording_alias (
                        id INTEGER PRIMARY KEY, entity INTEGER NOT NULL, name TEXT NOT NULL,
                        locale TEXT NOT NULL, sort_name TEXT NOT NULL, primary_flag TEXT NOT NULL,
                        name_key TEXT NOT NULL, locale_key TEXT NOT NULL
                    );
                    CREATE TABLE artist_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE area (id INTEGER PRIMARY KEY);
                    CREATE TABLE iso_3166_1 (area INTEGER PRIMARY KEY, code TEXT);
                    CREATE TABLE artist_credit_name (
                        credit INTEGER NOT NULL,
                        position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 2147483647),
                        artist INTEGER NOT NULL,
                        name TEXT NOT NULL,
                        join_phrase TEXT NOT NULL,
                        PRIMARY KEY (credit, position)
                    );
                    CREATE TABLE release_group_primary_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE release_group (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, credit INTEGER, type_id INTEGER);
                    CREATE TABLE release_country (release INTEGER NOT NULL, year INTEGER);
                    CREATE INDEX release_country_id ON release_country(release);
                    CREATE TABLE release_unknown_country (release INTEGER NOT NULL, year INTEGER);
                    CREATE INDEX release_unknown_country_id ON release_unknown_country(release);
                    CREATE TABLE release (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, credit INTEGER NOT NULL, group_id INTEGER NOT NULL);
                    CREATE TABLE recording (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL, length INTEGER, canonical_id INTEGER NOT NULL);
                    CREATE TABLE medium (id INTEGER PRIMARY KEY, release INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE TABLE track (id INTEGER PRIMARY KEY, recording INTEGER NOT NULL, medium INTEGER NOT NULL, position INTEGER NOT NULL);
                    CREATE INDEX track_recording_id ON track(recording);
                    CREATE TABLE url (id INTEGER PRIMARY KEY, gid TEXT NOT NULL, url TEXT NOT NULL);
                    CREATE TABLE link_type (
                        id INTEGER PRIMARY KEY, gid TEXT NOT NULL, name TEXT NOT NULL,
                        entity0 TEXT, entity1 TEXT, canonical_id INTEGER NOT NULL, name_key TEXT
                    );
                    CREATE TABLE link (id INTEGER PRIMARY KEY, link_type INTEGER NOT NULL);
                    CREATE TABLE link_attribute_type (id INTEGER PRIMARY KEY, gid TEXT NOT NULL);
                    CREATE TABLE link_attribute (link INTEGER NOT NULL, attribute_type INTEGER NOT NULL);
                    CREATE INDEX link_attribute_link ON link_attribute(link);
                    CREATE TABLE l_artist_url (link INTEGER NOT NULL, artist INTEGER NOT NULL, url INTEGER NOT NULL);
                    CREATE INDEX l_artist_url_artist ON l_artist_url(artist);
                    CREATE TABLE l_recording_url (link INTEGER NOT NULL, recording INTEGER NOT NULL, url INTEGER NOT NULL);
                    CREATE INDEX l_recording_url_recording ON l_recording_url(recording);
                    CREATE TABLE l_recording_recording (link INTEGER NOT NULL, e0 INTEGER NOT NULL, e1 INTEGER NOT NULL);
                    CREATE TABLE l_recording_work (link INTEGER NOT NULL, recording INTEGER NOT NULL, work INTEGER NOT NULL);
                    CREATE TABLE recording_work_link (id INTEGER PRIMARY KEY, link_type INTEGER NOT NULL);
                    CREATE TABLE isrc (recording INTEGER NOT NULL, code TEXT NOT NULL);
                    CREATE INDEX isrc_recording ON isrc(recording);
                    CREATE TABLE work_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE work (id INTEGER PRIMARY KEY, gid TEXT NOT NULL UNIQUE, name TEXT NOT NULL, type_id INTEGER);
                    CREATE TABLE work_alias (
                        id INTEGER PRIMARY KEY, entity INTEGER NOT NULL, name TEXT NOT NULL,
                        locale TEXT NOT NULL, sort_name TEXT NOT NULL, primary_flag TEXT NOT NULL,
                        name_key TEXT NOT NULL, locale_key TEXT NOT NULL
                    );
                    CREATE TABLE label_type (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                    CREATE TABLE label (id INTEGER PRIMARY KEY, gid TEXT NOT NULL UNIQUE, name TEXT NOT NULL, code INTEGER, type_id INTEGER);
                    CREATE TABLE release_label (id INTEGER PRIMARY KEY, release INTEGER NOT NULL, label INTEGER, number_key TEXT NOT NULL);
                """)

                counts = {}
                counts["artist_type"] = load(db, dump_dir / "artist_type", FIELD_COUNTS["artist_type"], (ARTIST_TYPE_LOOKUP_ID, ARTIST_TYPE_NAME), "artist_type", "id,name", as_ints("artist_type.id", (0,)))
                counts["area"] = load(db, dump_dir / "area", FIELD_COUNTS["area"], (AREA_ID,), "area", "id", as_ints("area.id", (0,)))
                counts["iso_3166_1"] = load(db, dump_dir / "iso_3166_1", FIELD_COUNTS["iso_3166_1"], (ISO_AREA_ID, ISO_CODE), "iso_3166_1", "area,code", as_ints("iso_3166_1.area", (0,)))
                counts["artist"] = load(db, dump_dir / "artist", FIELD_COUNTS["artist"], (ARTIST_ID, ARTIST_GID, ARTIST_NAME, ARTIST_SORT_NAME, ARTIST_TYPE_ID, ARTIST_AREA_ID), "artist", "id,gid,name,sort_name,type_id,area_id", as_ints("artist.id", (0, 4, 5), (4, 5)))
                counts["artist_alias"] = load_aliases(db, dump_dir, "artist")
                counts["work_type"] = load(
                    db, dump_dir / "work_type", FIELD_COUNTS["work_type"],
                    (WORK_TYPE_LOOKUP_ID, WORK_TYPE_NAME), "work_type", "id,name", work_type_values,
                )
                counts["work"] = load(
                    db, dump_dir / "work", FIELD_COUNTS["work"],
                    (WORK_ID, WORK_GID, WORK_NAME, WORK_TYPE_ID), "work", "id,gid,name,type_id", work_values,
                )
                counts["work_alias"] = load_aliases(db, dump_dir, "work")
                load(
                    db, dump_dir / "label_type", FIELD_COUNTS["label_type"],
                    (LABEL_TYPE_LOOKUP_ID, LABEL_TYPE_NAME), "label_type", "id,name", label_type_values,
                )
                counts["label"] = load(
                    db, dump_dir / "label", FIELD_COUNTS["label"],
                    (LABEL_ID, LABEL_GID, LABEL_NAME, LABEL_CODE, LABEL_TYPE_ID),
                    "label", "id,gid,name,code,type_id", label_values,
                )
                load(
                    db, dump_dir / "release_label", FIELD_COUNTS["release_label"],
                    (RELEASE_LABEL_ID, RELEASE_LABEL_RELEASE_ID, RELEASE_LABEL_LABEL_ID, RELEASE_LABEL_CATNO),
                    "release_label", "id,release,label,number_key", release_label_values,
                )
                counts["artist_credit_name"] = load(
                    db, dump_dir / "artist_credit_name", FIELD_COUNTS["artist_credit_name"],
                    (CREDIT_ID, CREDIT_POSITION, CREDIT_ARTIST_ID, CREDIT_NAME, CREDIT_JOIN_PHRASE),
                    "artist_credit_name", "credit,position,artist,name,join_phrase",
                    as_ints("artist_credit_name", (0, 1, 2)),
                )
                counts["release_group_primary_type"] = load(db, dump_dir / "release_group_primary_type", FIELD_COUNTS["release_group_primary_type"], (PRIMARY_TYPE_ID, PRIMARY_TYPE_NAME), "release_group_primary_type", "id,name", as_ints("release_group_primary_type.id", (0,)))
                counts["release_group"] = load(db, dump_dir / "release_group", FIELD_COUNTS["release_group"], (GROUP_ID, GROUP_GID, GROUP_NAME, GROUP_CREDIT_ID, GROUP_TYPE_ID), "release_group", "id,gid,name,credit,type_id", as_ints("release_group.id", (0, 3, 4), (3, 4)))
                counts["release_country"] = load(db, dump_dir / "release_country", FIELD_COUNTS["release_country"], (RELEASE_COUNTRY_RELEASE_ID, RELEASE_COUNTRY_YEAR), "release_country", "release,year", as_ints("release_country", (0, 1), (1,)))
                counts["release_unknown_country"] = load(db, dump_dir / "release_unknown_country", FIELD_COUNTS["release_unknown_country"], (RELEASE_UNKNOWN_COUNTRY_RELEASE_ID, RELEASE_UNKNOWN_COUNTRY_YEAR), "release_unknown_country", "release,year", as_ints("release_unknown_country", (0, 1), (1,)))
                counts["release"] = load(db, dump_dir / "release", FIELD_COUNTS["release"], (RELEASE_ID, RELEASE_GID, RELEASE_NAME, RELEASE_CREDIT_ID, RELEASE_GROUP_ID), "release", "id,gid,name,credit,group_id", as_ints("release.id", (0, 3, 4)))
                counts["release_alias"] = load_aliases(db, dump_dir, "release")
                counts["release_group_alias"] = load_aliases(db, dump_dir, "release_group")
                counts["recording"] = load(
                    db, dump_dir / "recording", FIELD_COUNTS["recording"],
                    (RECORDING_ID, RECORDING_GID, RECORDING_NAME, RECORDING_LENGTH),
                    "recording", "id,gid,name,length,canonical_id", as_ints("recording.id", (0, 3), (3,), mark_canonical_id=True),
                )
                counts["recording_alias"] = load_aliases(db, dump_dir, "recording")
                counts["medium"] = load(db, dump_dir / "medium", FIELD_COUNTS["medium"], (MEDIUM_ID, MEDIUM_RELEASE_ID, MEDIUM_POSITION), "medium", "id,release,position", as_ints("medium.id", (0, 1, 2)))
                counts["track"] = load(db, dump_dir / "track", FIELD_COUNTS["track"], (TRACK_ID, TRACK_RECORDING_ID, TRACK_MEDIUM_ID, TRACK_POSITION), "track", "id,recording,medium,position", as_ints("track.id", (0, 1, 2, 3)))
                # Relationship slice: resolve the allowlisted link-type and
                # attribute GIDs to table ids (fail closed when a pinned GID
                # is absent), then stream the giant link tables once, keeping
                # only allowlisted links. The kept-link id set is small
                # (allowlisted types only), so the downstream membership
                # filters are set lookups, not joins.
                counts["link_type"] = load(
                    db, dump_dir / "link_type", FIELD_COUNTS["link_type"],
                    (LINK_TYPE_ID, LINK_TYPE_GID, LINK_TYPE_NAME, LINK_TYPE_ENTITY0, LINK_TYPE_ENTITY1),
                    "link_type", "id,gid,name,entity0,entity1,canonical_id", as_ints("link_type.id", (0,), mark_canonical_id=True),
                )
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

                counts["l_recording_work"] = load(
                    db, dump_dir / "l_recording_work", FIELD_COUNTS["l_recording_work"],
                    (L_RW_LINK_ID, L_RW_RECORDING_ID, L_RW_WORK_ID),
                    "l_recording_work", "link,recording,work", recording_work_values,
                )
                db.execute("CREATE INDEX l_recording_work_link ON l_recording_work(link)")
                kept_link_ids: set[int] = set()
                referenced_url_ids: set[int] = set()

                def stage_link_and_keep_allowlisted(selected) -> bool:
                    link_id = required_int(selected[0], "link.id")
                    type_id = required_int(selected[1], "link.link_type")
                    # Resolve recording-work links during the same scan, but
                    # never add them to the URL/music-video allowlist by reference.
                    if counts["l_recording_work"] and 0 < link_id <= 2**31 - 1 and db.execute(
                        "SELECT 1 FROM l_recording_work WHERE link=? LIMIT 1", (link_id,),
                    ).fetchone():
                        context = "l_recording_work link"
                        db.execute("INSERT INTO recording_work_link VALUES (?,?)", (
                            positive_key(selected[0], context, "id"),
                            positive_key(selected[1], context, "link_type"),
                        ))
                    if type_id in allowlisted_type_ids:
                        kept_link_ids.add(link_id)
                        return True
                    return False

                counts["link"] = load_filtered(
                    db, dump_dir / "link", FIELD_COUNTS["link"], (LINK_ID, LINK_LINK_TYPE_ID),
                    "link", "id,link_type", stage_link_and_keep_allowlisted,
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
                bad_alias = db.execute("SELECT al.entity FROM artist_alias al LEFT JOIN artist a ON a.id=al.entity WHERE a.id IS NULL LIMIT 1").fetchone()
                if bad_alias:
                    raise ValueError(f"artist_alias references missing artist {bad_alias[0]}")
                bad_recording_alias = db.execute("""
                    SELECT al.entity,r.id FROM recording_alias al LEFT JOIN recording r ON r.id=al.entity
                    WHERE r.id IS NULL OR NOT r.canonical_id LIMIT 1
                """).fetchone()
                if bad_recording_alias:
                    entity_id, resolved_id = bad_recording_alias
                    reason = "missing recording" if resolved_id is None else "invalid recording id"
                    raise ValueError(f"recording_alias references {reason} {entity_id}")
                for entity in ("release", "release_group", "work"):
                    missing_alias_parent = db.execute(f"""
                        SELECT al.entity FROM {entity}_alias al LEFT JOIN {entity} e ON e.id=al.entity
                        WHERE e.id IS NULL LIMIT 1
                    """).fetchone()
                    if missing_alias_parent:
                        raise ValueError(f"{entity}_alias references missing {entity} {missing_alias_parent[0]}")
                bad_work_type = db.execute("""
                    SELECT w.id,w.type_id FROM work w LEFT JOIN work_type t ON t.id=w.type_id
                    WHERE w.type_id IS NOT NULL AND t.id IS NULL LIMIT 1
                """).fetchone()
                if bad_work_type:
                    raise ValueError(f"work {bad_work_type[0]} references missing work_type {bad_work_type[1]}")
                bad_label_type = db.execute("""
                    SELECT l.id,l.type_id FROM label l LEFT JOIN label_type t ON t.id=l.type_id
                    WHERE l.type_id IS NOT NULL AND t.id IS NULL LIMIT 1
                """).fetchone()
                if bad_label_type:
                    raise ValueError(f"label {bad_label_type[0]} references missing label_type {bad_label_type[1]}")
                bad_release_label = db.execute("""
                    SELECT rl.release,rl.label,r.id FROM release_label rl
                    LEFT JOIN release r ON r.id=rl.release
                    LEFT JOIN label l ON l.id=rl.label
                    WHERE r.id IS NULL OR (rl.label IS NOT NULL AND l.id IS NULL) LIMIT 1
                """).fetchone()
                if bad_release_label:
                    release_id, label_id, resolved_release = bad_release_label
                    missing = f"release {release_id}" if resolved_release is None else f"label {label_id}"
                    raise ValueError(f"release_label references missing {missing}")
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
                # Dangling URL targets fail closed like every other orphan
                # above: the url load only keeps IDs referenced by kept
                # links, so a kept edge whose URL row is absent from the
                # dump would otherwise be silently dropped by the inner
                # joins in the projection queries below.
                bad_l_artist_url = db.execute("SELECT l.url FROM l_artist_url l LEFT JOIN url u ON u.id=l.url WHERE u.id IS NULL LIMIT 1").fetchone()
                if bad_l_artist_url:
                    raise ValueError(f"artist-url link references missing url {bad_l_artist_url[0]}")
                bad_l_recording_url = db.execute("SELECT l.url FROM l_recording_url l LEFT JOIN url u ON u.id=l.url WHERE u.id IS NULL LIMIT 1").fetchone()
                if bad_l_recording_url:
                    raise ValueError(f"recording-url link references missing url {bad_l_recording_url[0]}")
                bad_l_rr = db.execute("SELECT l.e0,l.e1 FROM l_recording_recording l LEFT JOIN recording r0 ON r0.id=l.e0 LEFT JOIN recording r1 ON r1.id=l.e1 WHERE r0.id IS NULL OR r1.id IS NULL LIMIT 1").fetchone()
                if bad_l_rr:
                    raise ValueError(f"music-video link references missing recording ({bad_l_rr[0]}, {bad_l_rr[1]})")
                bad_recording_work = db.execute("""
                    SELECT CASE
                        WHEN r.id IS NULL THEN 'references missing recording ' || l.recording
                        WHEN w.id IS NULL THEN 'references missing work ' || l.work
                        WHEN k.id IS NULL THEN 'references missing link ' || l.link
                        WHEN lt.id IS NULL THEN 'references missing link_type ' || k.link_type
                        WHEN NOT r.canonical_id THEN 'invalid recording id ' || r.id
                        WHEN NOT lt.canonical_id THEN 'invalid link_type id ' || lt.id
                        ELSE 'link_type ' || lt.id || ' is not recording-to-work'
                    END
                    FROM l_recording_work l
                    LEFT JOIN recording r ON r.id=l.recording
                    LEFT JOIN work w ON w.id=l.work
                    LEFT JOIN recording_work_link k ON k.id=l.link
                    LEFT JOIN link_type lt ON lt.id=k.link_type
                    WHERE r.id IS NULL OR w.id IS NULL OR k.id IS NULL OR lt.id IS NULL
                        OR NOT r.canonical_id OR NOT lt.canonical_id
                        OR lt.entity0 IS NOT 'recording' OR lt.entity1 IS NOT 'work'
                    LIMIT 1
                """).fetchone()
                if bad_recording_work:
                    raise ValueError(f"l_recording_work: {bad_recording_work[0]}")
                # Decode only referenced type names, once per type. Legacy URL
                # and music-video projections still use the raw name and GIDs.
                for type_id, name in db.execute("""
                    SELECT id,name FROM link_type
                    WHERE id IN (SELECT link_type FROM recording_work_link)
                """).fetchall():
                    name_key = validate_copy_name(name, "l_recording_work link_type")
                    if not name_key.strip():
                        raise ValueError("l_recording_work link_type: name must not be empty")
                    db.execute("UPDATE link_type SET name_key=? WHERE id=?", (name_key, type_id))
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
                artist_count = insert_select(db, artist_query, output_dir / curated_name("artist"), updated_at)
                artist_alias_count = project_aliases(db, "artist", output_dir, updated_at)
                recording_alias_count = project_aliases(db, "recording", output_dir, updated_at)
                release_group_query = """
                    SELECT g.gid,g.name,a.gid,pt.name
                    FROM release_group g
                    LEFT JOIN artist_credit_name acn ON acn.credit=g.credit
                        AND acn.position=(SELECT MIN(acn2.position) FROM artist_credit_name acn2 WHERE acn2.credit=g.credit)
                    LEFT JOIN artist a ON a.id=acn.artist
                    LEFT JOIN release_group_primary_type pt ON pt.id=g.type_id
                    ORDER BY g.id
                """
                release_group_count = insert_select(db, release_group_query, output_dir / curated_name("release_group"), updated_at)
                release_group_alias_count = project_aliases(db, "release_group", output_dir, updated_at)
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
                    output_dir / curated_name("release"),
                    updated_at,
                    lambda row: (row[0], row[1], row[2], row[3], row[4], f"https://coverartarchive.org/release/{row[0]}/front-250", row[5]),
                )
                release_alias_count = project_aliases(db, "release", output_dir, updated_at)
                # Release-group-only credits do not project here. Preserve
                # the credited spelling and join phrase as upstream COPY text.
                release_credit_query = """
                    SELECT r.gid,c.position,a.gid,c.name,c.join_phrase
                    FROM release r
                    JOIN artist_credit_name c ON c.credit=r.credit
                    JOIN artist a ON a.id=c.artist
                    ORDER BY r.id,c.position
                """
                release_credit_count = insert_select(db, release_credit_query, output_dir / curated_name("release_credit"), updated_at)
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
                recording_count = insert_select(db, recording_query, output_dir / curated_name("recording"), updated_at)
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
                track_count = insert_select(db, track_query, output_dir / curated_name("track"), updated_at)
                # URLs: only the allowlisted relationship targets the `url`
                # load kept.
                url_query = """
                    SELECT gid,url FROM url ORDER BY id
                """
                url_count = insert_select(db, url_query, output_dir / curated_name("url"), updated_at)
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
                artist_url_count = insert_select(db, artist_url_query, output_dir / curated_name("artist_url"), updated_at)
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
                with open_output(output_dir / curated_name("recording_url")) as output:
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
                music_video_count = insert_select(db, music_video_query, output_dir / curated_name("recording_music_video"), updated_at)
                # ISRCs, deduplicated and uppercased (the mirror CHECK
                # requires the canonical 12-character uppercase form).
                isrc_query = """
                    SELECT DISTINCT rec.gid,UPPER(i.code)
                    FROM isrc i
                    JOIN recording rec ON rec.id=i.recording
                    ORDER BY rec.gid,2
                """
                isrc_count = insert_select(db, isrc_query, output_dir / curated_name("recording_isrc"), updated_at)

                work_query = """
                    SELECT w.gid,w.name,t.name
                    FROM work w LEFT JOIN work_type t ON t.id=w.type_id
                    ORDER BY w.id
                """
                work_count = insert_select(db, work_query, output_dir / curated_name("work"), updated_at)
                work_alias_count = project_aliases(db, "work", output_dir, updated_at)

                # Group directed triples by decoded type name, choosing one
                # deterministic COPY spelling without emitting raw delimiters.
                recording_work_query = """
                    SELECT r.gid,w.gid,MIN(lt.name)
                    FROM l_recording_work l
                    JOIN recording r ON r.id=l.recording
                    JOIN work w ON w.id=l.work
                    JOIN recording_work_link k ON k.id=l.link
                    JOIN link_type lt ON lt.id=k.link_type
                    GROUP BY r.gid,w.gid,lt.name_key
                    ORDER BY r.gid,w.gid,lt.name_key
                """
                recording_work_count = insert_select(
                    db, recording_work_query, output_dir / curated_name("recording_work"),
                    updated_at, validate_recording_work_row,
                )

                label_query = """
                    SELECT l.gid,l.name,l.code,t.name
                    FROM label l LEFT JOIN label_type t ON t.id=l.type_id
                    ORDER BY l.id
                """
                label_count = insert_select(db, label_query, output_dir / curated_name("label"), updated_at)

                # An absent label is not an edge. A real label with no number
                # uses empty text. Sort and deduplicate before re-escaping COPY.
                release_label_query = """
                    SELECT DISTINCT LOWER(r.gid),l.gid,rl.number_key
                    FROM release_label rl
                    JOIN release r ON r.id=rl.release
                    JOIN label l ON l.id=rl.label
                    ORDER BY 1,2,3
                """
                release_label_count = insert_select(
                    db, release_label_query, output_dir / curated_name("release_label"),
                    updated_at, release_label_output,
                )

                for table, input_count, output_count in (
                    ("artist", counts["artist"], artist_count),
                    ("work", counts["work"], work_count),
                    ("label", counts["label"], label_count),
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
                    ("artist_alias", counts["artist_alias"], artist_alias_count),
                    ("release_alias", counts["release_alias"], release_alias_count),
                    ("release_group_alias", counts["release_group_alias"], release_group_alias_count),
                    ("work_alias", counts["work_alias"], work_alias_count),
                    ("recording_alias", counts["recording_alias"], recording_alias_count),
                    ("artist_url", counts["l_artist_url"], artist_url_count),
                    ("recording_url", counts["l_recording_url"], recording_url_count),
                    ("recording_music_video", counts["l_recording_recording"], music_video_count),
                    ("recording_work", counts["l_recording_work"], recording_work_count),
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
                for output in output_dir.iterdir():
                    previous = snapshot / output.name
                    if not previous.is_file() or not filecmp.cmp(output, previous, shallow=False):
                        raise ValueError("this DUMP_TS already has different curated output; use a fresh dump directory or a new dump")
            else:
                os.replace(output_dir, snapshot)
            link_tmp = dump_dir / f".curated-link-{uuid.uuid4().hex}"
            os.symlink(snapshot_name, link_tmp)
            os.replace(link_tmp, curated_link)

        print(f"etl: published {snapshot_name} via curated (artist={artist_count}, artist_alias={artist_alias_count}, release_group={release_group_count}, release={release_count}, release_credit={release_credit_count}, recording={recording_count}, track={track_count}, url={url_count}, artist_url={artist_url_count}, recording_url={recording_url_count}, recording_music_video={music_video_count}, recording_isrc={isrc_count}, work={work_count}, recording_work={recording_work_count}, "
              f"recording_alias={recording_alias_count}, release_alias={release_alias_count}, release_group_alias={release_group_alias_count}, "
              f"work_alias={work_alias_count}, label={label_count}, release_label={release_label_count})")
        return 0
    except (OSError, sqlite3.Error, ValueError) as exc:
        print(f"etl: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
