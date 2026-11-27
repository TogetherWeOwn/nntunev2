"""Render an inert SQL refresh text for the fixed synthetic genre fixture.

Adapted from refresh_sql in the retained materializer at ca357b8e, narrowed:
only the fixed fixture memberships of synthetic_resolver are admitted and
deletes are scoped to the rows this payload owns. Nothing here connects to a
database, applies the SQL, schedules or installs anything. Target schema is the
candidate origin/feat/cc0-genre-mirror-schema:migrations/0041_cc0_genre.sql,
absent from main; this is not a production ingest or a coverage measurement.
"""

import hashlib
import json

from synthetic_resolver import FIXTURE_PAIRS, resolve_synthetic

MAX_SQL_BYTES = 4 * 1024**2

# Exact rows of containers/genre-extract/tests/fixtures/expected/{genres,
# genre_parents}.jsonl. Callers cannot supply genres, parents or labels.
FIXTURE_GENRES = (
    ("Q100", "synthetic root alpha", True), ("Q101", None, True),
    ("Q102", "synthetic branch beta", True), ("Q104", None, False),
    ("Q105", "cyc a", True), ("Q106", "cyc b", True), ("Q107", None, False),
)
FIXTURE_PARENTS = (("Q100", "Q107"), ("Q101", "Q100"), ("Q102", "Q100"),
                   ("Q105", "Q106"), ("Q106", "Q105"))


def _literal(value):
    # ASCII-only JSON; the transaction pins standard_conforming_strings first.
    text = json.dumps(value, sort_keys=True, separators=(",", ":"))
    return "'" + text.replace("'", "''") + "'::jsonb"


def _rows(value, columns):
    return f"jsonb_to_recordset({_literal(value)}) AS x({columns})"


def _canonical(config):
    """Sort every order-insensitive list so permuted configs serialize equally."""
    return {**config,
            "traversal_barriers": sorted(config["traversal_barriers"]),
            "roots": sorted(({**r, "anchors": sorted(r["anchors"])} for r in config["roots"]),
                            key=lambda r: r["root_id"]),
            "overrides": sorted(({**r, "roots": sorted(r.get("roots", []))}
                                 for r in config["overrides"]),
                                key=lambda r: (r["genre_qid"], r["action"]))}


def synthetic_refresh_sql(cohort_bytes, memberships, config):
    """Return deterministic SQL bytes for the fixed synthetic fixture only.

    Memberships may select a subset of the fixture. The refresh owns every
    fixture pair: it deletes all fixture wikidata rows, genre roll-ups and
    overrides, then inserts the current selection and configuration. Nothing
    outside those fixture keys is deleted.
    """
    if (type(memberships) is not list or len(memberships) > len(FIXTURE_PAIRS)
            or any(type(row) is not dict for row in memberships)
            or type(config) is not dict):
        raise ValueError("invalid or over-budget synthetic refresh input")
    genres = [{"genre_qid": q, "present": p} for q, _, p in FIXTURE_GENRES]
    parents = [{"genre_qid": c, "parent_qid": p} for c, p in FIXTURE_PARENTS]
    result = resolve_synthetic(cohort_bytes, genres, parents, memberships, config)
    admitted = {q for q, _, _ in FIXTURE_GENRES}
    present = {q for q, _, exists in FIXTURE_GENRES if exists}
    if (not {row["genre_qid"] for row in result["genre_rollup"]} <= admitted
            or not {row["genre_qid"] for row in config["overrides"]} <= present):
        raise ValueError("roll-up or override genre outside admitted fixture scope")
    config = _canonical(config)
    genre_rows = [{"genre_qid": q, "label": label} for q, label, _ in FIXTURE_GENRES]
    selected = sorted({(row["artist_mbid"].lower(), row["genre_qid"]) for row in memberships})
    artist_rows = [{"artist_mbid": a, "genre_qid": q} for a, q in selected]
    owned_rows = [{"artist_mbid": a, "genre_qid": q} for a, q in sorted(FIXTURE_PAIRS)]
    roots = [{**r, "retired": r.get("retired", False)} for r in config["roots"]]
    overrides = [{**r, "roots": r.get("roots", [])} for r in config["overrides"]]
    identity = {"schema": "nntune.genre-mirror.v1", "dataset_kind": "synthetic",
                "cohort_sha256": hashlib.sha256(cohort_bytes).hexdigest(),
                "real_data_gate": "not_evaluated"}
    sql = ("BEGIN;\nSET LOCAL search_path = public, pg_catalog;\n"
           "SET LOCAL standard_conforming_strings = on;\n"
           "LOCK TABLE public.genre_snapshot, public.artist_genre, public.genre_root, "
           "public.genre, public.genre_rollup, public.genre_override IN SHARE ROW EXCLUSIVE MODE;\n"
           "INSERT INTO public.genre_root(root_id,display_name,sort_order,retired) "
           "SELECT root_id,display_name,sort_order,retired FROM "
           + _rows(roots, "root_id text, display_name text, sort_order integer, retired boolean")
           + " ON CONFLICT(root_id) DO UPDATE SET display_name=excluded.display_name, "
           "sort_order=excluded.sort_order, retired=excluded.retired;\n"
           "INSERT INTO public.genre(qid,label) SELECT genre_qid,label FROM "
           + _rows(genre_rows, "genre_qid text, label text")
           + " ON CONFLICT(qid) DO UPDATE SET label=excluded.label;\n"
           "DELETE FROM public.genre_rollup WHERE genre_qid IN (SELECT genre_qid FROM "
           + _rows(genre_rows, "genre_qid text") + ");\n"
           "INSERT INTO public.genre_rollup(genre_qid,root_id) SELECT genre_qid,root_id FROM "
           + _rows(result["genre_rollup"], "genre_qid text, root_id text") + ";\n"
           "DELETE FROM public.genre_override WHERE genre_qid IN (SELECT genre_qid FROM "
           + _rows(genre_rows, "genre_qid text") + ");\n"
           "INSERT INTO public.genre_override(genre_qid,action,roots,reason) "
           "SELECT genre_qid,action,roots,reason FROM "
           + _rows(overrides, "genre_qid text, action text, roots text[], reason text")
           + " ON CONFLICT(genre_qid,action) DO UPDATE SET roots=excluded.roots, reason=excluded.reason;\n"
           "DELETE FROM public.artist_genre t USING "
           + _rows(owned_rows, "artist_mbid uuid, genre_qid text")
           + " WHERE t.source='wikidata' AND t.artist_mbid=x.artist_mbid AND t.genre_qid=x.genre_qid;\n"
           "INSERT INTO public.artist_genre(artist_mbid,genre_qid,source) "
           "SELECT x.artist_mbid,x.genre_qid,'wikidata' FROM "
           + _rows(artist_rows, "artist_mbid uuid, genre_qid text")
           + " JOIN public.mb_artist a ON a.mbid=x.artist_mbid;\n"
           "INSERT INTO public.genre_snapshot(singleton,identity,config) VALUES(true,"
           + _literal(identity) + "," + _literal(config)
           + ") ON CONFLICT(singleton) DO UPDATE SET identity=excluded.identity, config=excluded.config;\n"
           "COMMIT;\n").encode("ascii")
    if len(sql) > MAX_SQL_BYTES:
        raise ValueError("refresh SQL byte budget exceeded")
    return sql
