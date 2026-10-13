#!/usr/bin/env python3
"""Public-text hygiene lint: fail closed on internal identifiers.

Owner rule (threat-model section 1, CONTRIBUTING.md): this public repo
carries no TOG ids, internal hosts, or agent names in any public text.
This script enforces the file-content half: it scans tracked text files
and exits 1 on any match. Wire it into CI as part of ``ci-ok``.

Banned classes (rule id -> shape):

  tog-id
    ``TOG-`` followed by digits (e.g. the ``TOG-`` + ``<n>`` shape).
    The ``TOG-*`` star form used by rule statements is NOT an id and
    never matches (a ``*`` is not a digit).
  internal-host
    Host shapes that only exist inside our walls: private DNS suffixes
    (the bare names ``internal``, ``local``, ``lan``, ``intranet``,
    ``corp`` — see ``_INTERNAL_SUFFIX`` for the exact shape),
    RFC-1918 IPv4, the cloud metadata IP, ``workforce`` control-plane
    hosts, and absolute instance-UI links
    (``https`` + ``://`` + any host + ``/<PREFIX>/issues|agents|.../``).
  instance-path
    Relative instance-UI paths (``/<PREFIX>/issues|agents|projects|
    approvals|runs/``). These deep-link the internal tracker even with
    no hostname attached. Lowercase app routes (``/api/issues/``) do
    not match: the prefix must be 2-6 capitals.
  agent-name
    Agent-scheme deep-links (the ``agent`` scheme followed by ``://``
    plus an identifier), ``paperclip`` runner handles (the word
    ``paperclip``, then ``-`` or ``_``, then ``agent``), and
    ``PAPERCLIP_*_(AGENT|RUN|TASK|SESSION|CHAT)_ID`` variables carrying
    a UUID value. Bare variable names (no value) stay documentable;
    role titles (``CISO``) are not agent names.

Allowlist (exact and reviewable — a widened allowlist destroys the
property, so every entry names its reason):

  * ``host.docker.internal`` is a standard Docker dev hostname, not our
    infrastructure: it is masked before matching. (The mask pattern below
    spells the suffix via a group so this file itself stays clean.)
  * Lines stating the rule itself (they contain ``No tracker IDs``) are
    exempt from the tog-id rule only. Examples in those sentences MUST
    use the ``TOG-*`` star form, never a real id.
  * FIXTURE_EXCLUSIONS: exact test-fixture paths holding manufactured
    violations for this lint's own unit tests. No directory, extension,
    or generated-file exemptions.

Reports print file:line + rule id only — never the matched bytes. CI
logs on this repo are public (threat-model section 2), so the report
itself must not re-publish the leak.

Usage:
  python3 .github/scripts/public_text_lint.py [PATH ...]
  No PATHs: scan all git-tracked files. Explicit PATHs (e.g. new files
  not yet staged) are scanned as given.
"""

import re
import subprocess
import sys

# Exact lint-fixture paths only. No directory, extension or generated-file
# exemptions.
FIXTURE_EXCLUSIONS = frozenset(
    {
        ".github/scripts/test_public_text_lint.py",
    }
)

_TOG = re.compile(r"\bTOG-\d+\b")

_INTERNAL_SUFFIX = re.compile(r"\.(?:internal|local|lan|intranet|corp)\b", re.IGNORECASE)
_WORKFORCE_HOST = re.compile(r"\bworkforce\.[A-Za-z0-9.-]+", re.IGNORECASE)
_ABSOLUTE_UI_LINK = re.compile(
    r"\bhttps?://\S+/[A-Z]{2,6}/(?:issues|agents|projects|approvals|runs)/"
)
_PRIVATE_IPV4 = re.compile(
    r"(?:\b10\.\d{1,3}\.\d{1,3}\.\d{1,3}\b"
    r"|\b192\.168\.\d{1,3}\.\d{1,3}\b"
    r"|\b172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}\b"
    r"|\b169\.254\.169\.254\b)"
)
_RELATIVE_UI_PATH = re.compile(r"/[A-Z]{2,6}/(?:issues|agents|projects|approvals|runs)/")

_AGENT_DEEPLINK = re.compile(r"\bagent://\S+")
_RUNNER_HANDLE = re.compile(r"\bpaperclip[-_]agent\b", re.IGNORECASE)
_UUID_VALUED_ID = re.compile(
    r"\bPAPERCLIP_[A-Z_]*(?:AGENT|RUN|TASK|SESSION|CHAT)[A-Z_]*\s*[:=]\s*"
    r"['\"]?[0-9a-fA-F]{8}-"
)

RULES = (
    ("tog-id", (_TOG,)),
    ("internal-host", (_INTERNAL_SUFFIX, _WORKFORCE_HOST, _ABSOLUTE_UI_LINK, _PRIVATE_IPV4)),
    ("instance-path", (_RELATIVE_UI_PATH,)),
    ("agent-name", (_AGENT_DEEPLINK, _RUNNER_HANDLE, _UUID_VALUED_ID)),
)

# Lines stating the rule itself; exempt from the tog-id rule only.
TOG_SENTENCE_ALLOW = re.compile(r"No tracker IDs")

# Group form keeps this definitional line out of its own suffix rule.
_DOCKER_HOST_MASK = re.compile(r"host\.docker\.(?:internal)\b", re.IGNORECASE)


def find_violations(text):
    """Return [(line_number, rule_id)] for banned shapes in text.

    One hit per rule per line at most. Never returns matched bytes —
    callers report file:line + rule id only.
    """
    masked = _DOCKER_HOST_MASK.sub("host.docker.example", text)
    hits = []
    for lineno, line in enumerate(masked.splitlines(), start=1):
        for rule_id, patterns in RULES:
            if rule_id == "tog-id" and TOG_SENTENCE_ALLOW.search(line):
                continue
            if any(p.search(line) for p in patterns):
                hits.append((lineno, rule_id))
    return hits


def tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout.decode("utf-8", errors="replace")
    return [p for p in out.split("\0") if p and p not in FIXTURE_EXCLUSIONS]


def scan_path(path):
    with open(path, "rb") as fh:
        data = fh.read()
    if b"\0" in data:
        return []  # binary: not public text
    return find_violations(data.decode("utf-8", errors="replace"))


def main(argv):
    paths = argv[1:] or tracked_files()
    total_hits = 0
    per_rule = {}
    for path in paths:
        try:
            hits = scan_path(path)
        except (OSError, UnicodeError) as exc:
            print(f"::warning file={path}::public-text: unreadable, skipped ({exc})")
            continue
        for lineno, rule_id in hits:
            print(f"::error file={path},line={lineno}::public-text: {rule_id}")
            total_hits += 1
            per_rule[rule_id] = per_rule.get(rule_id, 0) + 1
    print(f"public-text: scanned {len(paths)} files, {total_hits} hits {per_rule}")
    return 1 if total_hits else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
