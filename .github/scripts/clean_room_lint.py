#!/usr/bin/env python3
"""Clean-room lint: fail closed on competitor-brand and extraction shapes.

AGENTS.md ("Clean-room, YouTube and MetaBrainz guardrails") states the
rule: competitor analysis output becomes written specs, and this repo is
written fresh from those specs. No competitor code, assets, brand names,
proprietary fonts or brand colours, no stream-extraction shapes, and no
references to where analysis evidence lives outside the repo. This
script enforces the mechanically checkable half; the checklist half
stays review discipline in AGENTS.md.

Banned classes (rule id -> shape). The brand token and the evidence
directory name are assembled from fragments below so this file does not
itself carry the contiguous shapes it bans:

  youtube-extraction
    `googlevideo`, `/youtubei/`, and `yt-dlp` / `yt_dlp` (the tool and
    its module name). These are direct-stream / extractor shapes; the
    only permitted YouTube playback is the official visible embed.
  competitor-brand
    The competitor's name as a word, any case. Covers asset and code
    references and branded font names containing it ("... Mix").
  competitor-font
    `Circular` with a capital C (plus common cut suffixes Sp/Std/TT).
    The capital keeps the lowercase generic English word usable.
  competitor-colour
    The two competitor brand greens as hex (`#1D` + `B954`,
    `#1E` + `D760`, `#` optional, any case).
  host-evidence-path
    An absolute host research path (`/home/<user>/research/...`) or a
    `research/<evidence-dir>` fragment. Matched by shape so the real
    path never has to be written here — writing it anywhere in this
    repo is exactly what the rule bans.

The same shapes are also checked against each tracked file's PATH
(reported as `<rule>-path` at line 1): a branded asset file must fail
on its name even when its bytes carry no text match.

Allowlist (exact and reviewable — a widened allowlist destroys the
property, so every entry names its reason):

  * DEFINITIONAL_EXCLUSIONS: the files that must name the banned things
    in order to state or test the prohibition (this script, its tests,
    and AGENTS.md, whose guardrail checklist names what implementers
    must not use). Exact paths, content scan only; path scan still runs.
  * SPIKE_ALLOWLIST: `workers/youtube/` and `docs/youtube-backend-spike.md`
    hold a pre-existing extraction spike that predates this gate. They
    are exempt from the `youtube-extraction` rule ONLY (brand, font,
    colour and evidence-path rules still apply there). This exemption
    is TEMPORARY: it exists until the spike is retired, and must be
    deleted in the same change that deletes the spike. Do not add new
    extraction code under cover of it.

Reports print file:line + rule id only — never the matched bytes (CI
logs on this repo are public). An input that cannot be read fails the
run: an incomplete scan is never a successful check.

Usage:
  python3 .github/scripts/clean_room_lint.py [PATH ...]
  No PATHs: scan all git-tracked files.
"""

import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from public_text_lint import _escape_annotation_property, _failure_category

# Files that state or test the prohibition and so must name the banned
# shapes. Exact paths only; see the docstring.
DEFINITIONAL_EXCLUSIONS = frozenset(
    {
        "AGENTS.md",
        ".github/scripts/clean_room_lint.py",
        ".github/scripts/test_clean_room_lint.py",
    }
)

# Pre-existing extraction spike, awaiting retirement. Exempt from the
# youtube-extraction rule only; every other rule still applies. Delete
# these entries together with the spike.
SPIKE_ALLOWLIST_PREFIX = "workers/youtube/"
SPIKE_ALLOWLIST_FILES = frozenset({"docs/youtube-backend-spike.md"})

# Fragment-assembled so no file in this repo — this one included —
# carries the contiguous brand word or the evidence directory name.
_BRAND = "spot" + "ify"
_EVIDENCE_DIR = "nntune-" + _BRAND

_EXTRACTION = re.compile(r"googlevideo|/youtubei/|yt[-_]dlp", re.IGNORECASE)
_BRAND_WORD = re.compile(rf"\b{_BRAND}\b", re.IGNORECASE)
_FONT = re.compile(r"\bCircular(?:Sp|Std|TT)?\b")
_COLOUR = re.compile(r"#?(?:1DB954|1ED760)\b", re.IGNORECASE)
_EVIDENCE = re.compile(
    rf"/home/[^\s/]+/research/|\bresearch/{re.escape(_EVIDENCE_DIR)}\b"
)

RULES = (
    ("youtube-extraction", _EXTRACTION),
    ("competitor-brand", _BRAND_WORD),
    ("competitor-font", _FONT),
    ("competitor-colour", _COLOUR),
    ("host-evidence-path", _EVIDENCE),
)


def _is_spike_path(path):
    return path.startswith(SPIKE_ALLOWLIST_PREFIX) or path in SPIKE_ALLOWLIST_FILES


def find_violations(text, path=""):
    """Return [(line_number, rule_id)] for banned shapes in text.

    One hit per rule per line at most; never returns matched bytes.
    Spike-allowlisted paths skip the extraction rule only.
    """
    spike = _is_spike_path(path)
    hits = []
    # Split on '\n' only, as in public_text_lint: the latin-1 decode in
    # scan_path can manufacture other Unicode line boundaries.
    for lineno, line in enumerate(text.split("\n"), start=1):
        for rule_id, pattern in RULES:
            if spike and rule_id == "youtube-extraction":
                continue
            if pattern.search(line):
                hits.append((lineno, rule_id))
    return hits


def find_path_violations(path):
    """Return [rule_id] for banned shapes in the path itself.

    A branded asset must fail on its file name even if its bytes are
    clean. Spike paths keep the same single-rule exemption.
    """
    spike = _is_spike_path(path)
    return [
        rule_id
        for rule_id, pattern in RULES
        if not (spike and rule_id == "youtube-extraction") and pattern.search(path)
    ]


def tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"], check=True, capture_output=True
    ).stdout.decode("utf-8", errors="replace")
    return [p for p in out.split("\0") if p]


def scan_path(path):
    hits = [(1, f"{rule_id}-path") for rule_id in find_path_violations(path)]
    if path in DEFINITIONAL_EXCLUSIONS:
        return hits
    with open(path, "rb") as fh:
        data = fh.read()
    # No binary early-return (same reasoning as public_text_lint): a
    # tracked binary is public once merged, so scan a lossless
    # latin-1 decode for embedded strings.
    return hits + find_violations(data.decode("latin-1"), path)


def main(argv):
    paths = argv[1:] or tracked_files()
    total_hits = 0
    per_rule = {}
    scanned = 0
    failed = 0
    for path in paths:
        try:
            hits = scan_path(path)
        except (OSError, UnicodeError) as exc:
            failed += 1
            print(
                f"::error file={_escape_annotation_property(path)}"
                f"::clean-room: {_failure_category(exc)}"
            )
            continue
        scanned += 1
        for lineno, rule_id in hits:
            print(
                f"::error file={_escape_annotation_property(path)},line={lineno}"
                f"::clean-room: {rule_id}"
            )
            total_hits += 1
            per_rule[rule_id] = per_rule.get(rule_id, 0) + 1
    print(
        f"clean-room: scanned {scanned} files, {failed} failed, "
        f"{total_hits} hits {per_rule}"
    )
    return 1 if total_hits or failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
