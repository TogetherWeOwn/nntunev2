#!/usr/bin/env python3
"""`pg_trgm` release-search baseline for the R2 snapshot spike.

Usage: pg_baseline.py load <curated-dir>
       pg_baseline.py search <golden.tsv> <out-hits.tsv>

`PSQL` (default `docker exec -i mbpg psql -U postgres`) is the psql
command. `load` copies the curated release and artist files into
`mb_release(mbid, title, artist_mbid)` / `mb_artist(mbid, name)` with the
mirror's GIN `gin_trgm_ops` title index (`migrations/0001_mb_mirror.sql`).
`search` runs the Worker's release `search_sql` text verbatim
(`workers/mb/src/lib.rs`, `Kind::Release`) as a prepared statement with
`LIMIT 5 OFFSET 0`, on `q` normalized like the HTTP layer, and writes the
same hit columns as `snapshot_eval search` plus per-query milliseconds.
"""

from __future__ import annotations

import os
import shlex
import subprocess
import sys
from pathlib import Path

PSQL = shlex.split(os.environ.get("PSQL", "docker exec -i mbpg psql -U postgres")) + [
    "-v", "ON_ERROR_STOP=1", "-X", "-q",
]

# Verbatim from `search_sql(Kind::Release, 5, 0)`.
RELEASE_SEARCH_SQL = (
    "SELECT r.mbid::text AS mbid, r.title AS label, similarity(r.title, $1) AS score, "
    "r.artist_mbid::text AS artist_mbid, a.name AS artist_name "
    "FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid "
    "WHERE r.title % $1 "
    "ORDER BY score DESC, label ASC, r.mbid ASC LIMIT 5 OFFSET 0"
)


def psql(sql: str, *, stdin=None, extra=()) -> str:
    if stdin is None:
        result = subprocess.run(PSQL + list(extra) + ["-c", sql], check=True,
                                capture_output=True, text=True)
        return result.stdout
    result = subprocess.run(PSQL + list(extra) + ["-c", sql], check=True,
                            stdin=stdin, capture_output=True, text=True)
    return result.stdout


def copy_in(table: str, path: Path) -> None:
    decompress = ["gzip", "-dc", str(path)] if path.suffix == ".gz" else ["cat", str(path)]
    with subprocess.Popen(decompress, stdout=subprocess.PIPE) as source:
        psql(f"COPY {table} FROM STDIN", stdin=source.stdout)
        if source.wait() != 0:
            raise SystemExit(f"pg_baseline: reading {path} failed")


def curated(directory: Path, table: str) -> Path:
    for name in (f"mbdump-{table}.tsv.gz", f"mbdump-{table}.tsv"):
        if (directory / name).is_file():
            return directory / name
    raise SystemExit(f"pg_baseline: missing curated {table} in {directory}")


def load(directory: Path) -> None:
    psql("CREATE EXTENSION IF NOT EXISTS pg_trgm")
    psql("CREATE UNLOGGED TABLE stage_artist (gid text, name text, sort_name text, type text, country text, updated_at text)")
    psql("CREATE UNLOGGED TABLE stage_release (gid text, artist_gid text, title text, release_type text, year text, caa_url text, group_gid text, updated_at text)")
    copy_in("stage_artist", curated(directory, "artist"))
    copy_in("stage_release", curated(directory, "release"))
    psql("CREATE TABLE mb_artist AS SELECT gid::uuid AS mbid, name FROM stage_artist")
    psql("CREATE TABLE mb_release AS SELECT gid::uuid AS mbid, title, artist_gid::uuid AS artist_mbid FROM stage_release")
    psql("DROP TABLE stage_artist, stage_release")
    psql("ALTER TABLE mb_artist ADD PRIMARY KEY (mbid)")
    psql("ALTER TABLE mb_release ADD PRIMARY KEY (mbid)")
    psql("CREATE INDEX mb_release_title_trgm_idx ON mb_release USING gin (title gin_trgm_ops)")
    psql("ANALYZE mb_artist")
    psql("ANALYZE mb_release")


def literal(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def normalize_query(raw: str) -> str:
    return " ".join(raw.split()).lower()


def search(golden: Path, out: Path) -> None:
    lines = ["\\timing on", "\\pset format unaligned", "\\pset tuples_only on",
             "\\pset fieldsep '\\t'", f"PREPARE s(text) AS {RELEASE_SEARCH_SQL};"]
    for row in golden.read_text(encoding="utf-8").splitlines():
        cols = row.split("\t")
        lines.append(f"\\echo @@{cols[0]}")
        lines.append(f"EXECUTE s({literal(normalize_query(cols[1]))});")
    script = "\n".join(lines) + "\n"
    result = subprocess.run(PSQL + ["-f", "-"], input=script, check=True,
                            capture_output=True, text=True)
    current, rank = None, 0
    with out.open("w", encoding="utf-8") as target:
        for line in result.stdout.splitlines():
            if line.startswith("@@"):
                current, rank = line[2:], 0
            elif line.startswith("Time: ") and current is not None:
                ms = line.split()[1]
                target.write(f"{current}\t0\t\t\t\t{ms}\n")
            elif line and current is not None:
                mbid, label, score, _artist_mbid, artist_name = (line.split("\t") + [""] * 5)[:5]
                rank += 1
                label = label.replace("\t", " ")
                target.write(f"{current}\t{rank}\t{mbid}\t{label}\t{artist_name}\t{score}\n")


def main() -> int:
    if len(sys.argv) == 3 and sys.argv[1] == "load":
        load(Path(sys.argv[2]))
        return 0
    if len(sys.argv) == 4 and sys.argv[1] == "search":
        search(Path(sys.argv[2]), Path(sys.argv[3]))
        return 0
    print(__doc__.splitlines()[2], file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
