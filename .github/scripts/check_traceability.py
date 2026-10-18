#!/usr/bin/env python3
"""M1 exit traceability gate: the matrix must cite every e2e spec.

``docs/m1-exit-traceability.md`` maps each M1 exit step to the ``e2e/``
spec(s) covering it. The mapping rots silently whenever a spec is added,
renamed, or retitled without updating the matrix, so this script diffs the
matrix against what Playwright actually sees and fails closed on drift:

  1. Missing citation: a spec file in ``e2e:list`` output that the matrix
     never cites (a new spec with no matrix row). The report names the file
     and up to five of its test titles.
  2. Stale path: a spec file the matrix cites that Playwright does not list
     (moved, renamed, or deleted), or any other cited ``e2e/`` file
     (helper, config, fixture) that no longer exists on disk. ``:line``
     suffixes are informational only and are not checked: line numbers move
     on every edit above them, while the file and title carry the signal.
  3. Stale title: a double-quoted string the matrix cites that matches no
     listed test title (a retitled test). Comparison is exact after
     whitespace normalization.

Inputs are the matrix markdown and a captured ``e2e:list`` run
(``npm --prefix e2e run e2e:list``), kept as a file so the check stays
pure-Python, offline, and unit-testable with no staging credentials:

  python3 .github/scripts/check_traceability.py \\
      --matrix docs/m1-exit-traceability.md --list-output /tmp/e2e-list.txt

Paths are resolved relative to --root (default: repo root, i.e. the current
directory). Exit 0 = matrix cites every spec, 1 = drift found,
2 = an input could not be read or parsed (fail closed, no traceback).

Matrix authoring rules this enforces (see the matrix "Re-verifying" section):

  * Every ``e2e/tests/*.spec.ts`` file must be cited somewhere in the
    matrix, including offline self-tests in the "not an M1 exit step"
    section. Cite files in backticks.
  * Double quotes are reserved for exact test titles. Any other quoted
    prose (e.g. a section name in another doc) fails unless it is listed
    in NON_TEST_QUOTES below with a comment naming what it refers to.
  * Fenced code blocks are not scanned, so re-verify commands stay inert.
"""

import argparse
import re
import sys
from pathlib import Path

# Quoted strings in the matrix that are cross-document section references,
# not test titles. Exact match only: if a test is ever titled exactly one of
# these, the title match below passes first and this set stays inert. Adding
# any other quoted prose to the matrix fails the check until it is either
# reworded or recorded here with a comment, so new quotes get a review.
NON_TEST_QUOTES = frozenset(
    {
        "Activation progression",  # e2e/README.md section
        "Evidence",  # e2e/README.md section (verdict format)
        "Staging promotion checklist",  # docs/ops.md section
        # docs/ops.md section (rollback evidence for future Worker lanes)
        "Future Worker deploy lanes: required rollback evidence",
    }
)

# `  [staging] › health.spec.ts:19:3 › describe chain › test title`
# Playwright may emit the path relative to the e2e root with a `tests/`
# prefix (`tests/health.spec.ts:19:3`); accept both, capture the filename.
LIST_LINE = re.compile(r"^.*›\s*(?:tests/)?([\w][\w.-]*\.spec\.ts):\d+:\d+\s*›\s*(.+?)\s*$")
TOTAL_LINE = re.compile(r"^Total:\s*(\d+) tests? in (\d+) files?\s*$")

BACKTICK_SPAN = re.compile(r"`([^`]+)`")
# `e2e/tests/foo.spec.ts`, `e2e/tests/foo.spec.ts:27`, or bare `foo.spec.ts:27`
SPEC_CITE = re.compile(r"(?:e2e/tests/)?([\w][\w.-]*\.spec\.ts)(?::\d+)?")
# Trailing :line on any other cited path (`e2e/helpers/accounts.ts:233`)
LINE_SUFFIX = re.compile(r"^(.+?):\d+$")
QUOTED_SPAN = re.compile(r'"([^"\n]+)"')

MAX_TITLES_PER_UNCITED_FILE = 5


class TraceabilityError(Exception):
    """An input could not be read or parsed."""


def strip_fences(text):
    """Remove fenced code blocks so re-verify commands stay inert."""
    kept = []
    in_fence = False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if not in_fence:
            kept.append(line)
    return "\n".join(kept)


def normalize(title):
    return " ".join(title.split())


def parse_list_output(text, source="<list-output>"):
    """Return ({spec file: [leaf titles]}, total or None). Raises on no tests."""
    files = {}
    total = None
    for line in text.splitlines():
        match = LIST_LINE.match(line)
        if match:
            spec, chain = match.group(1), match.group(2)
            leaf = normalize(chain.split("›")[-1])
            files.setdefault(f"e2e/tests/{spec}", []).append(leaf)
            continue
        match = TOTAL_LINE.match(line.strip())
        if match:
            total = (int(match.group(1)), int(match.group(2)))
    if not files:
        raise TraceabilityError(
            f"{source}: parsed zero tests; "
            "is this really `npm run e2e:list` output?"
        )
    if total is not None and total != (
        sum(len(titles) for titles in files.values()),
        len(files),
    ):
        raise TraceabilityError(
            f"{source}: Total line says {total[0]} tests in {total[1]} files "
            f"but {sum(len(t) for t in files.values())} titles "
            f"in {len(files)} files were parsed; "
            "the --list format may have changed"
        )
    return files, total


def parse_matrix(text):
    """Return (cited spec files, cited e2e paths, cited titles).

    Scans per paragraph, not per line: the matrix prose is hard-wrapped, so
    a quoted title (or backticked path) split across two lines would never
    match the per-line spans and a retitled test would pass unchecked.
    Joining each paragraph's lines with a space keeps wrapped citations
    checkable; normalize() already collapses the extra whitespace.
    """
    prose = strip_fences(text)
    spec_files, other_paths, titles = set(), set(), set()
    paragraphs = re.split(r"\n\s*\n", prose)
    for paragraph in paragraphs:
        joined = " ".join(paragraph.splitlines())
        for span in BACKTICK_SPAN.findall(joined):
            # Undo a hard-wrap split inside a spaceless token: real paths
            # never contain whitespace, so `al\npha` joins as `al pha`
            # and must be compacted before matching.
            compact = re.sub(r"\s+", "", span)
            spec = SPEC_CITE.fullmatch(compact)
            if spec:
                spec_files.add(f"e2e/tests/{spec.group(1)}")
                continue
            # A trailing :line is informational (see module docstring).
            path = LINE_SUFFIX.sub(r"\1", compact)
            if (
                path.startswith("e2e/")
                and "*" not in path
                and not path.endswith("/")
                and "." in path.rsplit("/", 1)[-1]
            ):
                other_paths.add(path)
        for span in QUOTED_SPAN.findall(joined):
            candidate = normalize(span)
            if candidate:
                titles.add(candidate)
    return spec_files, other_paths, titles


def check(root, matrix_path, list_text):
    """Return [drift strings]. Raises TraceabilityError on bad inputs."""
    try:
        matrix_text = matrix_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise TraceabilityError(f"{matrix_path}: cannot read matrix ({exc})") from exc
    listed, _ = parse_list_output(list_text)
    cited_specs, cited_paths, cited_titles = parse_matrix(matrix_text)
    listed_titles = {title for titles in listed.values() for title in titles}

    drifts = []
    for spec in sorted(set(listed) - cited_specs):
        sample = ", ".join(f"{t!r}" for t in listed[spec][:MAX_TITLES_PER_UNCITED_FILE])
        extra = (
            f" (+{len(listed[spec]) - MAX_TITLES_PER_UNCITED_FILE} more)"
            if len(listed[spec]) > MAX_TITLES_PER_UNCITED_FILE
            else ""
        )
        drifts.append(
            f"uncited spec file {spec} ({len(listed[spec])} tests: "
            f"{sample}{extra}); cite it in the matrix"
        )
    for spec in sorted(cited_specs - set(listed)):
        drifts.append(
            f"stale spec citation {spec}: cited in the matrix but absent "
            "from `e2e:list` output (moved, renamed, or deleted)"
        )
    for path in sorted(cited_paths):
        if not (root / path).is_file():
            drifts.append(
                f"stale path citation `{path}`: cited in the matrix but "
                "missing on disk"
            )
    for title in sorted(cited_titles):
        if title in listed_titles or title in NON_TEST_QUOTES:
            continue
        drifts.append(
            f'stale title citation "{title}": cited in the matrix but no '
            "`e2e:list` test carries that title (retitled?)"
        )
    return drifts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--root", default=".", help="repo root (default: cwd)")
    parser.add_argument(
        "--matrix",
        default="docs/m1-exit-traceability.md",
        help="matrix path, relative to --root unless absolute",
    )
    parser.add_argument(
        "--list-output",
        required=True,
        help="captured `npm run e2e:list` output "
        "(relative to --root unless absolute)",
    )
    args = parser.parse_args(argv)
    root = Path(args.root)
    matrix = Path(args.matrix)
    list_output = Path(args.list_output)
    if not matrix.is_absolute():
        matrix = root / matrix
    if not list_output.is_absolute():
        list_output = root / list_output
    try:
        list_text = list_output.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        print(f"::error::traceability: cannot read list output ({exc})")
        return 2
    try:
        drifts = check(root, matrix, list_text)
    except TraceabilityError as exc:
        print(f"::error::traceability: {exc}")
        return 2
    for drift in drifts:
        print(f"::error::traceability: {drift}")
    listed, total = parse_list_output(list_text)
    print(
        f"traceability: checked {len(listed)} spec files "
        f"({sum(len(t) for t in listed.values())} tests) "
        f"against {matrix}, {len(drifts)} drifts"
    )
    return 1 if drifts else 0


if __name__ == "__main__":
    sys.exit(main())
