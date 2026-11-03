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
    The competitor's name as a token, any case. Tokens are split on
    underscores as well as punctuation, so `..._logo.svg`,
    `<NAME>_CLIENT_ID` and `... Mix` all match, while the name buried
    inside a longer alphanumeric token does not.
  competitor-font
    `Circular` with a capital C (plus common cut suffixes Sp/Std/TT),
    same token rule. The capital keeps the lowercase generic English
    word usable.
  competitor-colour
    The two competitor brand greens as complete hex literals: 6 digits
    (`#1D` + `B954`, `#1E` + `D760`) or 8 digits with any alpha suffix
    (`#RRGGBBAA`; `AARRGGBB` forms match too, via the opaque-alpha
    suffix check), plus split literals (`0x1D, 0xB9, 0x54` in any
    spacing/case). A longer hex run that merely contains the digits
    is not the colour and does not match.
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
logs on this repo are public). When the match is in the path itself,
the matched span is replaced with `*` characters before the path is
annotated, so the report locates the file without re-publishing the
banned bytes. An input that cannot be read fails the run: an
incomplete scan is never a successful check.

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

# Token edges: underscores separate tokens (asset names, env vars,
# identifiers), but letters and digits do not. `\b` alone would treat
# `spotify_logo` as one word and miss the brand token.
_L = r"(?<![A-Za-z0-9])"
_R = r"(?![A-Za-z0-9])"

_EXTRACTION = re.compile(r"googlevideo|/youtubei/|yt[-_]dlp", re.IGNORECASE)
_BRAND_WORD = re.compile(rf"{_L}{_BRAND}{_R}", re.IGNORECASE)
_FONT = re.compile(rf"{_L}Circular(?:Sp|Std|TT)?{_R}")
# Whole hex literals only: exactly 6 digits, or 8 with the alpha byte
# on either side (#RRGGBBAA, and AARRGGBB as in 0xFF1D...). A longer
# hex run merely containing the digits (a hash, an unrelated 8-digit
# colour whose last six digits happen to match) is not the colour.
_COLOUR = re.compile(
    r"(?<![0-9A-Fa-f])(?:(?:1DB954|1ED760)(?:[0-9A-Fa-f]{2})?"
    r"|FF(?:1DB954|1ED760))(?![0-9A-Fa-f])",
    re.IGNORECASE,
)
# Split literals: three adjacent hex bytes spelling the colour
# (0x1D, 0xB9, 0x54 in any spacing/case).
_GREENS = (("1D", "B9", "54"), ("1E", "D7", "60"))
_COLOUR_SPLIT = re.compile(
    "|".join(rf"0x{x}\s*,\s*0x{y}\s*,\s*0x{z}" for x, y, z in _GREENS),
    re.IGNORECASE,
)
_EVIDENCE = re.compile(
    rf"/home/[^\s/]+/research/|\bresearch/{re.escape(_EVIDENCE_DIR)}\b"
)

RULES = (
    ("youtube-extraction", _EXTRACTION),
    ("competitor-brand", _BRAND_WORD),
    ("competitor-font", _FONT),
    ("competitor-colour", _COLOUR),
    ("competitor-colour", _COLOUR_SPLIT),
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
    return sorted(
        {
            rule_id
            for rule_id, pattern in RULES
            if not (spike and rule_id == "youtube-extraction")
            and pattern.search(path)
        }
    )


def redact_path(path):
    """Replace every rule-matching span in a path with `*` characters.

    Only used for diagnostics: the report locates the file without
    re-publishing banned bytes into public CI logs. Applied to every
    path the runner prints, violations and read errors alike; a clean
    path comes back unchanged.
    """
    for _, pattern in RULES:
        path = pattern.sub(lambda m: "*" * (m.end() - m.start()), path)
    return path


def tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"], check=True, capture_output=True
    ).stdout.decode("utf-8", errors="replace")
    return [p for p in out.split("\0") if p]


# Content reads use a lossless latin-1 decode with no binary
# early-return (same reasoning as public_text_lint): a tracked binary
# is public once merged, so embedded strings are scanned too.


def main(argv):
    paths = argv[1:] or tracked_files()
    total_hits = 0
    per_rule = {}
    scanned = 0
    failed = 0
    for path in paths:
        # Diagnostic form only: report paths are redacted so a banned
        # path never re-publishes its own bytes (see redact_path).
        shown = _escape_annotation_property(redact_path(path))
        # Path hits are reported first: an unreadable file with a banned
        # name must show both findings, not just the read error.
        hits = [(1, f"{rule_id}-path") for rule_id in find_path_violations(path)]
        if path not in DEFINITIONAL_EXCLUSIONS:
            try:
                with open(path, "rb") as fh:
                    data = fh.read()
            except (OSError, UnicodeError) as exc:
                failed += 1
                print(
                    f"::error file={shown}::clean-room: "
                    f"{_failure_category(exc)}"
                )
            else:
                scanned += 1
                hits += find_violations(data.decode("latin-1"), path)
        else:
            scanned += 1
        for lineno, rule_id in hits:
            print(
                f"::error file={shown},line={lineno}::clean-room: {rule_id}"
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
