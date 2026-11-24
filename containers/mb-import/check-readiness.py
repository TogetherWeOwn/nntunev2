#!/usr/bin/env python3
"""Compare an initial mirror load with the operator's import evidence."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


TABLES = (
    "artist", "release_group", "release", "recording", "track", "url",
    "artist_url", "recording_url", "recording_music_video", "recording_isrc",
)
SQL = Path(__file__).resolve().parents[2] / "workers/mb/src"
SHA256 = re.compile(r"[0-9a-f]{64}")
MBID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def load_evidence(path):
    with path.open("rb") as source:
        raw = source.read(512 * 1024 + 1)
    if len(raw) > 512 * 1024:
        raise ValueError
    evidence = json.loads(raw.decode("utf-8"))
    tables, probes = evidence["tables"], evidence["probes"]
    if not isinstance(tables, dict) or set(tables) != set(TABLES):
        raise ValueError
    for entry in tables.values():
        digest, lines, rows = entry["sha256"], entry["lines"], entry["rows"]
        if (not matches(SHA256, digest) or type(lines) is not int
                or type(rows) is not int or not 0 < rows <= lines < 2**63):
            raise ValueError
    if not isinstance(probes, list) or not 1 <= len(probes) <= 5:
        raise ValueError
    for probe in probes:
        query, tracks = probe["query"], probe["tracks"]
        if (not isinstance(query, str) or query != query.strip()
                or not 2 <= len(query) <= 100 or "\0" in query
                or not matches(MBID, probe["release_mbid"])
                or not matches(MBID, probe["artist_mbid"])
                or not isinstance(tracks, list) or not 1 <= len(tracks) <= 1000):
            raise ValueError
        query.encode("utf-8")  # Refuse unpaired surrogates before connecting.
        for track in tracks:
            if (not isinstance(track, list) or len(track) != 3
                    or any(type(pos) is not int or not 0 <= pos <= 32767 for pos in track[:2])
                    or not matches(MBID, track[2])):
                raise ValueError
    return tables, probes


def read_snapshot(tables, probes):
    search = (SQL / "release_search.sql").read_text(encoding="utf-8")
    recordings = (SQL / "release_recordings.sql").read_text(encoding="utf-8")
    statements = [
        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;",
        "SET LOCAL search_path TO pg_catalog, public, pg_temp;",
        "SET LOCAL statement_timeout TO '5min';",
        f"""PREPARE release_search(text) AS
            SELECT coalesce(json_agg(json_build_array(mbid, artist_mbid, artist_name IS NOT NULL)), '[]')
            FROM ({search} LIMIT 25 OFFSET 0) AS hits;""",
        f"""PREPARE release_recordings(text) AS
            SELECT coalesce(json_agg(json_build_array(medium_no, track_no, mbid)), '[]')
            FROM ({recordings}) AS tracks;""",
    ]
    for table in TABLES:
        digest = tables[table]["sha256"]
        # The table is a constant and the digest has passed the hex allowlist.
        statements.append(f"""SELECT json_build_array(
            (SELECT lines_done FROM public.mb_import_cursor
             WHERE table_name = '{table}' AND dump_id = '{digest}'),
            (SELECT count(*) FROM public.mb_{table}),
            (SELECT count(*) FROM public.mb_import_dlq
             WHERE table_name = '{table}' AND dump_id = '{digest}'));""")
    for probe in probes:
        query_hex = probe["query"].encode("utf-8").hex()
        statements.append(f"EXECUTE release_search(convert_from(decode('{query_hex}', 'hex'), 'UTF8'));")
        statements.append(f"EXECUTE release_recordings('{probe['release_mbid']}');")
    statements.append("ROLLBACK;")
    result = subprocess.run(
        ["psql", "-XwqAt", "-v", "ON_ERROR_STOP=1"],
        input="\n".join(statements), capture_output=True, text=True, check=True,
        env={**os.environ, "PGCONNECT_TIMEOUT": "10"},
    )
    return [json.loads(line) for line in result.stdout.splitlines()]


def report(tables, probes, snapshot):
    ready = True
    for table, (cursor, rows, dlq) in zip(TABLES, snapshot[:len(TABLES)], strict=True):
        expected = tables[table]
        lines = expected["lines"]
        passed = (cursor, rows, dlq) == (lines, expected["rows"], lines - expected["rows"])
        print(f"{table}: {'ok' if passed else 'mismatch'} cursor={cursor} rows={rows} dlq={dlq}")
        ready &= passed
    results = snapshot[len(TABLES):]
    for index, (probe, hits, tracks) in enumerate(zip(probes, results[::2], results[1::2], strict=True), 1):
        search_ok = [probe["release_mbid"], probe["artist_mbid"], True] in hits
        tracks_ok = probe["tracks"] == tracks
        print(f"probe {index}: search={'ok' if search_ok else 'mismatch'} "
              f"recordings={'ok' if tracks_ok else 'mismatch'}")
        ready &= search_ok and tracks_ok
    print("readiness: PASS" if ready else "readiness: FAIL")
    return 0 if ready else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path, help="JSON expectations from the initial import")
    args = parser.parse_args()
    try:
        tables, probes = load_evidence(args.evidence)
    except (OSError, ValueError, TypeError, KeyError, RecursionError):
        parser.exit(2, "readiness: could not read valid import evidence\n")
    try:
        return report(tables, probes, read_snapshot(tables, probes))
    except (OSError, ValueError, TypeError, subprocess.CalledProcessError):
        # libpq and SQL errors can carry connection details or catalogue data.
        parser.exit(2, "readiness: could not complete PostgreSQL evidence read\n")


if __name__ == "__main__":
    raise SystemExit(main())
