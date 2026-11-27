"""Resolve only repository-owned synthetic fixture memberships, never live data.

The fixed pairs are from the synthetic genre-extract fixture. A source or CC0
label supplied by a caller is not proof of provenance; no external source is
admitted here. This is not a production ingest or a coverage measurement.
"""

import json
from pathlib import Path

from cohort import MAX_ROWS, _mbid, admit_cohort
from extract_receipts import FILES, verify_receipts
from extract_receipts import MAX_ROWS as MAX_EXTRACT_ROWS
from rollup import resolve
from taxonomy import _is_qid

# P434/P136 pairs in genre-extract/tests/fixtures/synthetic.json.
FIXTURE_PAIRS = frozenset({
    ("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "Q101"),
    ("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "Q102"),
    ("22222222-2222-4222-8222-222222222222", "Q104"),
    ("66666666-6666-4666-8666-666666666666", "Q105"),
})


def resolve_synthetic(cohort_bytes, genres, parents, memberships, config):
    """Validate supplied fixture rows against the admitted cohort and resolve them."""
    if any(type(rows) is not list or len(rows) > MAX_EXTRACT_ROWS for rows in (genres, parents)):
        raise ValueError("invalid or over-budget synthetic graph rows")
    if type(memberships) is not list or len(memberships) > min(MAX_ROWS, len(FIXTURE_PAIRS)):
        raise ValueError("invalid or over-budget synthetic memberships")
    cohort = admit_cohort(cohort_bytes)
    artists = set(cohort["artist_mbids"])
    seen = set()
    rows = []
    for row in memberships:
        if (type(row) is not dict or set(row) != {"artist_mbid", "genre_qid", "source"}
                or type(row["source"]) is not str or row["source"] != "synthetic-fixture"
                or not _is_qid(row["genre_qid"])):
            raise ValueError("invalid or unsupported synthetic membership")
        artist = _mbid(row["artist_mbid"])
        key = (artist, row["genre_qid"])
        if artist not in artists or key not in FIXTURE_PAIRS or key in seen:
            raise ValueError("unverified, duplicate or out-of-cohort membership")
        seen.add(key)
        rows.append({"artist_mbid": artist, "genre_qid": key[1], "source": "wikidata"})
    return resolve(genres, parents, rows, config)


def resolve_fixture_extract(cohort_bytes, directory, config):
    """Resolve a receipt-checked extract only if it matches the shipped fixture."""
    cohort = admit_cohort(cohort_bytes)
    _, manifest_digest, outputs = verify_receipts(directory)
    expected = Path(__file__).resolve().parent.parent / "genre-extract/tests/fixtures/expected"
    if any(outputs[name] != (expected / name).read_bytes() for name in FILES):
        raise ValueError("extract is not the repository synthetic fixture")

    def rows(name):
        return [json.loads(line) for line in outputs[name].splitlines()]

    artists = set(cohort["artist_mbids"])
    memberships = [{**row, "source": "synthetic-fixture"}
                   for row in rows("artist_genres.jsonl") if row["artist_mbid"] in artists]
    result = resolve_synthetic(cohort_bytes, rows("genres.jsonl"),
                               rows("genre_parents.jsonl"), memberships, config)
    return {"manifest_sha256": manifest_digest, "resolution": result}
