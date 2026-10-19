#!/usr/bin/env python3
"""Dependabot coverage check: every manifest directory must have an entry.

Dependabot takes no globs and each ecosystem looks only in its configured
directory, so a manifest without a matching entry silently gets no
automated security updates (workers/repair shipped exactly this way).
This script derives the expected (ecosystem, directory) set from the repo
itself and fails when `.github/dependabot.yml` does not cover it:

  cargo:  "/" plus every Cargo.toml directory that is neither the root
          nor a root workspace member (covers both the root `exclude`
          form and the own-`[workspace]` form: a crate with its own
          `[workspace]` table builds fine via `cargo metadata` yet would
          otherwise be invisible to the exclude list alone).
          Workspace members resolve to the root lockfile and need no
          entry of their own.
  docker: every directory containing a `Dockerfile`.
  npm:    every directory containing a `package.json`.
  github-actions: "/" (`.github/workflows`).

It also pins the lockfile header comment in `dependabot.yml`: every
`Cargo.lock` on disk (outside build/output dirs) must be named in the
header and negated back into tracking in the root `.gitignore`, so the
header cannot go stale again the way it did when the worker lockfiles
were committed.

It also pins every external `FROM` in every `Dockerfile` (outside the
same skip dirs) to a `tag@sha256:<64hex>` pin: a digest-less FROM keeps
Dependabot from emitting digest-refresh PRs, and the header comment in
`dependabot.yml` asserts the pins hold, so an unpinned FROM is drift.
Stage refs (`FROM base`, where `base` was named by an earlier
`FROM ... AS base`) are internal and exempt, as is `FROM scratch`
(the empty base image takes no tag and cannot be pinned). The match
is a regex, not literal digests, so Dependabot digest-refresh PRs
stay green. The scan mirrors the production parser (moby/buildkit):
one file-start BOM is stripped, physical lines split LF-only,
backslash-continued lines join before parsing, and heredoc bodies on
RUN/COPY/ADD -- attached (`<<EOF`) and spaced (`<< EOF`) openers alike
-- are content: a `FROM ... AS x` inside one registers no stage.

Stdlib only (no PyYAML: the two fields are extracted with a line scan).
Exit 0 = covered; 1 = drift found; 2 = unusable input.

Usage:
  python3 .github/scripts/check_dependabot_coverage.py [--root DIR]
"""

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

# Directories never searched for manifests: VCS metadata, dependency
# installs, build output, and local deploy state. A manifest under one of
# these is never a Dependabot directory.
SKIP_DIRS = frozenset({
    ".git", "node_modules", "target", ".wrangler", ".svelte-kit",
    "build", "dist", ".output", "test-results", "playwright-report",
    "__pycache__",
})

ECOSYSTEM_RE = re.compile(
    r"""^[ \t]*-[ \t]*package-ecosystem:[ \t]*["']?([^"'#\s]+)"""
)
DIRECTORY_RE = re.compile(
    r"""^[ \t]*directory:[ \t]*["']?([^"'#\s]+)"""
)
# Leading whitespace matches BuildKit's strings.TrimSpace (Unicode-wide: a
# pasted NBSP before FROM is still an instruction), while the FROM-to-ref
# separator is BuildKit's reWhitespace (space/tab/VT/FF/CR). NBSP and U+2028
# are not separators, so `FROM\xa0debian` is one token, not an instruction.
# Physical lines split LF-only (see _physical_lines), so \v \f and lone \r
# can appear mid-line but never split one.
FROM_RE = re.compile(r"^\s*FROM[\t\v\f\r ]+(.+)$", re.IGNORECASE)
# Trailing `AS <stage>` on a FROM line (Docker keywords case-insensitive).
FROM_STAGE_RE = re.compile(
    r"[ \t]+AS[ \t]+([A-Za-z][A-Za-z0-9_.-]*)[ \t]*(#.*)?$", re.IGNORECASE
)
# External image ref with a tag+digest pin: the tag before `@sha256` is
# required, so digest-only refs (`img@sha256:...`) are drift, not pins.
# (`#` cannot appear in a ref, so anything after it is a trailing comment
# and stripped before matching).
FROM_DIGEST_RE = re.compile(r"[^@\s]+:[^/:@\s]+@sha256:[0-9a-f]{64}")
# Line continuation with the default `\` escape token (BuildKit's
# lineContinuationRegex): a lone trailing backslash, trailing spaces/tabs
# allowed, joins the next line directly; `\\` at end of line is literal.
_CONT_RE = re.compile(r"(?:[^\\]\\|^\\\\)[ \t]*$")
# Full-line comment (BuildKit isComment: first non-space char is `#`,
# Unicode-wide). Inline `#` is literal and never stripped.
_COMMENT_RE = re.compile(r"^\s*#")
# Instructions whose args BuildKit scans for heredoc openers (its
# heredocDirectives plus ONBUILD-wrapped ones). `<<` anywhere else --
# another instruction, a comment, a JSON-form arg -- opens nothing.
_HEREDOC_INSTR_RE = re.compile(
    r"^\s*(?:ONBUILD[\t\v\f\r ]+)?(?:RUN|COPY|ADD)[\t\v\f\r ]+(.*)$",
    re.IGNORECASE,
)
# Heredoc opener word (BuildKit reHeredoc, minus its vacuous `\s*`: within
# one shell word the gap holds no unquoted whitespace -- spaced openers are
# glued by the lexer and handled via _HEREDOC_LONE_RE plus the gap test).
# Attached form only here: `<<<` never matches, `\d<<` is an fd redirect.
_HEREDOC_WORD_RE = re.compile(r"^\d*<<(-?)([^<]+)$")
# Lone `<<`/`N<<` word: opens with the NEXT word as delimiter when the gap is
# tab/CR/space (BuildKit processPossibleHeredoc glue), so `<< EOF`, `<<\tEOF`
# and `2<< EOF` open. NBSP/VT/FF/U+2028 gaps split the word instead
# (`<<\xa0EOF` opens nothing), and `<<- EOF` opens nothing: the dash blocks
# the glue, the space splits, and `<<-` alone has an empty rest, which
# heredocFromMatch rejects.
_HEREDOC_LONE_RE = re.compile(r"^\d*<<$")
_HEREDOC_GAP_RE = re.compile(r"[ \t\r]+")


def _strip_continuation(line):
    """`(content, continues)`, mirroring trimContinuationCharacter."""
    match = _CONT_RE.search(line)
    if not match:
        return line, False
    if match.start() == 0 and line.startswith("\\"):
        return "", True  # lone-backslash line contributes nothing
    return line[:match.start()] + line[match.start()], True


def _shell_words(text):
    """Split on Unicode whitespace honoring quotes, preserving quote/escapes.

    Mirrors BuildKit's heredoc word scan (shlex with RawQuotes+RawEscapes,
    whose word splitter is unicode.IsSpace): quote characters stay in the
    word, so a quoted `<<` never matches the opener pattern.

    Returns `(word, start, end)` spans (`text[start:end]` holds the word) so
    callers can inspect the gap between consecutive words: an attached
    opener's delimiter never spans words, while a spaced opener's gap decides
    whether the lexer glues the delimiter on (see _HEREDOC_GAP_RE).
    """
    words = []
    current = []
    start = 0
    quote = None
    started = False
    i = 0
    while i < len(text):
        ch = text[i]
        if quote is not None:
            current.append(ch)
            if ch == quote:
                quote = None
            i += 1
        elif ch in ("'", '"'):
            if not started:
                start = i
            quote = ch
            current.append(ch)
            started = True
            i += 1
        elif ch == "\\" and i + 1 < len(text):
            if not started:
                start = i
            current.append(text[i:i + 2])
            started = True
            i += 2
        elif ch.isspace():
            if started:
                words.append(("".join(current), start, i))
                current = []
                started = False
            i += 1
        else:
            if not started:
                start = i
            current.append(ch)
            started = True
            i += 1
    if started:
        words.append(("".join(current), start, len(text)))
    return words


def _heredoc_delimiter(rest):
    """Delimiter name for a heredoc opener tail, or None when it opens nothing.

    Mirrors BuildKit's heredocFromMatch: quotes group and are removed,
    backslash escapes collapse (literal inside single quotes); an unclosed
    quote or an empty name opens nothing.
    """
    out = []
    quote = None
    i = 0
    while i < len(rest):
        ch = rest[i]
        if quote == "'":
            if ch == "'":
                quote = None
            else:
                out.append(ch)
            i += 1
        elif ch == "\\" and quote != "'" and i + 1 < len(rest):
            out.append(rest[i + 1])
            i += 2
        elif quote == '"' and ch != '"':
            out.append(ch)
            i += 1
        elif ch in ("'", '"'):
            quote = ch if quote is None else None
            i += 1
        else:
            out.append(ch)
            i += 1
    if quote is not None:
        return None
    return "".join(out) or None


def _heredoc_openers(logical):
    """`(delimiter, chomp_tabs)` pairs opened by a logical instruction line.

    Only RUN/COPY/ADD (optionally ONBUILD-wrapped) qualify, after leading
    `--...` builder flags; JSON-form args (`RUN [".."]`) carry no heredocs.
    Attached openers (`<<EOF`, `<<-EOF`, `2<<EOF`) match per word; a lone
    `<<`/`N<<` word opens with the next word as delimiter when the gap is
    tab/CR/space like BuildKit's lexer glue (`<< EOF` opens; NBSP-gapped
    and `<<- EOF` forms do not).
    """
    match = _HEREDOC_INSTR_RE.match(logical)
    if not match:
        return []
    argline = match.group(1)
    spans = _shell_words(argline)
    index = 0
    while index < len(spans) and spans[index][0].startswith("--"):
        index += 1
        if spans[index - 1][0] == "--":
            break
    spans = spans[index:]
    if not spans:
        return []
    remainder = " ".join(word for word, _, _ in spans)
    if "<<" not in remainder:
        return []
    if remainder.startswith("["):
        try:
            parsed = json.loads(remainder)
        except ValueError:
            pass  # not JSON: shell form, heredocs apply
        else:
            # A JSON array is exec form (or a build error for non-strings):
            # either way BuildKit scans no heredocs. Any other valid JSON
            # is not exec form, so fall through to the word scan.
            if isinstance(parsed, list):
                return []
    openers = []
    k = 0
    while k < len(spans):
        word, _, end = spans[k]
        opener = _HEREDOC_WORD_RE.match(word)
        if opener:
            name = _heredoc_delimiter(opener.group(2))
            if name is not None:
                openers.append((name, opener.group(1) == "-"))
            k += 1
            continue
        if (
            _HEREDOC_LONE_RE.match(word)
            and k + 1 < len(spans)
            and _HEREDOC_GAP_RE.fullmatch(argline[end:spans[k + 1][1]])
        ):
            # In BuildKit the gap glues both words into one, so the delimiter
            # word is consumed even when it opens nothing: a `<` in it voids
            # the whole word (reHeredoc's rest is `[^<]*`).
            delimiter = spans[k + 1][0]
            k += 2
            if "<" not in delimiter:
                name = _heredoc_delimiter(delimiter)
                if name is not None:
                    openers.append((name, False))
            continue
        k += 1
    return openers


def fail_input(message):
    """Report an unusable input (exit 2): the check itself never ran."""
    print(f"dependabot-coverage: input error: {message}", file=sys.stderr)
    return 2


def normalize_directory(directory):
    """Canonical Dependabot directory form: leading slash, no trailing."""
    directory = directory.strip()
    if not directory.startswith("/"):
        directory = "/" + directory
    if len(directory) > 1:
        directory = directory.rstrip("/")
    return directory


VERSION_RE = re.compile(r"""^[ \t]*version:[ \t]*["']?2["']?[ \t]*(#.*)?$""")
UPDATES_RE = re.compile(r"""^[ \t]*updates:[ \t]*(#.*)?$""")
# Root `Cargo.lock` must match as a word, not as a substring of a nested
# lock path (every nested lock name contains `Cargo.lock`).
ROOT_LOCK_RE = re.compile(r"(?<![/\w])Cargo\.lock")


def validate_dependabot_structure(text):
    """Require the `version: 2` + `updates:` skeleton (exit-2 otherwise).

    The entry extractor is a line scan (no PyYAML), so a structurally
    broken file must not report OK: GitHub's Dependabot app check also
    validates this file, but the coverage gate stays fail-closed on its
    own. Returns an error string, or None when the skeleton is present.
    """
    has_version = any(VERSION_RE.match(line) for line in text.splitlines())
    has_updates = any(UPDATES_RE.match(line) for line in text.splitlines())
    if not has_version:
        return "dependabot.yml is missing 'version: 2'"
    if not has_updates:
        return "dependabot.yml is missing 'updates:'"
    return None


def parse_dependabot_entries(text):
    """Extract the (ecosystem, directory) set from dependabot.yml text."""
    entries = set()
    ecosystem = None
    for line in text.splitlines():
        eco = ECOSYSTEM_RE.match(line)
        if eco:
            ecosystem = eco.group(1).strip().strip("\"'")
            continue
        d = DIRECTORY_RE.match(line)
        if d and ecosystem:
            entries.add((ecosystem, normalize_directory(d.group(1))))
            ecosystem = None
    return entries


def header_comment(text):
    """Leading `#` comment block of dependabot.yml (the lockfile header)."""
    lines = []
    for line in text.splitlines():
        if line.startswith("#"):
            lines.append(line)
        elif not line.strip():
            continue
        else:
            break
    return "\n".join(lines)


def repo_relative(path, root):
    return "/" + path.relative_to(root).as_posix()


def walk_manifests(root, filename):
    """Dirs (as `/`-prefixed Dependabot dirs) holding `filename`."""
    found = set()
    for candidate in root.rglob(filename):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        parent = candidate.parent
        if parent == root:
            found.add("/")
        else:
            found.add("/" + parent.relative_to(root).as_posix())
    return found


GLOB_CHARS = frozenset("*?[]{}!")


def read_root_workspace(root):
    """Parse root `Cargo.toml` workspace members + exclude (input error else)."""
    try:
        data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, "root Cargo.toml not found"
    except (tomllib.TOMLDecodeError, OSError) as exc:
        return None, f"root Cargo.toml unreadable: {exc}"
    workspace = data.get("workspace", {})
    if not isinstance(workspace, dict):
        return None, "root Cargo.toml [workspace] is not a table"
    members = workspace.get("members", [])
    exclude = workspace.get("exclude", [])
    if not isinstance(members, list):
        return None, "root Cargo.toml workspace.members is not a list"
    if not isinstance(exclude, list):
        return None, "root Cargo.toml workspace.exclude is not a list"
    for entry in members:
        if isinstance(entry, str) and any(c in GLOB_CHARS for c in entry):
            return None, (
                f"root Cargo.toml workspace.members contains a glob {entry!r}: "
                "expand it to literal paths so the coverage check can resolve it"
            )
    return {"members": members, "exclude": exclude}, None


def standalone_crate_dirs(root):
    """Standalone cargo dirs: exclude list plus non-member manifest dirs.

    The exclude list alone misses the own-`[workspace]` form (a crate with
    its own `[workspace]` table builds fine yet is not in `exclude`), so
    every `Cargo.toml` directory that is neither the root nor a workspace
    member is also expected to have its own cargo entry.
    """
    workspace, err = read_root_workspace(root)
    if err is not None:
        return None, err
    dirs = set()
    for entry in workspace["exclude"]:
        if not isinstance(entry, str):
            continue
        if (root / entry / "Cargo.toml").is_file():
            dirs.add(normalize_directory(entry))
    members = set()
    for entry in workspace["members"]:
        if isinstance(entry, str):
            members.add(normalize_directory(entry))
    for manifest_dir in walk_manifests(root, "Cargo.toml"):
        if manifest_dir == "/":
            continue
        if manifest_dir in members:
            continue
        dirs.add(manifest_dir)
    return dirs, None


def expected_entries(root):
    """Derive the expected (ecosystem, directory) set from the repo."""
    standalone, err = standalone_crate_dirs(root)
    if err is not None:
        return None, err
    expected = {("cargo", "/")}
    expected |= {("cargo", d) for d in standalone}
    expected |= {("docker", d) for d in walk_manifests(root, "Dockerfile")}
    expected |= {("npm", d) for d in walk_manifests(root, "package.json")}
    if (root / ".github" / "workflows").is_dir():
        expected.add(("github-actions", "/"))
    return expected, None


def walk_dockerfiles(root):
    """All `Dockerfile` paths, relative POSIX form, outside SKIP_DIRS."""
    found = []
    for candidate in root.rglob("Dockerfile"):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        found.append(rel.as_posix())
    return sorted(found)


def _physical_lines(text):
    """Split Dockerfile text into physical lines exactly like BuildKit.

    BuildKit's scanLines splits only on `\\n` and trimNewline strips trailing
    `\\r`s; `str.splitlines()` also breaks on \\v \\f \\x1c-\\x1e \\x85
    \\u2028 \\u2029, which would expose a fake `FROM ... AS x` hidden after
    one of those inside a comment. A single trailing empty chunk (the file
    ends with `\\n`) is dropped, matching splitlines.
    """
    lines = [chunk.rstrip("\r") for chunk in text.split("\n")]
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def unpinned_from_refs(text):
    """`(lineno, ref)` for each external FROM lacking a tag+digest pin.

    Stage refs are exempt: a FROM whose ref was named by an earlier
    `FROM ... AS <stage>` in the same file is internal (stage names match
    case-insensitively, like Docker itself). `FROM scratch` is exempt: the
    empty base image takes no tag and cannot be pinned. Leading
    `--platform=...` flags are skipped to reach the ref. An empty or
    unparseable ref is reported, not skipped: the gate stays fail-closed.

    The scan mirrors the production parser (moby/buildkit): one file-start
    BOM is stripped, physical lines split LF-only, leading whitespace is
    Unicode-wide, separators are ASCII reWhitespace, `\\`-continued lines
    join before parsing, and heredoc bodies on RUN/COPY/ADD are content --
    neither stages nor pins are read inside them. Spaced `<< EOF` openers
    glue like the shell lexer; NBSP-gapped and `<<- `-spaced forms open
    nothing.
    """
    if text.startswith("\ufeff"):
        # BuildKit strips one file-start BOM before parsing (discardBOM).
        text = text[1:]
    problems = []
    stages = set()
    pending = []  # (delimiter, chomp_tabs) heredocs awaiting terminators
    physical = _physical_lines(text)
    i = 0
    while i < len(physical):
        if pending:
            name, chomp = pending[0]
            candidate = physical[i].lstrip("\t") if chomp else physical[i]
            if candidate == name:
                pending.pop(0)
            i += 1
            continue
        first = physical[i].lstrip()
        if first.startswith("#") or first == "":
            i += 1
            continue
        lineno = i + 1
        content, continued = _strip_continuation(first)
        chunks = [content]
        while continued:
            i += 1
            if i >= len(physical):
                break
            nxt = physical[i]
            # Comments and blanks inside a continuation are skipped,
            # never joined (BuildKit's inner scan loop).
            if _COMMENT_RE.match(nxt) or nxt.strip() == "":
                continue
            content, continued = _strip_continuation(nxt)
            chunks.append(content)
        i += 1
        logical = "".join(chunks)
        match = FROM_RE.match(logical)
        if match:
            rest = match.group(1).strip()
            staged = FROM_STAGE_RE.search(rest)
            stage_name = None
            if staged:
                stage_name = staged.group(1).lower()
                rest = rest[:staged.start()].strip()
            tokens = rest.split()
            ref = next((t for t in tokens if not t.startswith("--")), "")
            ref = ref.split("#", 1)[0].strip()
            if not ref:
                problems.append((lineno, logical.strip()))
            elif ref.lower() != "scratch" and ref.lower() not in stages:
                if not FROM_DIGEST_RE.fullmatch(ref):
                    problems.append((lineno, ref))
            if stage_name is not None:
                # Docker defines the name for later lines only: recording it
                # before the check would let `FROM debian AS debian` exempt
                # its own floating ref.
                stages.add(stage_name)
        for name, chomp in _heredoc_openers(logical):
            pending.append((name, chomp))
    return problems


def tracked_lockfiles(root):
    """All `Cargo.lock` paths, relative POSIX form, outside SKIP_DIRS."""
    locks = []
    for candidate in root.rglob("Cargo.lock"):
        try:
            rel = candidate.relative_to(root)
        except ValueError:
            continue
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        locks.append(rel.as_posix())
    return sorted(locks)


def gitignore_negations(root):
    """Set of `!`-negated paths from the root `.gitignore`."""
    try:
        text = (root / ".gitignore").read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    negated = set()
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("!"):
            negated.add(line[1:].lstrip("/"))
    return negated


def check_coverage(root):
    """Full check; returns a list of drift strings (empty when covered)."""
    try:
        text = (root / ".github" / "dependabot.yml").read_text(encoding="utf-8")
    except FileNotFoundError:
        return None, "dependabot.yml not found: .github/dependabot.yml"
    except OSError as exc:
        return None, f"dependabot.yml unreadable: {exc}"
    struct_err = validate_dependabot_structure(text)
    if struct_err is not None:
        return None, struct_err
    actual = parse_dependabot_entries(text)
    expected, err = expected_entries(root)
    if err is not None:
        return None, err
    drift = []
    for ecosystem, directory in sorted(expected):
        if (ecosystem, directory) not in actual:
            drift.append(
                f"missing {ecosystem} entry for directory {directory!r} "
                f"(add it to .github/dependabot.yml)"
            )
    # Every cargo entry must point at a real manifest directory.
    for ecosystem, directory in sorted(actual):
        if ecosystem != "cargo":
            continue
        if directory == "/":
            continue
        if not (root / directory.lstrip("/") / "Cargo.toml").is_file():
            drift.append(
                f"stale cargo entry for directory {directory!r} "
                f"(no Cargo.toml there)"
            )
    # Pin the FROM lines: every external FROM must carry a tag+digest pin
    # so Dependabot emits digest-refresh PRs (see the dependabot.yml header).
    for dockerfile in walk_dockerfiles(root):
        try:
            # Bytes, not read_text: universal-newline translation would turn
            # a lone \r into \n before the LF-only split (BuildKit reads raw
            # bytes; the BOM survives utf-8 decoding for the scan to strip).
            docker_text = (root / dockerfile).read_bytes().decode("utf-8")
        except OSError as exc:
            return None, f"{dockerfile} unreadable: {exc}"
        for lineno, ref in unpinned_from_refs(docker_text):
            drift.append(
                f"{dockerfile}:{lineno}: external FROM {ref!r} lacks a "
                f"tag@sha256 pin (Dependabot cannot track it)"
            )
    # Pin the lockfile header: every Cargo.lock must be named in the
    # header comment and negated back into tracking in .gitignore.
    header = header_comment(text)
    locks = tracked_lockfiles(root)
    for lock in locks:
        # Nested lockfiles must be named by full relative path (a bare
        # `Cargo.lock` mention would match every lock's basename and let
        # the header go stale again); the root lock matches as a word so a
        # header naming only `workers/api/Cargo.lock` does not satisfy it.
        if lock == "Cargo.lock":
            named = ROOT_LOCK_RE.search(header) is not None
        else:
            named = lock in header
        if not named:
            drift.append(
                f"lockfile {lock} not named in the dependabot.yml header "
                f"(update the Lockfiles comment)"
            )
    negated = gitignore_negations(root)
    if negated is None:
        return None, "root .gitignore not found"
    for lock in locks:
        if lock not in negated:
            drift.append(
                f"lockfile {lock} not negated back into tracking in "
                f".gitignore (add '!/{lock}')"
            )
    # Every cargo-covered lockfile directory needs its own entry, except
    # the root: workspace members resolve to the root lockfile.
    for lock in locks:
        if lock == "Cargo.lock":
            continue
        lock_dir = "/" + lock.rsplit("/", 1)[0]
        if ("cargo", lock_dir) not in actual:
            drift.append(
                f"tracked lockfile {lock} has no cargo entry for {lock_dir!r}"
            )
    return drift, None


def main(argv):
    parser = argparse.ArgumentParser(
        description="Fail when a manifest directory lacks a Dependabot entry."
    )
    parser.add_argument("--root", default=".")
    args = parser.parse_args(argv)
    root = Path(args.root)
    drift, err = check_coverage(root)
    if err is not None:
        return fail_input(err)
    if drift:
        for item in drift:
            print(f"dependabot-coverage: drift: {item}")
        print(f"dependabot-coverage: DRIFT ({len(drift)} problem(s))")
        return 1
    print("dependabot-coverage: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
