#!/usr/bin/env python3
"""Bounded offline projection of CC0 Wikidata P434/P136/P279 statements."""

from __future__ import annotations

import argparse
import bz2
import gzip
import hashlib
import json
import math
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import sys
import tempfile
import unicodedata
import uuid
import zlib

VERSION = "1"
SCHEMA = "nntune.genre-extract.v1"
QID = re.compile(r"Q[1-9][0-9]{0,19}\Z", re.ASCII)
MBID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z", re.ASCII)


class Refused(ValueError):
    """A diagnostic that contains no untrusted values or local paths."""


class ArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, "genre-extract: refused (invalid command-line arguments)\n")


def encode(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise Refused("duplicate JSON object key")
        result[key] = value
    return result


def invalid_constant(value: str) -> None:
    raise Refused("non-finite JSON number")


def finite_float(value: str) -> float:
    number = float(value)
    if not math.isfinite(number):
        raise Refused("non-finite JSON number")
    return number


def entities(stream, args, identity: dict, counts: dict):
    digest = hashlib.sha256()
    size = 0
    state = "start" if args.format == "json-array" else "jsonl"
    comma = False
    seen = False
    for line_number in range(1, args.max_entities + 4):
        # The extra byte detects oversized records without allocating a whole line.
        raw = stream.readline(min(args.max_line_bytes, args.max_input_bytes - size) + 1)
        if not raw:
            break
        size += len(raw)
        if size > args.max_input_bytes:
            raise Refused("max-input-bytes exceeded")
        if len(raw) > args.max_line_bytes:
            raise Refused("max-line-bytes exceeded")
        digest.update(raw)
        line = raw.decode("utf-8").strip(" \t\r\n")
        if not line:
            raise Refused(f"blank input record at line {line_number}")
        if state == "start":
            if line == "[]":
                state = "done"
            elif line == "[":
                state = "array"
            else:
                raise Refused("expected standalone JSON array opener")
            continue
        if state == "done":
            raise Refused("data after JSON array end")
        if state == "array":
            if line == "]":
                if comma:
                    raise Refused("trailing array comma")
                state = "done"
                continue
            if seen and not comma:
                raise Refused("missing array comma")
            comma = line.endswith(",")
            if comma:
                line = line[:-1]
            seen = True
        try:
            entity = json.loads(line, object_pairs_hook=unique_object,
                                parse_constant=invalid_constant, parse_float=finite_float)
        except (ValueError, RecursionError) as exc:
            raise Refused(f"invalid JSON record at line {line_number}") from exc
        counts["entities"] += 1
        if counts["entities"] > args.max_entities:
            raise Refused("max-entities exceeded")
        if not isinstance(entity, dict):
            raise Refused("entity must be an object")
        yield entity
    else:
        raise Refused("too many input records")
    if state not in ("jsonl", "done"):
        raise Refused("truncated JSON array")
    identity.update(sha256=digest.hexdigest(), bytes=size, format=args.format)


def claims(entity: dict, prop: str, counts: dict):
    values = entity.get("claims", {})
    if not isinstance(values, dict):
        raise Refused("claims must be an object")
    statements = values.get(prop, [])
    if not isinstance(statements, list):
        raise Refused("property statements must be an array")
    for statement in statements:
        if not isinstance(statement, dict):
            raise Refused("statement must be an object")
        if statement.get("rank") == "deprecated":
            continue
        snak = statement.get("mainsnak")
        if (statement.get("type") != "statement" or not isinstance(snak, dict)
                or statement.get("rank") not in ("normal", "preferred")):
            counts["invalid_claims"] += 1
            continue
        if snak.get("snaktype") in ("somevalue", "novalue"):
            continue
        expected = "external-id" if prop == "P434" else "wikibase-item"
        dv = snak.get("datavalue")
        if (snak.get("property") != prop or snak.get("snaktype") != "value"
                or snak.get("datatype") != expected or not isinstance(dv, dict)):
            counts["invalid_claims"] += 1
            continue
        value = dv.get("value")
        if prop == "P434":
            if dv.get("type") == "string" and isinstance(value, str) and MBID.fullmatch(value):
                yield str(uuid.UUID(value))
                continue
        elif dv.get("type") == "wikibase-entityid" and isinstance(value, dict):
            qid = value.get("id")
            number = value.get("numeric-id")
            if (value.get("entity-type") == "item" and isinstance(qid, str) and QID.fullmatch(qid)
                    and ("numeric-id" not in value or (type(number) is int and number == int(qid[1:])))):
                yield qid
                continue
        counts["invalid_claims"] += 1


def label(entity: dict, language: str) -> str | None:
    labels = entity.get("labels", {})
    if not isinstance(labels, dict):
        raise Refused("labels must be an object")
    if language not in labels:
        return None
    entry = labels[language]
    if not isinstance(entry, dict) or entry.get("language") != language:
        raise Refused("invalid label language")
    value = entry.get("value")
    if (not isinstance(value, str) or not value.strip() or len(value.encode("utf-8")) > 4096
            or any(unicodedata.category(c) in ("Cc", "Cs") for c in value)):
        raise Refused("invalid label text")
    return value


def load(db: sqlite3.Connection, stream, args, identity: dict, counts: dict) -> None:
    for entity in entities(stream, args, identity, counts):
        if entity.get("type") in ("property", "lexeme"):
            continue
        qid = entity.get("id")
        if entity.get("type") != "item" or not isinstance(qid, str) or not QID.fullmatch(qid):
            raise Refused("invalid item identity")
        # A repeated item is ambiguous even if its selected statements happen to agree.
        db.execute("INSERT INTO nodes VALUES (?, ?)", (qid, label(entity, args.language)))
        for parent in claims(entity, "P279", counts):
            db.execute("INSERT OR IGNORE INTO parents VALUES (?, ?)", (qid, parent))
        mbids = sorted(set(claims(entity, "P434", counts)))
        genres = sorted(set(claims(entity, "P136", counts)))
        for mbid in mbids:
            for genre in genres:
                db.execute("INSERT OR IGNORE INTO memberships VALUES (?, ?)", (mbid, genre))


def select_ancestry(db: sqlite3.Connection, max_depth: int) -> None:
    db.execute("INSERT OR IGNORE INTO selected SELECT genre, 0 FROM memberships")
    for depth in range(max_depth):
        db.execute("""INSERT OR IGNORE INTO selected
            SELECT p.parent, ? FROM selected s JOIN parents p ON p.genre = s.qid
            WHERE s.depth = ?""", (depth + 1, depth))
        if db.execute("SELECT changes()").fetchone()[0] == 0:
            break
    if db.execute("""SELECT 1 FROM selected s JOIN parents p ON p.genre = s.qid
            LEFT JOIN selected t ON t.qid = p.parent WHERE t.qid IS NULL LIMIT 1""").fetchone():
        raise Refused("max-depth exceeded; ancestry would be incomplete")


def write_rows(path: Path, rows, keys: tuple[str, ...], budget: list[int]) -> dict:
    digest = hashlib.sha256()
    size = count = 0
    with path.open("xb") as target:
        for row in rows:
            record = dict(zip(keys, row))
            if "present" in record:
                record["present"] = bool(record["present"])
            raw = encode(record)
            budget[0] -= len(raw)
            if budget[0] < 0:
                raise Refused("max-output-bytes exceeded")
            target.write(raw)
            digest.update(raw)
            size += len(raw)
            count += 1
    return {"sha256": digest.hexdigest(), "bytes": size, "rows": count}


def extract(args) -> None:
    if not stat.S_ISREG(args.input.stat().st_mode):
        raise Refused("input must be a local regular file")
    # Reserving the destination exclusively prevents replacing even an empty directory.
    # Consumers admit it only after manifest.json appears as the final completion marker.
    args.output.mkdir()
    try:
        with tempfile.TemporaryDirectory(prefix=".genre-extract-", dir=args.output.parent) as scratch:
            db = sqlite3.connect(Path(scratch) / "projection.sqlite3")
            try:
                db.execute("PRAGMA hard_heap_limit=67108864")
                limit = db.execute("PRAGMA hard_heap_limit").fetchone()
                if not limit or type(limit[0]) is not int or not 0 < limit[0] <= 67108864:
                    raise Refused("SQLite hard heap limit unavailable")
                db.executescript(f"""
                    PRAGMA page_size=4096;
                    PRAGMA max_page_count={args.max_db_bytes // 4096};
                    PRAGMA cache_size=-8192;
                    PRAGMA journal_mode=OFF;
                    PRAGMA synchronous=OFF;
                    PRAGMA temp_store=MEMORY;
                    CREATE TABLE nodes(qid TEXT PRIMARY KEY, label TEXT) WITHOUT ROWID;
                    CREATE TABLE parents(genre TEXT, parent TEXT, PRIMARY KEY(genre,parent)) WITHOUT ROWID;
                    CREATE TABLE memberships(mbid TEXT, genre TEXT, PRIMARY KEY(mbid,genre)) WITHOUT ROWID;
                    CREATE TABLE selected(qid TEXT PRIMARY KEY, depth INTEGER) WITHOUT ROWID;
                """)
                identity = {}
                counts = {"entities": 0, "invalid_claims": 0}
                opener = gzip.open if args.input.suffix == ".gz" else bz2.open if args.input.suffix == ".bz2" else open
                with opener(args.input, "rb") as stream:
                    load(db, stream, args, identity, counts)
                select_ancestry(db, args.max_depth)
                outputs = {}
                budget = [args.max_output_bytes]
                outputs["artist_genres.jsonl"] = write_rows(args.output / "artist_genres.jsonl",
                    db.execute("SELECT mbid, genre FROM memberships ORDER BY mbid, genre"),
                    ("artist_mbid", "genre_qid"), budget)
                outputs["genres.jsonl"] = write_rows(args.output / "genres.jsonl",
                    db.execute("""SELECT s.qid, n.label, n.qid IS NOT NULL FROM selected s
                        LEFT JOIN nodes n ON n.qid=s.qid ORDER BY s.qid"""),
                    ("genre_qid", "label", "present"), budget)
                outputs["genre_parents.jsonl"] = write_rows(args.output / "genre_parents.jsonl",
                    db.execute("""SELECT p.genre, p.parent FROM parents p
                        JOIN selected s ON s.qid=p.genre ORDER BY p.genre, p.parent"""),
                    ("genre_qid", "parent_qid"), budget)
                manifest = {
                    "schema": SCHEMA, "extractor_version": VERSION,
                    "extractor_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                    "source": {"name": "wikidata", "license": "CC0-1.0", "dump_version": args.dump_version},
                    "input": identity, "language": args.language,
                    "limits": {key: getattr(args, key) for key in (
                        "max_line_bytes", "max_input_bytes", "max_entities", "max_db_bytes",
                        "max_output_bytes", "max_depth")},
                    "counts": counts, "outputs": outputs,
                }
                raw = encode(manifest)
                if len(raw) > budget[0]:
                    raise Refused("max-output-bytes exceeded")
                marker = args.output / ".manifest.tmp"
                marker.write_bytes(raw)
                marker.rename(args.output / "manifest.json")
            finally:
                db.close()
    except BaseException:
        # The directory was created by this run; never touch a pre-existing output.
        shutil.rmtree(args.output)
        raise


def main() -> int:
    parser = ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dump-version", required=True)
    parser.add_argument("--format", choices=("json-array", "jsonl"), default="json-array")
    parser.add_argument("--language", default="en")
    parser.add_argument("--max-line-bytes", type=int, default=16 * 1024**2)
    parser.add_argument("--max-input-bytes", type=int, default=8 * 1024**3)
    parser.add_argument("--max-entities", type=int, default=10_000_000)
    parser.add_argument("--max-db-bytes", type=int, default=1024**3)
    parser.add_argument("--max-output-bytes", type=int, default=1024**3)
    parser.add_argument("--max-depth", type=int, default=128)
    args = parser.parse_args()
    if (min(args.max_line_bytes, args.max_input_bytes, args.max_entities, args.max_output_bytes) < 1
            or max(args.max_line_bytes, args.max_input_bytes) >= sys.maxsize
            or args.max_db_bytes < 65536 or args.max_db_bytes > 1024**4
            or args.max_depth < 0 or args.max_depth > 10000):
        parser.error("positive limits required; DB 64 KiB..1 TiB; depth 0..10000")
    if not re.fullmatch(r"[a-z][a-z0-9-]{0,34}", args.language):
        parser.error("invalid language code")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", args.dump_version):
        parser.error("dump-version must be a bounded version identifier, not a path or URL")
    try:
        extract(args)
    except (OSError, ValueError, sqlite3.Error, EOFError, RecursionError, MemoryError, zlib.error) as exc:
        # Only our own fixed diagnostics may be shown, never parser/SQL/path details.
        reason = str(exc) if isinstance(exc, Refused) else type(exc).__name__
        print(f"genre-extract: refused ({reason})", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
