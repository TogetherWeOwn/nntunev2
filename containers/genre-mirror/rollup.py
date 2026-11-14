"""Resolve supplied genre and artist rows offline; callers own all IO.

Manual precedence is a resolution rule, not authorization to ingest manual
or file-tag data. Inputs must be appropriately licensed and bounded by callers.
"""

from graph import Graph
from overrides import Overrides
from roots import Roots
from taxonomy import _is_qid, _present_set, validate_config

SOURCES = frozenset({"wikidata", "file_tag", "manual"})


def _qkey(qid):
    return int(qid[1:])


def resolve(genres, parents, memberships, config):
    """Return deterministic genre/artist roots and per-genre diagnostics."""
    validate_config(config, genres)
    present = _present_set(genres)
    if type(parents) is not list or type(memberships) is not list:
        raise ValueError("invalid genre rollup input: rows")
    parent_map = {}
    for row in parents:
        if (type(row) is not dict or not _is_qid(row.get("genre_qid"))
                or not _is_qid(row.get("parent_qid"))):
            raise ValueError("invalid genre rollup input: parents")
        parent_map.setdefault(row["genre_qid"], set()).add(row["parent_qid"])
    parent_lists = {qid: sorted(values, key=_qkey) for qid, values in parent_map.items()}
    universe = {row["genre_qid"] for row in genres}
    by_artist = {}
    for row in memberships:
        if (type(row) is not dict or not _is_qid(row.get("genre_qid"))
                or type(row.get("artist_mbid")) is not str
                or type(row.get("source")) is not str or row["source"] not in SOURCES):
            raise ValueError("invalid genre rollup input: memberships")
        universe.add(row["genre_qid"])
        by_artist.setdefault(row["artist_mbid"], []).append(row)

    limits = config["limits"]
    graph = Graph(parent_lists, frozenset(config["traversal_barriers"]),
                  limits["max_depth"], limits["max_nodes"])
    roots = Roots(config, graph)
    resolver = Overrides(config, roots, present)
    final, diagnostics = {}, []
    for qid in sorted(universe | set(resolver.actions), key=_qkey):
        selected, capped, cycle, missing = resolver.resolve(qid)
        if qid in universe:
            final[qid] = selected
        flags = []
        if capped:
            flags.append("traversal_capped")
        if cycle:
            flags.append("cycle_detected")
        if missing:
            flags.append("override_target_missing")
        if qid in universe and len(selected) > limits["broad_multi_threshold"]:
            flags.append("broad_multi")
        entries = [("flag", flag) for flag in flags]
        if qid in universe and not selected:
            actions = resolver.actions.get(qid, {}) if qid in present else {}
            removed = bool(actions.get("remove") and
                           set(roots.resolve(qid)[0]) & actions["remove"])
            reason = ("excluded" if "exclude" in actions else
                      "unknown_genre" if qid not in present else
                      "removed" if removed else
                      "no_parents" if qid not in parent_lists else "no_root_reached")
            entries.append(("reason", reason))
        diagnostics.extend({"genre_qid": qid, kind: code}
                           for kind, code in sorted(entries, key=lambda item: item[1]))

    genre_rollup = [{"genre_qid": qid, "root_id": rid}
                    for qid in sorted(final, key=_qkey) for rid in final[qid]]
    artist_roots = []
    for artist in sorted(by_artist):
        rows = by_artist[artist]
        if any(row["source"] == "manual" for row in rows):
            rows = [row for row in rows if row["source"] == "manual"]
        selected = {rid for row in rows for rid in final[row["genre_qid"]]}
        artist_roots.extend({"artist_mbid": artist, "root_id": rid}
                            for rid in sorted(selected, key=roots.sort_order.__getitem__))
    return {"genre_rollup": genre_rollup, "artist_roots": artist_roots,
            "diagnostics": diagnostics}
