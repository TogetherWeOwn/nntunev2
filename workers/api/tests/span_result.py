"""Validate synthetic span observations; this module does not execute a Worker.

The caller supplies decoded JSON objects, not a pass/fail flag. Identities are
non-empty strings. Both evaluators raise ValueError on any unexpected shape or
identity. A successful negative control proves the unscoped task observed root,
not merely that some unrelated check failed.

Positive: {root, tasks: [{before, mid, end, after}, ...], child:
{before, mid, end, after}}. At least two concurrent tasks are required; child
is the live-span future awaited outside its scope. Every after is root.
Negative: {root, before, mid, end, after}, all identities equal root.
"""


def _object(value, keys):
    if type(value) is not dict or value.keys() != set(keys):
        raise ValueError("unexpected observation shape")
    return value


def _identity(value):
    if type(value) is not str or not value or value.strip() != value:
        raise ValueError("invalid span identity")
    return value


def _scoped(value, root):
    _object(value, ("before", "mid", "end", "after"))
    before, mid, end, after = (_identity(value[key]) for key in
                               ("before", "mid", "end", "after"))
    if before == root or before != mid or before != end or after != root:
        raise ValueError("scope identity or restoration mismatch")
    return before


def evaluate_positive(value):
    """Require distinct, stable task and child spans and restored root."""
    _object(value, ("root", "tasks", "child"))
    root = _identity(value["root"])
    tasks = value["tasks"]
    if type(tasks) is not list or len(tasks) < 2:
        raise ValueError("at least two concurrent tasks are required")
    identities = [_scoped(task, root) for task in tasks]
    identities.append(_scoped(value["child"], root))
    if len(set(identities)) != len(identities):
        raise ValueError("scoped identities must be distinct")


def evaluate_negative(value):
    """Require an unscoped control to observe root at every checkpoint."""
    _object(value, ("root", "before", "mid", "end", "after"))
    root = _identity(value["root"])
    if any(_identity(value[key]) != root for key in ("before", "mid", "end", "after")):
        raise ValueError("negative control did not observe root throughout")
