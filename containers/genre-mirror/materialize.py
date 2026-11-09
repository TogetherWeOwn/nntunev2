#!/usr/bin/env python3
"""Validate a local CC0 extract and prepare an atomic mirror refresh; never apply it."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import sys
import uuid

# Isolated-mode CLI imports only this trusted source sibling, never the input directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from rollup import ROOT_ID_RE, resolve

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from genre_root_coverage import calculate

MAX_BYTES = 32 * 1024**2
MAX_ROWS = 50_000
MAX_GENRES = 10_000
QID = re.compile(r"Q[1-9][0-9]{0,19}\Z")
SHA = re.compile(r"[0-9a-f]{64}\Z")
FILES = {"genres.jsonl", "genre_parents.jsonl", "artist_genres.jsonl"}


def encode(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode("utf-8")


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def invalid_constant(_):
    raise ValueError("non-finite JSON")


def decode(raw):
    return json.loads(raw.decode("utf-8"), object_pairs_hook=pairs, parse_constant=invalid_constant)


def shape(value, keys):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ValueError("invalid record shape")


def read_bounded(path, limit=MAX_BYTES):
    if path.is_symlink() or not stat.S_ISREG(path.stat().st_mode):
        raise ValueError("regular file required")
    with path.open("rb") as stream:
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise ValueError("input byte budget exceeded")
    return raw


def mbid(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", value):
        raise ValueError("invalid artist MBID")
    normalized = str(uuid.UUID(value))
    if uuid.UUID(normalized).int == 0:
        raise ValueError("nil artist MBID")
    return normalized


def qid(value):
    if not isinstance(value, str) or not QID.fullmatch(value):
        raise ValueError("invalid genre QID")
    return value


def load_extract(directory):
    raw_manifest = read_bounded(directory / "manifest.json", 1024**2)
    manifest = decode(raw_manifest)
    shape(manifest, ("schema", "extractor_version", "extractor_sha256", "source", "input", "language", "limits", "counts", "outputs"))
    if manifest["schema"] != "nntune.genre-extract.v1" or manifest["extractor_version"] != "1":
        raise ValueError("unsupported extract version")
    shape(manifest["source"], ("name", "license", "dump_version"))
    if (manifest["source"]["name"] != "wikidata" or manifest["source"]["license"] != "CC0-1.0"
            or not isinstance(manifest["source"]["dump_version"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", manifest["source"]["dump_version"])):
        raise ValueError("unsupported source declaration")
    if not isinstance(manifest["extractor_sha256"], str) or not SHA.fullmatch(manifest["extractor_sha256"]):
        raise ValueError("invalid extractor identity")
    shape(manifest["input"], ("sha256", "bytes", "format"))
    if (not isinstance(manifest["input"]["sha256"], str) or not SHA.fullmatch(manifest["input"]["sha256"])
            or type(manifest["input"]["bytes"]) is not int or manifest["input"]["bytes"] < 0
            or manifest["input"]["format"] not in ("json-array", "jsonl")
            or not isinstance(manifest["language"], str) or not re.fullmatch(r"[a-z][a-z0-9-]{0,34}", manifest["language"])):
        raise ValueError("invalid extract input identity")
    shape(manifest["counts"], ("entities", "invalid_claims"))
    shape(manifest["limits"], ("max_line_bytes", "max_input_bytes", "max_entities", "max_db_bytes", "max_output_bytes", "max_depth"))
    if any(type(v) is not int or v < 0 for section in ("counts", "limits") for v in manifest[section].values()):
        raise ValueError("invalid extract counts or limits")
    shape(manifest["outputs"], FILES)
    tables = {}
    budget = MAX_BYTES - len(raw_manifest)
    for name in sorted(FILES):
        receipt = manifest["outputs"][name]
        shape(receipt, ("sha256", "bytes", "rows"))
        if (type(receipt["bytes"]) is not int or not 0 <= receipt["bytes"] <= budget
                or type(receipt["rows"]) is not int or not 0 <= receipt["rows"] <= MAX_ROWS
                or not isinstance(receipt["sha256"], str) or not SHA.fullmatch(receipt["sha256"])):
            raise ValueError("invalid output receipt")
        raw = read_bounded(directory / name, budget)
        budget -= len(raw)
        if len(raw) != receipt["bytes"] or digest(raw) != receipt["sha256"] or (raw and not raw.endswith(b"\n")):
            raise ValueError("extract digest or framing mismatch")
        lines = raw.split(b"\n")[:-1] if raw else []
        if len(lines) != receipt["rows"]:
            raise ValueError("extract row count mismatch")
        tables[name] = [decode(line) for line in lines]
    genres = tables["genres.jsonl"]
    if len(genres) > MAX_GENRES:
        raise ValueError("genre budget exceeded")
    seen = set()
    for row in genres:
        shape(row, ("genre_qid", "label", "present"))
        key = qid(row["genre_qid"])
        label = row["label"]
        if (key in seen or type(row["present"]) is not bool
                or (not row["present"] and label is not None)
                or (label is not None and (not isinstance(label, str) or not label.strip()
                    or len(label.encode("utf-8")) > 4096
                    or any(ord(c) < 32 or 127 <= ord(c) <= 159 or 0xD800 <= ord(c) <= 0xDFFF for c in label)))):
            raise ValueError("invalid genre row")
        seen.add(key)
    present = {row["genre_qid"] for row in genres if row["present"]}
    for name, fields in (("genre_parents.jsonl", ("genre_qid", "parent_qid")), ("artist_genres.jsonl", ("artist_mbid", "genre_qid"))):
        keys = set()
        for row in tables[name]:
            shape(row, fields)
            qid(row["genre_qid"])
            if row["genre_qid"] not in seen:
                raise ValueError("missing genre reference")
            if name == "genre_parents.jsonl":
                if row["genre_qid"] not in present:
                    raise ValueError("absent genre must be a leaf")
                qid(row["parent_qid"])
                if row["parent_qid"] not in seen:
                    raise ValueError("missing parent reference")
            else:
                if mbid(row["artist_mbid"]) != row["artist_mbid"]:
                    raise ValueError("noncanonical extract MBID")
            key = tuple(row[f] for f in fields)
            if key in keys:
                raise ValueError("duplicate extract row")
            keys.add(key)
    return manifest, digest(raw_manifest), tables


def coverage_payload(cohort, roots, artist_roots):
    shape(cohort, ("schema", "dataset_kind", "artist_mbids", "playable_artist_mbids"))
    if cohort["schema"] != "nntune.genre-artists.v1" or cohort["dataset_kind"] not in ("synthetic", "actual-export"):
        raise ValueError("invalid cohort declaration")
    for key in ("artist_mbids", "playable_artist_mbids"):
        if not isinstance(cohort[key], list) or len(cohort[key]) > MAX_ROWS:
            raise ValueError("cohort row budget exceeded")
    artists = {mbid(x) for x in cohort["artist_mbids"]}
    playable = {mbid(x) for x in cohort["playable_artist_mbids"]}
    if not playable <= artists:
        raise ValueError("playable artists absent from artist mirror export")
    # Reserve transport IDs for retired roots too; never rename canonical IDs.
    if type(roots) is not list:
        raise ValueError("invalid coverage roots")
    mapping, active, slugs = {}, set(), set()
    for root in roots:
        if type(root) is not dict:
            raise ValueError("invalid coverage root")
        canonical = root.get("root_id")
        retired = root.get("retired", False)
        if (type(canonical) is not str or not ROOT_ID_RE.fullmatch(canonical)
                or canonical in mapping or type(retired) is not bool):
            raise ValueError("invalid or duplicate canonical root")
        slug = canonical.replace("_", "-")
        if not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", slug) or slug in slugs:
            raise ValueError("root transport collision or invalid coverage slug")
        mapping[canonical] = slug
        slugs.add(slug)
        if not retired:
            active.add(canonical)
    if type(artist_roots) is not list:
        raise ValueError("invalid coverage memberships")
    memberships = []
    for row in artist_roots:
        shape(row, ("artist_mbid", "root_id"))
        artist = mbid(row["artist_mbid"])
        root = row["root_id"]
        if root is not None and (type(root) is not str or root not in active):
            raise ValueError("unmapped or retired coverage root reference")
        if artist in artists:
            memberships.append({"artist_mbid": artist, "root_id": None if root is None else mapping[root]})
    return ({"schema_version": 1, "dataset_kind": cohort["dataset_kind"],
             "playable_artist_mbids": sorted(playable), "resolved_root_ids": sorted(mapping[r] for r in active),
             "memberships": memberships}, mapping, artists)


def literal(value):
    # The generated transaction pins standard_conforming_strings before literals.
    return "'" + json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).replace("'", "''") + "'::jsonb"


def refresh_sql(genres, memberships, result, config, identity):
    def recordset(value, columns):
        return f"jsonb_to_recordset({literal(value)}) AS x({columns})"
    roots = [{**r, "retired": r.get("retired", False)} for r in config["roots"]]
    overrides = [{**r, "roots": r.get("roots", [])} for r in config["overrides"]]
    # This is an export, not an execution path. Schema install/apply needs its own gate.
    return ("BEGIN;\nSET LOCAL search_path = public, pg_catalog;\nSET LOCAL standard_conforming_strings = on;\n"
        "LOCK TABLE public.genre_snapshot, public.artist_genre, public.genre_root, public.genre, public.genre_rollup, public.genre_override IN SHARE ROW EXCLUSIVE MODE;\n"
        "UPDATE public.genre_root SET retired = true;\n"
        "INSERT INTO public.genre_root(root_id, display_name, sort_order, retired) SELECT root_id, display_name, sort_order, retired FROM "
        + recordset(roots, "root_id text, display_name text, sort_order integer, retired boolean")
        + " ON CONFLICT(root_id) DO UPDATE SET display_name=excluded.display_name, sort_order=excluded.sort_order, retired=excluded.retired;\n"
        "INSERT INTO public.genre(qid,label) SELECT genre_qid,label FROM "
        + recordset(genres, "genre_qid text, label text")
        + " ON CONFLICT(qid) DO UPDATE SET label=excluded.label;\n"
        "DELETE FROM public.genre_rollup;\nINSERT INTO public.genre_rollup(genre_qid,root_id) SELECT genre_qid,root_id FROM "
        + recordset(result["genre_rollup"], "genre_qid text, root_id text") + ";\n"
        "DELETE FROM public.genre_override;\nINSERT INTO public.genre_override(genre_qid,action,roots,reason) SELECT genre_qid,action,roots,reason FROM "
        + recordset(overrides, "genre_qid text, action text, roots text[], reason text") + ";\n"
        "DELETE FROM public.artist_genre WHERE source='wikidata';\n"
        "INSERT INTO public.artist_genre(artist_mbid,genre_qid,source) SELECT x.artist_mbid,x.genre_qid,'wikidata' FROM "
        + recordset(memberships, "artist_mbid uuid, genre_qid text")
        + " JOIN public.mb_artist a ON a.mbid=x.artist_mbid;\n"
        "INSERT INTO public.genre_snapshot(singleton,identity,config) VALUES(true," + literal(identity) + "," + literal(config)
        + ") ON CONFLICT(singleton) DO UPDATE SET identity=excluded.identity,config=excluded.config;\nCOMMIT;\n").encode("utf-8")


def materialize(extract, config_path, cohort_path, output):
    manifest, extract_id, tables = load_extract(extract)
    config_raw = read_bounded(config_path, 1024**2)
    config = decode(config_raw)
    cohort_raw = read_bounded(cohort_path)
    cohort = decode(cohort_raw)
    memberships = [{**r, "source": "wikidata"} for r in tables["artist_genres.jsonl"]]
    result = resolve(tables["genres.jsonl"], tables["genre_parents.jsonl"], memberships, config)
    coverage, mapping, artists = coverage_payload(cohort, config["roots"], result["artist_roots"])
    identity = {"schema": "nntune.genre-mirror.v1", "extract_manifest_sha256": extract_id,
                "config_sha256": digest(config_raw), "taxonomy_version": config["taxonomy_version"],
                "cohort_sha256": digest(cohort_raw), "dataset_kind": cohort["dataset_kind"],
                "source": manifest["source"], "producer": manifest, "real_data_gate": "not_evaluated",
                "coverage_root_id_map": mapping,
                "coverage_adapter": {"schema": "nntune.genre-coverage-adapter.v1",
                                     "canonical_to_coverage": mapping,
                                     "coverage_to_canonical": {v: k for k, v in mapping.items()}},
                "unmatched_artist_mbids": sorted({r["artist_mbid"] for r in memberships} - artists)}
    payloads = {"coverage.json": encode(coverage), "coverage-report.json": encode(calculate(coverage)),
                "diagnostics.json": encode(result["diagnostics"]),
                "genre_rollup.jsonl": b"".join(encode(r) for r in result["genre_rollup"]),
                "refresh.sql": refresh_sql(tables["genres.jsonl"], memberships, result, config, identity),
                "extract-manifest.json": encode(manifest)}
    identity["outputs"] = {name: {"sha256": digest(raw), "bytes": len(raw)} for name, raw in sorted(payloads.items())}
    payloads["manifest.json"] = encode(identity)
    if sum(map(len, payloads.values())) > MAX_BYTES:
        raise ValueError("output byte budget exceeded")
    output.mkdir()  # Existing directories/symlinks are never overwritten.
    try:
        for name, raw in payloads.items():
            (output / name).write_bytes(raw)
    except BaseException:
        shutil.rmtree(output)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("extract", "config", "cohort", "output"):
        parser.add_argument("--" + flag, type=Path, required=True)
    args = parser.parse_args()
    try:
        materialize(args.extract, args.config, args.cohort, args.output)
    except (OSError, ValueError, TypeError, KeyError, RecursionError, MemoryError):
        print("genre-mirror: refused invalid, incomplete or over-budget input/output", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
