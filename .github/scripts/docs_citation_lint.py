#!/usr/bin/env python3
"""Docs citation lint: fail when a backticked path:line citation goes stale.

Markdown `[text](target)` links are covered by ``docs_link_lint.py``, but the
main evidence format in ``docs/security/threat-model.md`` and the worker
READMEs is backticked ``path:N`` / ``path:N-M`` citations inside inline code,
which that lint skips by design. A code PR that shifts cited lines leaves
these citations pointing at the wrong code with green CI. This script makes a
stale citation a CI failure instead of a reader-facing surprise.

For every backticked ``path:N`` / ``path:N-M`` citation in tracked markdown
(outside fenced code blocks, which hold samples):

1. the path must resolve to a file, most-local scope first: the doc's
   own directory, then its crate/worker root (nearest ``Cargo.toml`` /
   ``wrangler.toml`` / ``package.json`` ancestor, plus a ``src/`` fallback,
   since READMEs cite ``src/...`` and bare ``vault.rs``), then the repo
   root, then a same-file twin (a bare basename resolves to the one file
   already cited in full elsewhere in the same doc, e.g. ``lib.rs:21``
   after ``crates/nntune-config/src/lib.rs``);
2. every ``N``/``M`` must be a valid 1-based line range inside the file
   (comma-separated ``path:N-M,P-Q`` unions check each part);
3. when a backticked identifier anchors the citation by adjacency
   (``IDENT`` immediately before ``(citation)``, a citation immediately
   before ``(IDENT)``, an ``(IDENT, citation)`` paren pair, or
   ``IDENT in/from/at/per/via citation``; ``A``, ``B`` and ``C``
   enumerations anchor every link), at least one anchor's symbol (last
   ``::`` segment) must land within the cited line or range start,
   tolerating a one-line doc-comment lead. Looser co-mentions (``Sources:``
   lists, sentences that merely name nearby symbols) are not anchors:
   precision beats recall, so only tight couplings gate the build. A
   citation with no local anchor inherits the anchors of the identical
   citation (same resolved target and ranges) elsewhere in the same file,
   so a fixed twin cannot leave its bare sibling stale.

Out of scope (silently skipped): bare ``:N`` continuations, citations
without a resolvable path shape (``localhost:3000``, timestamps),
unresolved host/port or registry/image references with a recognized host
suffix (e.g. ``db.example.com:5432`` or ``ghcr.io/org/image:2``),
unbackticked prose citations, and citations embedded in longer backticked
spans. Anchors never cross table rows, list items, or blank lines, but do
span soft line wraps. Multi-word backticks are not anchors (except
``X = ...`` assignments and item declarations such as ``pub mod foo``).
Symbol matching is ASCII case-insensitive whole-word, so an informal
``empty_line`` anchor lands on ``EMPTY_LINE``. A unit that declares its
citations historical (``examined head``) skips them: point-in-time audit
maps pin old lines on purpose, with symbols preferred after main moved.

Usage mirrors docs_link_lint.py: --root/--base-ref/--files, exit 0 = clean,
1 = stale citations or an unreadable input, 2 = usage/repository error.
"""

import argparse
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from docs_link_lint import (  # noqa: E402
    LintError,
    _escape_annotation_property,
    changed_markdown_files,
    is_fixture_path,
    lines_outside_fences,
    markdown_files,
)

# The cited start must land on the anchor symbol, tolerating a one-line
# doc-comment lead (``vault.rs:51-52`` cites the ``///`` line above the
# const). A two-line drift already flags: the const block it was tuned
# against moved exactly two lines while three citations stayed behind.
LANDING_TOLERANCE_LINES = 1

_MANIFEST_NAMES = ("Cargo.toml", "wrangler.toml", "package.json")

_BACKTICK = re.compile(r"`([^`\n]+)`")
_CITATION = re.compile(r"^([^`\s:]+):(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)$")
_IP_PATH = re.compile(r"^[0-9.]+$")
_HOSTLIKE_HEAD = re.compile(
    r"^(?:[a-z0-9-]+\.)+(?:com|net|org|io|dev|internal|local|example)$",
    re.IGNORECASE,
)
_RANGES = re.compile(r"^(\d+)(?:-(\d+))?$")
_ANCHOR_ASSIGN = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*.*$")
_ANCHOR_PATH = re.compile(
    r"^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+$"
)
_ANCHOR_ITEM = re.compile(
    r"^(?:pub\s+)?(?:mod|struct|enum|fn|const|static|trait|type|let)"
    r"\s+([A-Za-z_][A-Za-z0-9_]*)$"
)
_ANCHOR_PLAIN = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)(\(\))?$")

_BULLET = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+")
_PAREN_IDENT_GAP = re.compile(r"^\s*[,;:]{0,1}\s*((in|from|at|per|via)\s+)?$")
_BARE_GLUE_GAP = re.compile(r"^\s+(in|from|at|per|via)\s+$")
_CHAIN_SEP = re.compile(
    r"^(?:\s*(?:,\s*)?(?:(?:and|or)\s+)?|\s*/\s*)$"
)
_HISTORICAL = re.compile(r"examined.head", re.IGNORECASE)


def parse_ranges(ranges_text):
    """Split "N" / "N-M" / comma unions into [(start, end)] int pairs."""
    parts = []
    for chunk in ranges_text.split(","):
        match = _RANGES.match(chunk)
        if not match:  # pragma: no cover - regex pre-validated by _CITATION
            continue
        start = int(match.group(1))
        end = int(match.group(2)) if match.group(2) else start
        parts.append((start, end))
    return parts


def is_citation_content(content):
    """True when a backticked span is a path:N / path:N-M citation."""
    match = _CITATION.match(content)
    if not match:
        return False
    path = match.group(1)
    if "/" not in path and "." not in path:
        return False
    if _IP_PATH.match(path):
        return False
    return True


def split_citation(content):
    """Split citation content into (path, [(start, end)])."""
    path, ranges_text = content.split(":", 1)
    return path, parse_ranges(ranges_text)


def extract_anchor(content):
    """Return the anchor symbol for a backticked span, or None.

    Paths, URLs, multi-word prose and bare punctuation are not anchors.
    ``X = ...`` anchors X, ``a::b`` anchors b, item declarations anchor
    the declared name, and a plain identifier anchors itself.
    """
    if "/" in content or "#" in content or "\\" in content:
        return None
    match = _ANCHOR_ASSIGN.match(content)
    if match:
        return match.group(1)
    if "::" in content:
        if _ANCHOR_PATH.match(content):
            return content.rsplit("::", 1)[1]
        return None
    match = _ANCHOR_ITEM.match(content)
    if match:
        return match.group(1)
    match = _ANCHOR_PLAIN.match(content)
    if match:
        return match.group(1)
    return None


def crate_root_for(doc_abs, root):
    """Nearest manifest ancestor of the doc, or None at the repo root."""
    directory = doc_abs.parent
    while True:
        for name in _MANIFEST_NAMES:
            if (directory / name).is_file():
                return directory
        if directory == root or root not in directory.parents:
            return None
        if directory.parent == directory:
            return None
        directory = directory.parent


def _candidate_targets(path_text, doc_rel, root):
    """Yield candidate absolute paths in resolution order.

    Most-local scope wins, so a crate README citing ``Cargo.toml`` gets
    its own manifest rather than the workspace root's; unambiguous
    root-relative citations fall through to the root unchanged.
    """
    text = path_text.lstrip("/")
    doc_dir = (root / doc_rel).parent
    try:
        crate = crate_root_for(root / doc_rel, root)
    except OSError:
        crate = None
    candidates = [doc_dir / text]
    if crate is not None:
        candidates.append(crate / text)
        candidates.append(crate / "src" / text)
    candidates.append(doc_dir / "src" / text)
    candidates.append(root / text)
    seen = set()
    for candidate in candidates:
        try:
            normalized = candidate.resolve()
        except OSError:
            continue
        try:
            normalized.relative_to(root)
        except ValueError:
            continue
        if normalized in seen:
            continue
        seen.add(normalized)
        yield normalized


def resolve_physical(path_text, doc_rel, root):
    """Resolve a citation path to an existing file, or None."""
    for candidate in _candidate_targets(path_text, doc_rel, root):
        try:
            if candidate.is_file():
                return candidate
        except OSError:
            continue
    return None


def _is_table_row(line):
    return line.lstrip().startswith("|")


def split_units(numbered_lines):
    """Group fence-stripped lines into anchor-analysis units.

    Each table row and each list item is its own unit so anchors never
    cross rows or bullets; other lines group into blank-delimited
    paragraphs (soft wraps join).
    """
    units = []
    current = []
    for lineno, line in numbered_lines:
        if _is_table_row(line):
            if current:
                units.append(current)
                current = []
            units.append([(lineno, line)])
        elif not line.strip():
            if current:
                units.append(current)
                current = []
        elif _BULLET.match(line):
            if current:
                units.append(current)
            current = [(lineno, line)]
        else:
            current.append((lineno, line))
    if current:
        units.append(current)
    return units


def unit_spans(unit):
    """Join a unit's lines and locate its backticked spans.

    Returns (text, [(start, end, content, lineno)]).
    """
    text = ""
    linenos = []
    for pos, (lineno, line) in enumerate(unit):
        if pos:
            text += " "
            linenos.append(lineno)
        for char in line:
            text += char
            linenos.append(lineno)
    spans = []
    for match in _BACKTICK.finditer(text):
        spans.append(
            (match.start(), match.end(), match.group(1), linenos[match.start()])
        )
    return text, spans


def _enclosing_group(text, pos):
    """Return (open_idx, close_idx) of the paren/bracket group at pos.

    Scans backtick-aware: backticked spans never open, close, or bound
    a group. Returns None when pos sits outside any group.
    """
    depth = 0
    idx = pos - 1
    in_code = False
    while idx >= 0:
        char = text[idx]
        if char == "`":
            in_code = not in_code
        elif not in_code:
            if char in ")]":
                depth += 1
            elif char in "([":
                if not depth:
                    break
                depth -= 1
        idx -= 1
    if idx < 0:
        return None
    opener = text[idx]
    closer = ")" if opener == "(" else "]"
    depth = 0
    in_code = False
    end = idx
    while end < len(text):
        char = text[end]
        if char == "`":
            in_code = not in_code
        elif not in_code:
            if char in "([":
                depth += 1
            elif char in ")]":
                depth -= 1
                if not depth and char == closer:
                    return idx, end
                if not depth:
                    return None
        end += 1
    return None


def _chain_idents(text, spans, idx, direction):
    """Collect ``A``, ``B`` and ``C``-style identifiers in an enumeration.

    Walks from spans[idx] while the gaps hold only commas, slashes, or
    and/or glue: ``zero_ttl... and over_long... (range)`` anchors both
    tests, and the range passes when either lands at its start. Citations
    and non-identifier spans break the chain.
    """
    symbols = []
    pos = idx
    while 0 <= pos + direction < len(spans):
        nxt = pos + direction
        if direction < 0:
            gap = text[spans[nxt][1] : spans[pos][0]]
        else:
            gap = text[spans[pos][1] : spans[nxt][0]]
        if not _CHAIN_SEP.match(gap):
            break
        content = spans[nxt][2]
        if is_citation_content(content):
            break
        symbol = extract_anchor(content)
        if not symbol:
            break
        symbols.append(symbol)
        pos = nxt
    return symbols


def local_anchors(text, spans, idx):
    """Anchor symbols adjacent to the citation span at spans[idx].

    Four tight couplings only: IDENT before ``(citation)``, a citation
    before ``(IDENT)``, an ``(IDENT, citation)`` pair (exactly one span
    precedes the citation in its group), and ``IDENT in/from/at/per/via
    citation``. ``A``, ``B`` and ``C`` enumerations anchor every link.
    """
    start, end, _, _ = spans[idx]
    anchors = set()
    if idx > 0:
        prev = spans[idx - 1]
        symbol = (
            extract_anchor(prev[2])
            if not is_citation_content(prev[2])
            else None
        )
        if symbol:
            gap = text[prev[1] : start]
            if gap.strip() == "(":
                anchors.add(symbol)
                anchors.update(_chain_idents(text, spans, idx - 1, -1))
            elif _BARE_GLUE_GAP.match(gap):
                anchors.add(symbol)
                anchors.update(_chain_idents(text, spans, idx - 1, -1))
            else:
                group = _enclosing_group(text, start)
                if group is not None:
                    inner = [
                        s
                        for s in spans
                        if group[0] < s[0] and s[1] <= start
                    ]
                    if (
                        len(inner) == 1
                        and inner[0][0] == prev[0]
                        and _PAREN_IDENT_GAP.match(gap)
                    ):
                        # No chaining here: the group holds exactly one
                        # span, and reaching outside it would be wrong.
                        anchors.add(symbol)
    if idx + 1 < len(spans):
        nxt = spans[idx + 1]
        symbol = (
            extract_anchor(nxt[2]) if not is_citation_content(nxt[2]) else None
        )
        if symbol:
            gap = text[end : nxt[0]]
            if gap.strip() == "(":
                anchors.add(symbol)
                anchors.update(_chain_idents(text, spans, idx + 1, 1))
    return anchors


def citation_occurrences(source_rel, root):
    """Yield occurrence dicts for every citation in one markdown file."""
    try:
        text = (root / source_rel).read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise LintError(f"cannot read {source_rel}: {exc}") from exc
    occurrences = []
    for unit in split_units(list(lines_outside_fences(text))):
        unit_text, spans = unit_spans(unit)
        if _HISTORICAL.search(unit_text):
            continue
        for idx, (start, end, content, lineno) in enumerate(spans):
            if not is_citation_content(content):
                continue
            path_text, parts = split_citation(content)
            occurrences.append(
                {
                    "lineno": lineno,
                    "citation": content,
                    "path": path_text,
                    "parts": parts,
                    "anchors": local_anchors(unit_text, spans, idx),
                }
            )
    return occurrences


def twin_path_targets(source_rel, root):
    """Map bare basenames to resolved files cited in full in this doc."""
    try:
        text = (root / source_rel).read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise LintError(f"cannot read {source_rel}: {exc}") from exc
    twins = {}
    for _, line in lines_outside_fences(text):
        for match in _BACKTICK.finditer(line):
            content = match.group(1)
            if " " in content or "\t" in content:
                continue
            if is_citation_content(content):
                candidate, _ = split_citation(content)
            elif "/" in content or "." in content:
                candidate = content
            else:
                continue
            if _IP_PATH.match(candidate):
                continue
            target = resolve_physical(candidate, source_rel, root)
            if target is None:
                continue
            twins.setdefault(target.name, set()).add(target)
    return twins


def check_source(source_rel, root, line_cache):
    """Return [(lineno, citation, reason)] violations for one file."""
    occurrences = citation_occurrences(source_rel, root)
    if not occurrences:
        return []
    twins = twin_path_targets(source_rel, root)
    resolved = {}
    ignored = set()
    for occurrence in occurrences:
        target = resolve_physical(occurrence["path"], source_rel, root)
        if target is None and "/" not in occurrence["path"]:
            candidates = twins.get(Path(occurrence["path"]).name, set())
            if len(candidates) == 1:
                target = next(iter(candidates))
        # A hostname/port or numeric image tag is not a missing source
        # file. Resolve first so real repository paths still get checked.
        if target is None and _HOSTLIKE_HEAD.fullmatch(
            occurrence["path"].split("/", 1)[0]
        ):
            ignored.add(id(occurrence))
        resolved[id(occurrence)] = target
    twin_anchors = {}
    for occurrence in occurrences:
        target = resolved[id(occurrence)]
        if target is None or not occurrence["anchors"]:
            continue
        key = (target, tuple(occurrence["parts"]))
        twin_anchors.setdefault(key, set()).update(occurrence["anchors"])
    violations = []
    for occurrence in occurrences:
        if id(occurrence) in ignored:
            continue
        lineno = occurrence["lineno"]
        citation = occurrence["citation"]
        target = resolved[id(occurrence)]
        if target is None:
            violations.append(
                (lineno, citation, "citation target does not resolve")
            )
            continue
        if target not in line_cache:
            try:
                line_cache[target] = target.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as exc:
                raise LintError(
                    f"cannot read citation target "
                    f"{target.relative_to(root)}: {exc}"
                ) from exc
        lines = line_cache[target]
        anchors = set(occurrence["anchors"])
        inherited = False
        if not anchors:
            key = (target, tuple(occurrence["parts"]))
            anchors = set(twin_anchors.get(key, set()))
            inherited = bool(anchors)
        for start, end in occurrence["parts"]:
            if start < 1 or end < start:
                violations.append((lineno, citation, "invalid citation range"))
                continue
            if start > len(lines) or end > len(lines):
                violations.append((lineno, citation, "cited line out of range"))
                continue
            if not anchors:
                continue
            # A single-line citation tolerates the same doc-comment lead as
            # a range start; a range additionally caps the window at its end
            # so a symbol below the cited span still flags.
            window_end = end if end > start else start + LANDING_TOLERANCE_LINES
            window = lines[start - 1 : min(window_end, start + LANDING_TOLERANCE_LINES)]
            landed = [
                symbol
                for symbol in sorted(anchors)
                if any(
                    re.search(
                        r"\b%s\b" % re.escape(symbol), line, re.IGNORECASE
                    )
                    for line in window
                )
            ]
            if not landed:
                wanted = ", ".join(sorted(anchors))
                if inherited:
                    wanted += " (twin-anchored)"
                violations.append(
                    (lineno, citation, f"citation does not land on {wanted}")
                )
    seen = set()
    unique = []
    for violation in violations:
        if violation not in seen:
            seen.add(violation)
            unique.append(violation)
    return unique


def check_files(root, files):
    line_cache = {}
    violations = 0
    for rel in files:
        try:
            hits = check_source(rel, root, line_cache)
        except LintError as exc:
            print(
                f"::error file={_escape_annotation_property(rel.as_posix())}"
                f"::docs-citations: {exc}"
            )
            return None
        for lineno, citation, reason in hits:
            print(
                f"::error file={_escape_annotation_property(rel.as_posix())},"
                f"line={lineno}::docs-citations: {reason}: {citation}"
            )
            violations += 1
    return violations


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--base-ref", default=None)
    parser.add_argument("--files", nargs="*", default=None)
    args = parser.parse_args(argv)
    try:
        root = args.root.resolve()
        if not root.is_dir():
            raise LintError(f"repository root is not a directory: {root}")
        if args.files is not None:
            files = []
            for name in args.files:
                rel = Path(name)
                if rel.is_absolute():
                    try:
                        rel = rel.resolve().relative_to(root)
                    except ValueError:
                        raise LintError(f"file is outside --root: {name}")
                if rel.suffix.lower() not in (".md", ".markdown", ".mdown"):
                    continue
                if is_fixture_path(rel):
                    continue
                if (root / rel).is_file():
                    files.append(rel)
        elif args.base_ref:
            files = changed_markdown_files(root, args.base_ref)
        else:
            files = markdown_files(root)
        violations = check_files(root, files)
        if violations is None:
            return 1
        print(f"docs-citations: scanned {len(files)} files, {violations} violations")
        return 1 if violations else 0
    except LintError as exc:
        print(f"::error::docs-citations: {exc}")
        return 2


if __name__ == "__main__":
    sys.exit(main())
