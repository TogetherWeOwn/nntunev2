"""Pure offline genre taxonomy configuration validation (stdlib only)."""

import re

QID_RE = re.compile(r"Q[1-9][0-9]{0,19}")
ROOT_ID_RE = re.compile(r"[a-z0-9_]{1,32}")
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
