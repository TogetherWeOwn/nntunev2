#!/usr/bin/env python3
"""Docs internal-link lint: fail on broken file links/anchors in changed docs.

Roadmap/matrix/ops cross-links rot silently: nothing else checks docs links,
so this script makes a broken internal link a CI failure instead of a
reader-facing surprise.

Scope (by design, so unrelated history never reds the build):

  * Only markdown files (``.md``, ``.markdown``, ``.mdown``) are scanned, and
    the CI lint step checks only the files changed in the PR (``--base-ref``
    diff). The CI self-test step additionally pins the whole checked-in tree
    clean, so a rename that breaks another file's link still fails
    (fail-safe): a green main means a clean tree, so only a PR that
    introduces breakage — on the source or the target side — can go red.
  * Only *internal* links are checked: relative file links
    (``cost-basis.md``, ``../ops.md#section``), repo-root absolute links
    (``/docs/ops.md``) and same-file anchors (``#section``). External links
    (any ``scheme:`` target or ``//host`` target, e.g. ``https://``,
    ``mailto:``) are out of scope, as are links that escape the repository
    root (GitHub-routed paths such as the advisory link in ``SECURITY.md``).
  * Only inline ``[text](target)`` links (including ``![alt](target)``
    images) are checked; there are no reference-style links in this repo.
    Link-like text inside fenced code blocks and inline code spans is
    ignored, so code samples can never fail the lint.
  * Anchors are verified against GitHub heading slugs plus explicit
    ``<a name/id>`` and ``id=`` attributes, but only for markdown targets;
    an anchor on a non-markdown target is unverifiable and skipped.

Test-data allowlist (exact and reviewable): anything under
``.github/scripts/fixtures/`` is lint test data holding intentionally
broken links and is never scanned, mirroring the fixture exclusion in
``public_text_lint.py``.

An input that cannot be read fails the run (nonzero exit): an incomplete
scan is never reported as a successful check.

Usage:
  python3 .github/scripts/docs_link_lint.py [--root DIR] [--base-ref REF]
      [--files PATH ...]
  --files: check exactly these markdown files (resolved under --root).
  --base-ref: check markdown files changed between the base commit and the
      working tree (the CI mode; scoped so history never reds the build).
  Neither: scan every tracked markdown file under --root (local full
      check; untracked/ignored trees such as node_modules are skipped).
  Exit 0 = clean, 1 = broken links found or an input failed,
  2 = a usage or repository error (fail closed, no traceback).
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

MARKDOWN_SUFFIXES = (".md", ".markdown", ".mdown")

# Lint test data holding intentionally broken links: never scanned, in any
# mode (changed-file or full), so the lint's own fixtures cannot red it.
FIXTURE_PREFIX = Path(".github/scripts/fixtures")

_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
# Fences may sit inside list items at 4+ leading spaces (CommonMark counts
# indent relative to the item), so match any leading whitespace here. The
# opener's indent is recorded: a closer only counts when its indent is below
# opener_indent + 4 (a 4-space-indented line cannot close a column-0 fence),
# and an opener indented 4+ only counts when a matching closer exists later
# (otherwise it is an indented code block, and opening would swallow the
# rest of the file past the block).
_FENCE_OPEN = re.compile(r"^([ \t]*)(`{3,}|~{3,})(.*)$")
_FENCE_CLOSE = re.compile(r"^([ \t]*)(`{3,}|~{3,})[ \t]*$")
_INLINE_CODE = re.compile(r"`[^`\n]+`")
_SCHEME = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")
_HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*(?:#+\s*)?$")
_NAMED_ANCHOR = re.compile(
    r"<a\s+[^>]*(?:name|id)\s*=\s*[\"']([^\"']+)[\"']", re.IGNORECASE
)
_ID_ATTR = re.compile(
    r"<[a-zA-Z][^>]*\s+id\s*=\s*[\"']([^\"']+)[\"']", re.IGNORECASE
)


class LintError(Exception):
    """A usage or repository error (exit 2, fail closed)."""


def _clean_git_env():
    """Environment for git subprocesses, without an inherited repo location.

    A caller-exported GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE or
    GIT_COMMON_DIR would make git select a repository other than the
    explicit -C cwd, so drop them here.
    """
    env = dict(os.environ)
    env.pop("GIT_DIR", None)
    env.pop("GIT_WORK_TREE", None)
    env.pop("GIT_INDEX_FILE", None)
    env.pop("GIT_COMMON_DIR", None)
    return env


def _escape_annotation_property(value):
    """Render a filename safely for a workflow-command property value."""
    safe = value.encode("utf-8", "backslashreplace").decode("utf-8")
    escaped = []
    for char in safe:
        if char == "%":
            escaped.append("%25")
        elif char == ":":
            escaped.append("%3A")
        elif char == ",":
            escaped.append("%2C")
        elif ord(char) < 0x20 or ord(char) == 0x7F:
            escaped.append(f"%{ord(char):02X}")
        else:
            escaped.append(char)
    return "".join(escaped)


def _indent_width(indent):
    """Width of a leading-whitespace run (a tab advances to a multiple of 4)."""
    return len(indent.expandtabs(4))


def _has_matching_closer(lines, start, marker, opener_indent):
    """True when the opener's own closer appears before any other fence.

    The first later fence-marker line decides: when it sits at a similar
    indent (opener_indent - 3 <= width < opener_indent + 4) and is a valid
    bare closer (same character, at least as long), the opener is a real
    fence. Any other fence first — a column-0 block's opener/closer, an
    info-string opener of another block — means the indented line is an
    indented code block, not a fence. Lines at a similar indent that cannot
    close (info strings, other characters, shorter markers) are block
    content and are skipped.
    """
    for future in lines[start:]:
        opened = _FENCE_OPEN.match(future)
        if not opened:
            continue
        width = _indent_width(opened.group(1))
        if not opener_indent - 3 <= width < opener_indent + 4:
            return False
        close = _FENCE_CLOSE.match(future)
        if not close:
            continue
        if close.group(2)[0] != marker[0]:
            continue
        if len(close.group(2)) < len(marker):
            continue
        return True
    return False


def lines_outside_fences(text):
    """Yield (lineno, line) for lines outside CommonMark fenced code blocks.

    A closing fence must use the opener's character, be at least as long,
    carry only whitespace after the marker, and sit at an indent below
    opener_indent + 4 — so a ````` ``` ````` line inside a ```` ````md ````
    block, a ```` ```python ```` info-string line, or a 4-space-indented
    line inside a column-0 sample is content, not a closer. An opener
    indented 4+ only opens when a matching closer exists later; otherwise
    it is an indented code block and must not swallow the rest of the file.
    """
    lines = text.split("\n")
    fence = None
    fence_indent = 0
    for idx, line in enumerate(lines):
        lineno = idx + 1
        if fence is not None:
            close = _FENCE_CLOSE.match(line)
            if (
                close
                and close.group(2)[0] == fence[0]
                and len(close.group(2)) >= len(fence)
                and _indent_width(close.group(1)) < fence_indent + 4
            ):
                fence = None
            continue
        opened = _FENCE_OPEN.match(line)
        if opened:
            indent_str, marker, rest = opened.group(1), opened.group(2), opened.group(3)
            # A backtick fence's info string cannot contain a backtick:
            # such a line starts inline code, not a fenced block.
            if marker[0] == "`" and "`" in rest:
                yield lineno, line
                continue
            indent = _indent_width(indent_str)
            if indent >= 4 and not _has_matching_closer(
                lines, idx + 1, marker, indent
            ):
                yield lineno, line
                continue
            fence = marker
            fence_indent = indent
            continue
        yield lineno, line


def strip_fences(text):
    """Remove fenced code blocks, keeping inline code spans intact."""
    return "\n".join(line for _, line in lines_outside_fences(text))


def code_free_lines(text):
    """Yield (lineno, line) for lines outside fenced code blocks.

    Inline code spans are removed per line. Line numbers always refer to
    the original file, so error annotations point at the real line even
    when a fenced block precedes the violation.
    """
    for lineno, line in lines_outside_fences(text):
        yield lineno, _INLINE_CODE.sub("", line)


def strip_code(text):
    """Remove fenced code blocks and inline code spans from markdown."""
    return _INLINE_CODE.sub("", strip_fences(text))


def extract_link_targets(text):
    """Yield raw link targets from inline markdown links in code-free text."""
    for match in _LINK.finditer(text):
        raw = match.group(1).strip()
        if raw.startswith("<"):
            end = raw.find(">")
            if end == -1:
                continue
            target = raw[1:end].strip()
        else:
            target = raw.split()[0] if raw.split() else ""
        if target:
            yield target


def is_external(target):
    """True when a link target is outside the repo (out of scope)."""
    return bool(_SCHEME.match(target)) or target.startswith("//")


def split_target(target):
    """Split a link target into (path_part, anchor_or_None)."""
    if "#" in target:
        path_part, anchor = target.split("#", 1)
    else:
        path_part, anchor = target, None
    if "?" in path_part:
        path_part = path_part.split("?", 1)[0]
    return unquote(path_part.strip()), (unquote(anchor) if anchor is not None else None)


def github_slug(heading):
    """Render a heading the way GitHub slugs it for ``#fragment`` links.

    Inline formatting is reduced to its text (code spans, emphasis, links),
    the result is lowercased, every character that is not alphanumeric, a
    space, a hyphen or an underscore is dropped, and spaces become hyphens.
    """
    text = re.sub(r"!\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.replace("`", "")
    # Reduce emphasis to its text without touching intra-word underscores
    # (``some_name`` renders literally; ``_em_`` renders as ``em``).
    text = re.sub(r"(\*\*|__)(.+?)\1", r"\2", text)
    text = re.sub(r"(?<!\w)_([^_]+?)_(?!\w)", r"\1", text)
    text = re.sub(r"(?<!\w)\*([^*]+?)\*(?!\w)", r"\1", text)
    text = re.sub(r"~~(.+?)~~", r"\1", text)
    text = text.strip().lower()
    text = "".join(
        char for char in text if char.isalnum() or char in (" ", "-", "_")
    )
    return text.replace(" ", "-")


def anchors_in_markdown(text):
    """Return the set of linkable anchors (slugs + ids) in a markdown file."""
    anchors = set()
    counts = {}
    # Fences only: a heading's inline code still contributes its text to
    # the GitHub slug, so inline spans must survive here.
    for line in strip_fences(text).splitlines():
        match = _HEADING.match(line.lstrip())
        if match:
            slug = github_slug(match.group(2))
            seen = counts.get(slug, 0)
            anchors.add(f"{slug}-{seen}" if seen else slug)
            counts[slug] = seen + 1
    for match in _NAMED_ANCHOR.finditer(text):
        anchors.add(match.group(1))
    for match in _ID_ATTR.finditer(text):
        anchors.add(match.group(1))
    return anchors


def is_markdown_path(path):
    return path.suffix.lower() in MARKDOWN_SUFFIXES


def resolve_target(source_rel, path_part, root):
    """Resolve a link path against its source file.

    Returns the repo-relative target path, or None when the link escapes
    the repository root (a GitHub-routed path, out of scope).
    """
    if not path_part:
        return source_rel
    if path_part.startswith("/"):
        candidate = Path(path_part.lstrip("/"))
    else:
        candidate = Path(source_rel).parent / path_part
    parts = []
    for part in candidate.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                return None
            parts.pop()
        else:
            parts.append(part)
    return Path(*parts) if parts else Path(".")


def check_source(source_rel, root, anchor_cache):
    """Return [(lineno, target, reason)] violations for one markdown file."""
    try:
        text = (root / source_rel).read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise LintError(f"cannot read {source_rel}: {exc}") from exc
    violations = []
    for lineno, line in code_free_lines(text):
        for target in extract_link_targets(line):
            if is_external(target):
                continue
            path_part, anchor = split_target(target)
            if not path_part and anchor is None:
                continue
            resolved = resolve_target(source_rel, path_part, root)
            if resolved is None:
                # Escapes the repo root: a GitHub-routed path, not a file.
                continue
            if resolved == Path("."):
                target_path = root
            else:
                target_path = root / resolved
            if target_path.is_dir():
                if anchor is not None:
                    violations.append(
                        (lineno, target, "anchor on a directory link")
                    )
                continue
            if not target_path.is_file():
                violations.append((lineno, target, "target file does not exist"))
                continue
            if anchor is None or not is_markdown_path(target_path):
                continue
            if target_path not in anchor_cache:
                try:
                    anchor_cache[target_path] = anchors_in_markdown(
                        target_path.read_text(encoding="utf-8")
                    )
                except (OSError, UnicodeError) as exc:
                    raise LintError(
                        f"cannot read link target {resolved}: {exc}"
                    ) from exc
            if anchor not in anchor_cache[target_path]:
                violations.append(
                    (lineno, target, "anchor does not exist in target file")
                )
    return violations


def is_fixture_path(path):
    parts = Path(path).parts
    prefix = FIXTURE_PREFIX.parts
    return tuple(parts[: len(prefix)]) == tuple(prefix)


def _tracked_markdown_files(root):
    """Tracked markdown files under root, or None when root is outside git.

    Returns paths relative to root. Fixture roots (which hold intentionally
    broken links) are real subdirectories of the repo, so callers still
    apply the fixture filter; here we only scope the walk to versioned
    files so untracked/ignored trees (node_modules, build output) can never
    red a local full scan or the whole-tree self-test.
    """
    try:
        toplevel = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
            check=True,
            capture_output=True,
            text=True,
            env=_clean_git_env(),
        ).stdout.strip()
        out = subprocess.run(
            # --full-name: paths stay relative to the toplevel even when
            # root is a subdirectory (e.g. a fixture root), so the
            # relative_to(root) scoping below resolves correctly. No
            # suffix pathspecs: git pathspecs are case-sensitive, so
            # "*.md" would skip a tracked Guide.MD that the
            # suffix.lower() filter below treats as in scope. List
            # everything and filter suffixes in Python instead.
            ["git", "-C", str(root), "ls-files", "-z", "--full-name"],
            check=True,
            capture_output=True,
            env=_clean_git_env(),
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    if not toplevel:
        return None
    try:
        resolved_root = Path(root).resolve()
    except OSError:
        return None
    top = Path(toplevel)
    found = []
    for entry in out.split(b"\0"):
        if not entry:
            continue
        try:
            name = entry.decode("utf-8")
        except UnicodeDecodeError:
            continue
        try:
            rel = (top / name).relative_to(resolved_root)
        except ValueError:
            continue
        if rel.suffix.lower() not in MARKDOWN_SUFFIXES:
            continue
        if is_fixture_path(rel):
            continue
        found.append(rel)
    return sorted(found)


def markdown_files(root):
    """Every tracked markdown file under root except lint-test fixtures.

    Outside a git repo (throwaway test roots, fixture dirs in isolation)
    falls back to a directory walk that still skips .git, node_modules
    and target trees.
    """
    tracked = _tracked_markdown_files(root)
    if tracked is not None:
        return tracked
    found = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if ".git" in path.parts:
            continue
        if "node_modules" in path.parts or "target" in path.parts:
            continue
        if path.suffix.lower() not in MARKDOWN_SUFFIXES:
            continue
        rel = path.relative_to(root)
        if is_fixture_path(rel):
            continue
        found.append(rel)
    return found


def changed_markdown_files(root, base_ref):
    """Markdown files changed between the base commit and the working tree."""
    try:
        base_commit = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--verify", f"{base_ref}^{{commit}}"],
            check=True,
            capture_output=True,
            text=True,
            env=_clean_git_env(),
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        if base_ref != "main":
            raise LintError(f"cannot resolve base ref {base_ref!r}")
        try:
            base_commit = subprocess.run(
                ["git", "-C", str(root), "rev-parse", "--verify", "origin/main^{commit}"],
                check=True,
                capture_output=True,
                text=True,
                env=_clean_git_env(),
            ).stdout.strip()
        except (OSError, subprocess.CalledProcessError) as exc:
            raise LintError(f"cannot resolve base ref {base_ref!r}") from exc
    try:
        out = subprocess.run(
            ["git", "-C", str(root), "diff", "--name-only", "-z",
             "--diff-filter=ACMR", base_commit, "--"],
            check=True,
            capture_output=True,
            env=_clean_git_env(),
        ).stdout
    except (OSError, subprocess.CalledProcessError) as exc:
        raise LintError(f"cannot diff against base {base_ref!r}: {exc}") from exc
    changed = []
    for entry in out.split(b"\0"):
        if not entry:
            continue
        try:
            name = entry.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise LintError(f"cannot decode changed path: {exc}") from exc
        rel = Path(name)
        if rel.suffix.lower() not in MARKDOWN_SUFFIXES:
            continue
        if is_fixture_path(rel):
            continue
        if not (root / rel).is_file():
            continue
        changed.append(rel)
    return changed


def check_files(root, files):
    anchor_cache = {}
    violations = 0
    for rel in files:
        try:
            hits = check_source(rel, root, anchor_cache)
        except LintError as exc:
            print(f"::error file={_escape_annotation_property(rel.as_posix())}"
                  f"::docs-links: {exc}")
            return None
        for lineno, target, reason in hits:
            print(f"::error file={_escape_annotation_property(rel.as_posix())},"
                  f"line={lineno}::docs-links: {reason}: {target}")
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
                if rel.suffix.lower() not in MARKDOWN_SUFFIXES:
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
        print(f"docs-links: scanned {len(files)} files, {violations} violations")
        return 1 if violations else 0
    except LintError as exc:
        print(f"::error::docs-links: {exc}")
        return 2


if __name__ == "__main__":
    sys.exit(main())
