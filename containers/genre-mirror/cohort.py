"""Admit explicitly synthetic offline cohort bytes; does not measure coverage.

Adapted from the cohort validation in the retained materializer at ca357b8e.
The declaration is not proof of provenance; callers must supply synthetic data.
"""

import json
import re
import uuid

MAX_BYTES = 32 * 1024**2
MAX_ROWS = 50_000
MBID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z")


def _pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _invalid_constant(_):
    raise ValueError("non-finite JSON")


def _mbid(value):
    if type(value) is not str or not MBID.fullmatch(value):
        raise ValueError("invalid artist MBID")
    parsed = uuid.UUID(value)
    if parsed.int == 0:
        raise ValueError("nil artist MBID")
    return str(parsed)


def admit_cohort(raw):
    """Validate bounded JSON bytes and return normalized synthetic artist lists."""
    if type(raw) is not bytes or len(raw) > MAX_BYTES:
        raise ValueError("cohort byte budget exceeded or invalid input")
    try:
        cohort = json.loads(raw.decode("utf-8"), object_pairs_hook=_pairs,
                            parse_constant=_invalid_constant)
    except (UnicodeError, RecursionError) as exc:
        raise ValueError("invalid cohort JSON") from exc
    if (not isinstance(cohort, dict)
            or set(cohort) != {"schema", "dataset_kind", "artist_mbids", "playable_artist_mbids"}
            or cohort["schema"] != "nntune.genre-artists.v1"
            or cohort["dataset_kind"] != "synthetic"):
        raise ValueError("invalid synthetic cohort declaration")
    normalized = {}
    for key in ("artist_mbids", "playable_artist_mbids"):
        rows = cohort[key]
        if not isinstance(rows, list) or len(rows) > MAX_ROWS:
            raise ValueError("cohort row budget exceeded or invalid list")
        values = [_mbid(value) for value in rows]
        if len(set(values)) != len(values):
            raise ValueError("duplicate cohort artist")
        normalized[key] = sorted(values)
    if not set(normalized["playable_artist_mbids"]) <= set(normalized["artist_mbids"]):
        raise ValueError("playable artists absent from cohort")
    return {"schema": cohort["schema"], "dataset_kind": "synthetic", **normalized}
