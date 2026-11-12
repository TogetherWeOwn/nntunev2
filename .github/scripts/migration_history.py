#!/usr/bin/env python3
"""Bounded Git provenance for source renames, never applied-row identities."""
import hashlib
import json
import pathlib
import re
import selectors
import subprocess
import time
import tomllib

FILE_RE = re.compile(r"[0-9]{4}_[A-Za-z0-9_]+\.sql\Z")
PATH_RE = re.compile(r"migrations/[0-9]{4}_[A-Za-z0-9_]+\.sql\Z")
SHA_RE = re.compile(r"[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
EDGE_KEYS = ("parent", "commit", "old_path", "new_path", "old_sha256", "new_sha256")
MAX_COMMITS = 512
MAX_EVENTS = 2048
MAX_FILES = 3000
MAX_INPUT = 2_000_000
MAX_OUTPUT = 8_000_000
MAX_BLOB = 2_000_000
MAX_TOTAL_OUTPUT = 128_000_000
MAX_STATES = 100_000
MAX_EVENT_REFERENCES = 250_000
GIT_TIMEOUT = 20


class HistoryError(ValueError):
    pass


def edge_key(edge):
    return tuple(edge[key] for key in EDGE_KEYS)


def validate_history(entries):
    """Validate source metadata separately from positive applied identities.

    A ledger has no commit column. Reused historical names owned by different
    entries (or targets) cannot safely be disambiguated there, so refuse them.
    Same-name occurrences within one lineage retain their full Git identities.
    """
    if not isinstance(entries, list) or any(not isinstance(e, dict) for e in entries):
        raise HistoryError("migration entries must be tables")
    if len(entries) > MAX_FILES:
        raise HistoryError("migration entry bound exceeded")
    current = {e.get("file"): i for i, e in enumerate(entries)
               if isinstance(e.get("file"), str)}
    owners, count = {}, 0
    for index, entry in enumerate(entries):
        if not isinstance(entry.get("renamed_from", []), list):
            raise HistoryError("renamed_from must be a list")
        for alias in entry.get("renamed_from", []):
            if isinstance(alias, dict) and isinstance(alias.get("file"), str):
                owners.setdefault(alias["file"], index)
    for index, entry in enumerate(entries):
        history = entry.get("source_rename_history", [])
        if not isinstance(history, list):
            raise HistoryError("source_rename_history must be a list of tables")
        seen = set()
        for edge in history:
            count += 1
            if count > MAX_EVENTS:
                raise HistoryError("source rename event bound exceeded")
            if (not isinstance(edge, dict) or set(edge) != set(EDGE_KEYS)
                    or any(not isinstance(edge[k], str) for k in EDGE_KEYS)):
                raise HistoryError("source rename edge requires exactly the six string fields")
            if any(not COMMIT_RE.fullmatch(edge[k]) for k in ("parent", "commit")):
                raise HistoryError("source rename commits must be full lowercase Git object IDs")
            if edge["parent"] == edge["commit"]:
                raise HistoryError("source rename commit cannot be its own parent")
            if any(not SHA_RE.fullmatch(edge[k]) for k in ("old_sha256", "new_sha256")):
                raise HistoryError("source rename hashes must be lowercase SHA-256")
            if (any(not PATH_RE.fullmatch(edge[k]) for k in ("old_path", "new_path"))
                    or edge["old_path"] == edge["new_path"]):
                raise HistoryError("source rename paths must be distinct flat migration paths")
            key = edge_key(edge)
            if key in seen:
                raise HistoryError("duplicate source rename edge")
            seen.add(key)
            for path in (edge["old_path"], edge["new_path"]):
                name = path.removeprefix("migrations/")
                if current.get(name, index) != index or owners.get(name, index) != index:
                    raise HistoryError("source rename name has ambiguous entry or target ownership")
                owners[name] = index


def parse_manifest(text):
    if len(text.encode("utf-8")) > MAX_INPUT:
        raise HistoryError("manifest input bound exceeded")
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        raise HistoryError("migration manifest does not parse") from exc
    if "source_rename_history" in data:
        raise HistoryError("source_rename_history belongs on its final migration entry")
    return validate_entries(data.get("migration", []))


def validate_entries(entries):
    validate_history(entries)
    names, aliases = set(), set()
    for entry in entries:
        if (not isinstance(entry.get("file"), str) or not FILE_RE.fullmatch(entry["file"])
                or not isinstance(entry.get("sha256"), str) or not SHA_RE.fullmatch(entry["sha256"])
                or entry.get("target") not in ("app", "mirror")):
            raise HistoryError("migration manifest entry is malformed")
        if entry["file"] in names:
            raise HistoryError("duplicate migration entry")
        names.add(entry["file"])
        renamed = entry.get("renamed_from", [])
        if not isinstance(renamed, list):
            raise HistoryError("renamed_from must be a list")
        for alias in renamed:
            if (not isinstance(alias, dict) or set(alias) != {"file", "sha256"}
                    or not isinstance(alias["file"], str) or not FILE_RE.fullmatch(alias["file"])
                    or alias["sha256"] != entry["sha256"]):
                raise HistoryError("renamed_from must be a byte-identical file/sha256 pair")
            if alias["file"] in aliases:
                raise HistoryError("duplicate migration alias")
            aliases.add(alias["file"])
    if names & aliases:
        raise HistoryError("migration alias collides with a current name")
    return entries


def bounded_metadata_json(value):
    """Bound canonical JSON before serializing any complete metadata object."""
    size = 0
    active = set()

    def add(amount):
        nonlocal size
        size += amount
        if size > MAX_INPUT:
            raise HistoryError("import metadata bound exceeded")

    def visit(item, depth):
        if depth > 100:
            raise HistoryError("import metadata nesting bound exceeded")
        if isinstance(item, str):
            # A JSON string needs at least one byte per character. Encode only
            # small chunks so an oversized value cannot cause a large copy.
            add(len(item) + 2)
            for start in range(0, len(item), 4096):
                chunk = item[start:start + 4096]
                if any(0xD800 <= ord(char) <= 0xDFFF for char in chunk):
                    raise HistoryError("import metadata contains non-scalar Unicode")
                add(len(json.dumps(chunk, ensure_ascii=True)) - 2 - len(chunk))
        elif isinstance(item, (dict, list)):
            identity = id(item)
            if identity in active:
                raise HistoryError("import metadata contains a cycle")
            active.add(identity)
            add(2 + max(0, len(item) - 1))
            if isinstance(item, dict):
                add(len(item))  # colons
                for key, child in item.items():
                    if not isinstance(key, str):
                        raise HistoryError("import metadata keys must be strings")
                    visit(key, depth + 1)
                    visit(child, depth + 1)
            else:
                for child in item:
                    visit(child, depth + 1)
            active.remove(identity)
        else:
            if type(item) not in (int, float, bool, type(None)):
                raise HistoryError("import metadata is not canonical JSON")
            if type(item) is int and item.bit_length() > MAX_INPUT * 3:
                raise HistoryError("import metadata bound exceeded")
            add(len(json.dumps(item, allow_nan=False)))

    try:
        visit(value, 0)
        return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)
    except (TypeError, ValueError, UnicodeError, RecursionError) as exc:
        if isinstance(exc, HistoryError):
            raise
        raise HistoryError("import metadata is not canonical JSON") from exc


def validate_fresh_import(source, candidate, baseline, sql_bytes):
    """Compare one candidate with a separately verified source entry.

    This dormant helper does not establish source authority or verify Git history;
    callers must do both independently before treating a match as evidence.
    """
    source_json = bounded_metadata_json(source)
    candidate_json = bounded_metadata_json(candidate)
    validate_entries([source])
    if not isinstance(baseline, list):
        raise HistoryError("baseline must be a list of entries")
    if len(baseline) >= MAX_FILES:
        raise HistoryError("migration entry bound exceeded")
    # Bound aggregate baseline data, including aliases, before copying or
    # handing it to the shared validators which traverse those lists.
    bounded_metadata_json(baseline)
    validate_entries(baseline + [candidate])
    if source_json != candidate_json:
        raise HistoryError("import metadata differs from source")
    if not isinstance(sql_bytes, bytes) or len(sql_bytes) > MAX_BLOB:
        raise HistoryError("import SQL blob is invalid or oversized")
    if hashlib.sha256(sql_bytes).hexdigest() != source["sha256"]:
        raise HistoryError("import SQL hash differs from source")


def read_input(path):
    try:
        with pathlib.Path(path).open("rb") as stream:
            raw = stream.read(MAX_INPUT + 1)
        if len(raw) > MAX_INPUT:
            raise HistoryError("input byte bound exceeded")
        return raw.decode("utf-8")
    except (OSError, UnicodeError) as exc:
        raise HistoryError("cannot read history input") from exc


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise HistoryError("duplicate API JSON key")
        result[key] = value
    return result


def api_coverage(files_text, metadata_text, base, head):
    """Require complete paginated input pinned to the event's base and head."""
    if max(len(files_text.encode()), len(metadata_text.encode())) > MAX_INPUT:
        raise HistoryError("API input byte bound exceeded")
    try:
        pages = json.loads(files_text, object_pairs_hook=unique_object)
        metadata = json.loads(metadata_text, object_pairs_hook=unique_object)
    except (ValueError, RecursionError) as exc:
        raise HistoryError("invalid API JSON") from exc
    if (not isinstance(metadata, dict) or not isinstance(metadata.get("base"), dict)
            or not isinstance(metadata.get("head"), dict)
            or metadata["base"].get("sha") != base or metadata["head"].get("sha") != head):
        raise HistoryError("API metadata does not match pinned event commits")
    count = metadata.get("changed_files")
    if type(count) is not int or not 0 <= count < MAX_FILES:
        raise HistoryError("API file count missing or at truncation bound")
    if (not isinstance(pages, list) or not pages
            or any(not isinstance(page, list) or len(page) > 100 for page in pages)
            or any(len(page) != 100 for page in pages[:-1])):
        raise HistoryError("API pagination is malformed or incomplete")
    files = [item for page in pages for item in page]
    if len(files) != count:
        raise HistoryError("API file list is truncated or duplicated")
    names, changes, pairs = set(), {}, set()
    for item in files:
        if not isinstance(item, dict):
            raise HistoryError("malformed API file record")
        name, status = item.get("filename"), item.get("status")
        if not isinstance(name, str) or name in names or status not in (
                "added", "removed", "modified", "renamed", "changed", "copied"):
            raise HistoryError("invalid or duplicate API file record")
        names.add(name)
        old = item.get("previous_filename")
        if status == "renamed":
            if not isinstance(old, str) or old == name:
                raise HistoryError("API rename lacks a distinct old path")
            pairs.add((old, name))
        elif old is not None:
            raise HistoryError("unexpected API previous_filename")
        for path in (name, old) if old is not None else (name,):
            validate_sql_path(path)
            if PATH_RE.fullmatch(path):
                if path in changes:
                    raise HistoryError("ambiguous net API migration changes")
                changes[path] = ("D" if path == old or status == "removed" else
                                 "A" if status in ("added", "renamed", "copied") else "M")
    return changes, pairs


def validate_sql_path(path):
    if path.startswith("migrations/") and path.lower().endswith(".sql") and not PATH_RE.fullmatch(path):
        raise HistoryError("invalid migration path")


class EvidenceBudget:
    """One deadline/output allowance for acquisition, retries and both Git passes."""
    def __init__(self, *, deadline=None, total_output=0):
        self.deadline = time.monotonic() + 180 if deadline is None else deadline
        self.total_output = total_output

    def remaining(self, maximum):
        remaining = min(maximum, self.deadline - time.monotonic())
        if remaining <= 0:
            raise HistoryError("shared evidence deadline exceeded")
        return remaining

    def consume(self, size):
        self.total_output += size
        if self.total_output > MAX_TOTAL_OUTPUT:
            raise HistoryError("shared evidence output bound exceeded")


class GitHistory:
    def __init__(self, root, base, head, budget=None):
        if not all(COMMIT_RE.fullmatch(ref) for ref in (base, head)):
            raise HistoryError("base and head must be full pinned commit IDs")
        self.root, self.base, self.head = pathlib.Path(root), base, head
        self.budget = budget or EvidenceBudget()
        self.total_output, self.trees, self.hashes, self.diffs = 0, {}, {}, {}
        for ref in (base, head):
            if self.git("rev-parse", "--verify", f"{ref}^{{commit}}").decode().strip() != ref:
                raise HistoryError("missing pinned Git commit")
        bases = self.git("merge-base", "--all", base, head).decode().splitlines()
        if len(bases) != 1:
            raise HistoryError("missing or ambiguous Git merge base")
        self.merge_base = bases[0]
        raw = self.git("rev-list", "--reverse", "--topo-order", "--parents", f"{base}..{head}")
        rows = [line.split() for line in raw.decode().splitlines()]
        if len(rows) > MAX_COMMITS:
            raise HistoryError("Git commit bound exceeded")
        if any(len(row) < 2 for row in rows):
            raise HistoryError("missing or shallow Git parent history")
        self.commits = [(row[0], row[1:]) for row in rows]
        self.events, self.states = set(), {}

    def git(self, *args, limit=MAX_OUTPUT):
        """Bound stdout while reading, not after an unbounded capture_output."""
        deadline = time.monotonic() + self.budget.remaining(GIT_TIMEOUT)
        child = subprocess.Popen(["git", "--no-replace-objects", "-C", str(self.root),
                                  "-c", f"diff.renameLimit={MAX_FILES}", *args],
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        output = bytearray()
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not selector.select(remaining):
                        raise HistoryError("Git evidence collection timed out")
                    chunk = child.stdout.read1(min(65536, limit + 1 - len(output)))
                    if not chunk:
                        selector.unregister(child.stdout)
                        continue
                    output.extend(chunk)
                    self.total_output += len(chunk)
                    self.budget.consume(len(chunk))
                    if len(output) > limit or self.total_output > MAX_TOTAL_OUTPUT:
                        raise HistoryError("Git evidence output bound exceeded")
            if child.wait(timeout=max(0.01, deadline - time.monotonic())) != 0:
                raise HistoryError("Git evidence missing or unreadable")
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise HistoryError("Git evidence collection failed") from exc
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
            child.stdout.close()
        return bytes(output)

    def tree(self, commit):
        if commit not in self.trees:
            tree = {}
            for record in self.git("ls-tree", "-r", "-z", commit, "--", "migrations").split(b"\0"):
                if not record:
                    continue
                try:
                    meta, raw_path = record.split(b"\t", 1)
                    mode, kind, oid = meta.decode().split()
                    path = raw_path.decode("utf-8")
                except (ValueError, UnicodeError) as exc:
                    raise HistoryError("malformed Git tree") from exc
                validate_sql_path(path)
                if PATH_RE.fullmatch(path):
                    if mode != "100644" or kind != "blob" or not COMMIT_RE.fullmatch(oid):
                        raise HistoryError("migration Git object must be a regular non-executable blob")
                    tree[path] = oid
            if len(tree) > MAX_FILES:
                raise HistoryError("Git migration file bound exceeded")
            self.trees[commit] = tree
        return self.trees[commit]

    def sha(self, commit, path):
        oid = self.tree(commit).get(path)
        if oid is None:
            raise HistoryError("migration blob is missing")
        if oid not in self.hashes:
            self.hashes[oid] = hashlib.sha256(self.git("cat-file", "blob", oid, limit=MAX_BLOB)).hexdigest()
        return self.hashes[oid]

    def diff(self, parent, commit, renames=True):
        key = parent, commit, renames
        if key not in self.diffs:
            raw = self.git("diff-tree", "-r", "--no-commit-id", "--name-status", "-z",
                           "--find-renames=50%" if renames else "--no-renames", parent, commit)
            parts = raw.split(b"\0")
            if parts.pop() != b"":
                raise HistoryError("truncated Git diff")
            records, i, record_count = [], 0, 0
            try:
                while i < len(parts):
                    record_count += 1
                    if record_count > MAX_FILES:
                        raise HistoryError("Git change record bound exceeded")
                    status = parts[i].decode("ascii")
                    width = 2 if re.fullmatch(r"R[0-9]{1,3}", status) else 1
                    if width == 1 and status not in ("A", "D", "M", "T"):
                        raise HistoryError("unsupported Git change type")
                    paths = tuple(p.decode("utf-8") for p in parts[i + 1:i + 1 + width])
                    if len(paths) != width:
                        raise HistoryError("truncated Git change")
                    for path in paths:
                        validate_sql_path(path)
                    if any(PATH_RE.fullmatch(p) for p in paths):
                        if not all(PATH_RE.fullmatch(p) for p in paths) or status == "T":
                            raise HistoryError("migration crosses path or object-type boundary")
                        records.append((status, paths))
                    i += 1 + width
            except UnicodeError as exc:
                raise HistoryError("invalid Git path encoding") from exc
            if len(records) > MAX_FILES:
                raise HistoryError("Git change record bound exceeded")
            self.diffs[key] = records
        return self.diffs[key]

    def boundary(self, commit):
        return {path: (frozenset({("base", path)}), frozenset()) for path in self.tree(commit)}

    def collect(self):
        """Propagate commit/path identities, including ordinary edits and merges.

        At a merge, addition relative to one parent is not a second birth when
        another parent carries the lineage. Two independent births never unify.
        Every observed edge must ultimately belong to exactly one final entry.
        """
        state_count = reference_count = 0
        for commit, parents in self.commits:
            mappings = []
            for parent in parents:
                parent_state = self.states.get(parent)
                if parent_state is None:
                    parent_state = self.boundary(parent)
                changes, edges = {}, {}
                for status, paths in self.diff(parent, commit):
                    if status.startswith("R"):
                        old, new = paths
                        event = (parent, commit, old, new, self.sha(parent, old), self.sha(commit, new))
                        self.events.add(event)
                        if len(self.events) > MAX_EVENTS:
                            raise HistoryError("Git rename event bound exceeded")
                        changes[new], edges[new] = old, event
                    elif status == "A":
                        changes[paths[0]] = None
                    elif status == "D":
                        raise HistoryError("migration deletion has no provable rename lineage")
                mappings.append((parent_state, changes, edges))
            state = {}
            for path in self.tree(commit):
                roots, history = set(), set()
                for previous, changes, edges in mappings:
                    old_path = changes.get(path, path)
                    if old_path is not None and old_path in previous:
                        prior_roots, prior_history = previous[old_path]
                        roots.update(prior_roots)
                        history.update(prior_history)
                        if path in edges:
                            history.add(edges[path])
                if not roots:
                    roots.add((commit, path))
                state_count += 1
                reference_count += len(history)
                if state_count > MAX_STATES or reference_count > MAX_EVENT_REFERENCES:
                    raise HistoryError("Git lineage state bound exceeded")
                state[path] = frozenset(roots), frozenset(history)
            self.states[commit] = state
        return self.states.get(self.head, self.boundary(self.head))


def verify(root, base, head, manifest_text, coverage=None, *, budget=None, audit=None):
    """Return Git-derived edges by final entry; validate all claimed provenance.

    Common-baseline metadata was verified when introduced and is immutable.
    Target-only additions are not PR deletions. Original PR commits need not
    remain ancestors after a squash merge. Only additions are proved again;
    no historical applied aliases may be reclassified or removed.
    """
    entries = parse_manifest(manifest_text)
    history = GitHistory(root, base, head, budget=budget)
    base_text = history.git("show", f"{history.merge_base}:migrations/manifest.toml",
                            limit=MAX_INPUT).decode("utf-8")
    previous = {e["file"]: e for e in parse_manifest(base_text)}
    states = history.collect()
    owners, result = {}, {}
    chronology = {commit: i for i, (commit, _parents) in enumerate(history.commits)}
    for entry in entries:
        name, path = entry["file"], "migrations/" + entry["file"]
        if history.sha(head, path) != entry["sha256"]:
            raise HistoryError("final migration bytes do not match manifest")
        roots, events = states[path]
        old = previous.get(name, {})
        old_events = {edge_key(e) for e in old.get("source_rename_history", [])}
        claimed = {edge_key(e) for e in entry.get("source_rename_history", [])}
        old_aliases = {(a["file"], a["sha256"]) for a in old.get("renamed_from", [])}
        aliases = {(a["file"], a["sha256"]) for a in entry.get("renamed_from", [])}
        if not old_events <= claimed or not old_aliases <= aliases:
            raise HistoryError("alias and source history are append-only against base")
        if claimed - old_events - events:
            raise HistoryError("source rename evidence is forged, missing, unreachable or belongs to another entry")
        if (events or claimed - old_events or aliases - old_aliases) and len(roots) != 1:
            raise HistoryError("ambiguous independent Git lineages at merge")
        for alias in aliases - old_aliases:
            if not any(e[2] == "migrations/" + alias[0] and e[4] == alias[1] for e in events):
                raise HistoryError("new applied alias lacks genuine byte-identical historical provenance")
        for event in events:
            if event in owners and owners[event] != name:
                raise HistoryError("rename edge resolves to multiple current entries")
            owners[event] = name
            identical = event[4] == event[5] == entry["sha256"]
            applied = aliases | {(name, entry["sha256"])}
            if identical and (event[2].removeprefix("migrations/"), event[4]) not in applied:
                raise HistoryError("byte-identical rename lacks renamed_from applied alias")
            if not identical and event not in claimed:
                raise HistoryError("rename edge lacks source_rename_history coverage")
        result[name] = [dict(zip(EDGE_KEYS, event)) for event in
                        sorted(events, key=lambda e: (chronology[e[1]], e))]
    if set(owners) != history.events:
        raise HistoryError("rename has an unresolved terminal or missing tail to a final entry")
    # Retiring a base entry must not retire its source or applied history either.
    if set(previous) - {e["file"] for e in entries}:
        raise HistoryError("base migration entry removed")
    if coverage is not None:
        changes, pairs = coverage
        net = {}
        for status, paths in history.diff(history.merge_base, head, renames=False):
            net[paths[0]] = status
        if net != changes:
            raise HistoryError("API migration coverage differs from pinned Git net changes")
        for old, new in pairs:
            if PATH_RE.fullmatch(old) or PATH_RE.fullmatch(new):
                if not PATH_RE.fullmatch(old) or not PATH_RE.fullmatch(new) or new not in states:
                    raise HistoryError("net rename crosses boundary or has no final entry")
                # Net mappings are coverage, not fabricated chronological edges.
                if not any(e[2] == old for e in states[new][1]):
                    raise HistoryError("net rename has no verified source continuity")
    if audit is not None:
        audit.update(base=base, head=head, merge_base=history.merge_base,
                     commits=len(history.commits), events=len(history.events), outcome="verified")
    return result


def verify_preserved_source(root, base, head, manifest_sha256, *, budget):
    """Verify caller-supplied local Git history; this does not authorize imports.

    This is not repository-origin authorization, bounded acquisition, or an
    offline sandbox. Git inherits the caller's environment and configuration.
    The caller must establish identity and authorization independently and share
    one budget across acquisition and all verification passes. Baseline rename
    claims cannot be established by this verifier and are rejected.
    """
    if not isinstance(budget, EvidenceBudget):
        raise HistoryError("shared evidence budget required")
    if not isinstance(manifest_sha256, str) or not SHA_RE.fullmatch(manifest_sha256):
        raise HistoryError("invalid source manifest digest")
    source = GitHistory(root, base, head, budget=budget)
    if source.git("rev-parse", "--is-shallow-repository").strip() != b"false":
        raise HistoryError("shallow source repository cannot prove complete history")
    if source.merge_base != base:
        raise HistoryError("source base is not an ancestor of source head")
    baseline = source.git("show", f"{base}:migrations/manifest.toml", limit=MAX_INPUT)
    try:
        baseline_entries = parse_manifest(baseline.decode("utf-8"))
    except UnicodeError as exc:
        raise HistoryError("source baseline manifest is not UTF-8") from exc
    if any(entry.get("renamed_from") or entry.get("source_rename_history")
           for entry in baseline_entries):
        raise HistoryError("unverified baseline provenance")
    record = source.git("ls-tree", "-z", head, "--", "migrations/manifest.toml")
    match = re.fullmatch(rb"100644 blob ([0-9a-f]{40})\tmigrations/manifest\.toml\x00", record)
    if match is None:
        raise HistoryError("source manifest must be a regular non-executable blob")
    raw = source.git("cat-file", "blob", match.group(1).decode("ascii"), limit=MAX_INPUT)
    if hashlib.sha256(raw).hexdigest() != manifest_sha256:
        raise HistoryError("source manifest digest mismatch")
    try:
        text = raw.decode("utf-8")
    except UnicodeError as exc:
        raise HistoryError("source manifest is not UTF-8") from exc
    entries = parse_manifest(text)
    verify(root, base, head, text, budget=budget)
    budget.remaining(1)
    return entries
