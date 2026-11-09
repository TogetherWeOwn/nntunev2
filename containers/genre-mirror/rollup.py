"""Pure offline genre graph/config resolver (stdlib only, no IO).

Resolves each genre to product root ids over P279 (subclass of) edges only,
applies configured overrides, and derives artist roots. Callers own all IO,
schema and identifier/budget validation of the row inputs.

Manual membership precedence here is a resolution rule only: it neither
produces nor authorizes manual or file_tag ingestion.
"""

import re

QID_RE = re.compile(r"Q[1-9][0-9]{0,19}")
ROOT_ID_RE = re.compile(r"[a-z0-9_]{1,32}")
SOURCES = frozenset({"wikidata", "file_tag", "manual"})
ACTIONS = frozenset({"add", "remove", "replace", "exclude"})

MAX_ROOTS = 64
MAX_ANCHORS_PER_ROOT = 64
MAX_BARRIERS = 1024
MAX_OVERRIDES = 10000
MAX_DEPTH_LIMIT = 64
MAX_NODES_LIMIT = 100000
MAX_THRESHOLD_LIMIT = 64
MAX_NAME_LEN = 64
MAX_REASON_LEN = 200
MAX_SORT_ORDER = 1000000

TOP_KEYS = frozenset(
    {"taxonomy_version", "traversal_barriers", "limits", "roots", "overrides"}
)
LIMIT_KEYS = frozenset({"max_depth", "max_nodes", "broad_multi_threshold"})
ROOT_KEYS = frozenset({"root_id", "display_name", "sort_order", "anchors", "retired"})
OVERRIDE_KEYS = frozenset({"genre_qid", "action", "roots", "reason"})


def _bad(what):
    raise ValueError("invalid genre rollup config: " + what)


def _is_int(value, lo, hi):
    return type(value) is int and lo <= value <= hi


def _is_qid(value):
    return type(value) is str and QID_RE.fullmatch(value) is not None


def _is_root_id(value):
    return type(value) is str and ROOT_ID_RE.fullmatch(value) is not None


def _is_text(value, max_len):
    if type(value) is not str or not value.strip() or len(value) > max_len:
        return False
    return not any(ord(c) < 32 or 127 <= ord(c) <= 159 or 0xD800 <= ord(c) <= 0xDFFF for c in value)


def _qkey(qid):
    return int(qid[1:])


def _present_set(genres):
    if type(genres) is not list:
        raise ValueError("invalid genre rollup input: genres")
    present = set()
    for g in genres:
        if type(g) is not dict or not _is_qid(g.get("genre_qid")):
            raise ValueError("invalid genre rollup input: genres")
        if type(g.get("present")) is not bool:
            raise ValueError("invalid genre rollup input: genres")
        if g["present"]:
            present.add(g["genre_qid"])
    return present


def validate_config(config, genres):
    """Strictly validate config against genres; return the config or ValueError."""
    present = _present_set(genres)
    if type(config) is not dict or set(config) != TOP_KEYS:
        _bad("shape")
    if not _is_int(config["taxonomy_version"], 1, 1):
        _bad("taxonomy_version")

    barriers = config["traversal_barriers"]
    if type(barriers) is not list or len(barriers) > MAX_BARRIERS:
        _bad("traversal_barriers")
    if not all(_is_qid(b) for b in barriers) or len(set(barriers)) != len(barriers):
        _bad("traversal_barriers")

    limits = config["limits"]
    if type(limits) is not dict or set(limits) != LIMIT_KEYS:
        _bad("limits")
    if not _is_int(limits["max_depth"], 1, MAX_DEPTH_LIMIT):
        _bad("limits")
    if not _is_int(limits["max_nodes"], 1, MAX_NODES_LIMIT):
        _bad("limits")
    if not _is_int(limits["broad_multi_threshold"], 1, MAX_THRESHOLD_LIMIT):
        _bad("limits")

    roots = config["roots"]
    if type(roots) is not list or not roots or len(roots) > MAX_ROOTS:
        _bad("roots")
    root_ids, sorts, anchors_seen = set(), set(), set()
    active = set()
    barrier_set = set(barriers)
    for root in roots:
        if type(root) is not dict:
            _bad("root")
        keys = set(root)
        if not {"root_id", "display_name", "sort_order", "anchors"} <= keys <= ROOT_KEYS:
            _bad("root")
        rid = root["root_id"]
        if not _is_root_id(rid) or rid in root_ids:
            _bad("root_id")
        root_ids.add(rid)
        if not _is_text(root["display_name"], MAX_NAME_LEN):
            _bad("display_name")
        if not _is_int(root["sort_order"], 1, MAX_SORT_ORDER) or root["sort_order"] in sorts:
            _bad("sort_order")
        sorts.add(root["sort_order"])
        retired = root.get("retired", False)
        if type(retired) is not bool:
            _bad("retired")
        anchors = root["anchors"]
        if type(anchors) is not list or len(anchors) > MAX_ANCHORS_PER_ROOT:
            _bad("anchors")
        if not all(_is_qid(a) for a in anchors):
            _bad("anchors")
        for a in anchors:
            if a in anchors_seen or a in barrier_set:
                _bad("anchors")
            anchors_seen.add(a)
        if not retired:
            if not anchors or not any(a in present for a in anchors):
                _bad("active root anchors")
            active.add(rid)

    overrides = config["overrides"]
    if type(overrides) is not list or len(overrides) > MAX_OVERRIDES:
        _bad("overrides")
    seen = set()
    by_genre = {}
    for ov in overrides:
        if type(ov) is not dict:
            _bad("override")
        keys = set(ov)
        if not {"genre_qid", "action", "reason"} <= keys <= OVERRIDE_KEYS:
            _bad("override")
        qid, action = ov["genre_qid"], ov["action"]
        if not _is_qid(qid) or type(action) is not str or action not in ACTIONS:
            _bad("override")
        if not _is_text(ov["reason"], MAX_REASON_LEN):
            _bad("override reason")
        if (qid, action) in seen:
            _bad("duplicate override")
        seen.add((qid, action))
        roots_list = ov.get("roots", [])
        if type(roots_list) is not list:
            _bad("override roots")
        if action == "exclude":
            if roots_list:
                _bad("override roots")
        else:
            if not roots_list or len(roots_list) > MAX_ROOTS:
                _bad("override roots")
            if len(set(r for r in roots_list if type(r) is str)) != len(roots_list):
                _bad("override roots")
            if not all(_is_root_id(r) and r in active for r in roots_list):
                _bad("override roots")
        by_genre.setdefault(qid, {})[action] = set(roots_list)
    for acts in by_genre.values():
        if ("replace" in acts or "exclude" in acts) and len(acts) > 1:
            _bad("override combination")
        if "add" in acts and "remove" in acts and acts["add"] & acts["remove"]:
            _bad("override combination")
    return config


class _Graph:
    def __init__(self, parents, barriers, max_depth, max_nodes):
        self.parents = parents
        self.barriers = barriers
        self.max_depth = max_depth
        self.max_nodes = max_nodes

    def reach(self, start):
        """Return one bounded closure; never cache quadratic graph-wide ancestry."""
        if start in self.barriers:
            return (frozenset(), False, False)
        visited = {start}
        frontier = [start]
        depth = 0
        capped = False
        while frontier:
            nxt = []
            for node in frontier:
                for p in self.parents.get(node, ()):
                    if p in self.barriers or p in visited:
                        continue
                    if depth >= self.max_depth or len(visited) >= self.max_nodes:
                        capped = True
                        continue
                    visited.add(p)
                    nxt.append(p)
            frontier = nxt
            depth += 1
        return (frozenset(visited), capped, self._has_cycle(start, visited))

    def _has_cycle(self, start, visited):
        state = {}  # 1 = on stack, 2 = done
        stack = [(start, iter(self.parents.get(start, ())))]
        state[start] = 1
        while stack:
            node, it = stack[-1]
            advanced = False
            for p in it:
                if p not in visited:
                    continue
                s = state.get(p)
                if s == 1:
                    return True
                if s is None:
                    state[p] = 1
                    stack.append((p, iter(self.parents.get(p, ()))))
                    advanced = True
                    break
            if not advanced:
                state[node] = 2
                stack.pop()
        return False


def resolve(genres, parents, memberships, config):
    """Resolve genre roots, artist roots and diagnostics deterministically."""
    config = validate_config(config, genres)
    present = _present_set(genres)
    if type(parents) is not list or type(memberships) is not list:
        raise ValueError("invalid genre rollup input: rows")

    universe = set()
    for g in genres:
        universe.add(g["genre_qid"])
    parent_map = {}
    for row in parents:
        if type(row) is not dict or not _is_qid(row.get("genre_qid")) or not _is_qid(
            row.get("parent_qid")
        ):
            raise ValueError("invalid genre rollup input: parents")
        parent_map.setdefault(row["genre_qid"], set()).add(row["parent_qid"])
    parent_lists = {
        k: sorted(v, key=_qkey) for k, v in parent_map.items()
    }
    for row in memberships:
        if (
            type(row) is not dict
            or not _is_qid(row.get("genre_qid"))
            or type(row.get("artist_mbid")) is not str
            or row.get("source") not in SOURCES
        ):
            raise ValueError("invalid genre rollup input: memberships")
        universe.add(row["genre_qid"])

    limits = config["limits"]
    threshold = limits["broad_multi_threshold"]
    graph = _Graph(
        parent_lists,
        frozenset(config["traversal_barriers"]),
        limits["max_depth"],
        limits["max_nodes"],
    )

    sort_of = {}
    anchor_root = {}
    root_anchors = {}
    for root in config["roots"]:
        if root.get("retired", False):
            continue
        rid = root["root_id"]
        sort_of[rid] = root["sort_order"]
        root_anchors[rid] = list(root["anchors"])
        for a in root["anchors"]:
            anchor_root[a] = rid

    # Most-specific subsumption between active roots.
    reaches_root = {}
    for rid, anchors in root_anchors.items():
        found = set()
        for a in anchors:
            for node in graph.reach(a)[0]:
                r = anchor_root.get(node)
                if r is not None:
                    found.add(r)
        reaches_root[rid] = found
    narrower_than = {rid: set() for rid in root_anchors}
    for n in root_anchors:
        for b in root_anchors:
            if n == b:
                continue
            if b in reaches_root[n] and n not in reaches_root[b]:
                narrower_than[n].add(b)

    # Multiple anchors can project an acyclic genre graph into a cyclic root
    # relation. Such roots cannot strictly subsume one another: removing all
    # members would turn reached roots into an invented coverage gap.
    nesting_reach = {}
    for rid in root_anchors:
        visited, pending = set(), list(narrower_than[rid])
        while pending:
            other = pending.pop()
            if other not in visited:
                visited.add(other)
                pending.extend(narrower_than[other])
        nesting_reach[rid] = visited
    narrower_than = {n: {b for b in broader if n not in nesting_reach[b]}
                     for n, broader in narrower_than.items()}

    overrides = {}
    for ov in config["overrides"]:
        overrides.setdefault(ov["genre_qid"], {})[ov["action"]] = set(
            ov.get("roots", [])
        )

    final = {}
    diagnostics = []
    for qid in sorted(universe, key=_qkey):
        # A disappeared/merged override target is inert, never invented from config.
        acts = overrides.get(qid, {}) if qid in present else {}
        visited, capped, cycle = ((frozenset(), False, False)
                                  if "exclude" in acts or "replace" in acts else graph.reach(qid))
        flags = set()
        if cycle:
            flags.add("cycle_detected")
        if capped:
            flags.add("traversal_capped")
        candidates = {anchor_root[n] for n in visited if n in anchor_root}
        reduced = {
            b
            for b in candidates
            if not any(b in narrower_than[n] for n in candidates if n != b)
        }
        if "exclude" in acts:
            roots = set()
        elif "replace" in acts:
            roots = set(acts["replace"])
        else:
            roots = (reduced | acts.get("add", set())) - acts.get("remove", set())
        if qid in overrides and qid not in present:
            flags.add("override_target_missing")
        reason = None
        if not roots:
            if "exclude" in acts:
                reason = "excluded"
            elif qid not in present:
                reason = "unknown_genre"
            elif qid not in parent_lists:
                reason = "no_parents"
            else:
                reason = "no_root_reached"
        elif len(roots) > threshold:
            flags.add("broad_multi")
        final[qid] = roots
        entries = sorted(flags)
        out = [("flag", f) for f in entries]
        if reason is not None:
            out.append(("reason", reason))
        for kind, code in out:
            diagnostics.append((_qkey(qid), code, {"genre_qid": qid, kind: code}))
    # Overrides targeting genres outside the universe still warn.
    for qid in overrides:
        if qid not in universe:
            diagnostics.append(
                (
                    _qkey(qid),
                    "override_target_missing",
                    {"genre_qid": qid, "flag": "override_target_missing"},
                )
            )
    diagnostics.sort(key=lambda t: (t[0], t[1]))

    genre_rollup = [
        {"genre_qid": qid, "root_id": rid}
        for qid in sorted(final, key=_qkey)
        for rid in sorted(final[qid], key=lambda r: sort_of[r])
    ]

    by_artist = {}
    for row in memberships:
        by_artist.setdefault(row["artist_mbid"], []).append(row)
    artist_roots = []
    for artist in sorted(by_artist):
        rows = by_artist[artist]
        if any(r["source"] == "manual" for r in rows):
            rows = [r for r in rows if r["source"] == "manual"]
        roots = set()
        for r in rows:
            roots |= final[r["genre_qid"]]
        for rid in sorted(roots, key=lambda x: sort_of[x]):
            artist_roots.append({"artist_mbid": artist, "root_id": rid})

    return {
        "genre_rollup": genre_rollup,
        "artist_roots": artist_roots,
        "diagnostics": [d[2] for d in diagnostics],
    }
