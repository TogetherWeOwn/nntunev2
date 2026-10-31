#!/usr/bin/env python3
"""Require a staging apply declaration on PRs that add or modify migrations.

App migrations reach `nntunev2_staging` out-of-band (docs/ops.md, "Database
roles and migrator wiring"), so a merged migration nobody applied leaves the
`staging-api` schema gate red. When the PR diff adds or modifies a numbered
SQL file directly under `migrations/`, the PR body must carry a
`## Staging migration apply` section with every field filled in and, for
`app` migrations, the apply checkbox checked. PRs that touch no numbered
migration pass silently.

The PR body is untrusted: it is read from the event JSON file
(GITHUB_EVENT_PATH or --event), never from argv or a shell expansion.
"""

import argparse
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from migrations_gate import NUMBERED_SQL, GateError, git, load_manifest  # noqa: E402


SECTION_HEADING = re.compile(r"^#{2,3}[ \t]*Staging migration apply[ \t]*#*[ \t]*$", re.IGNORECASE)
NEXT_HEADING = re.compile(r"^#{1,3}[ \t]")
FIELD_LINE = re.compile(r"^[ \t]*[-*][ \t]+(?!\[)([^:\n]+?)[ \t]*:[ \t]*(.*?)[ \t]*$")
CHECKBOX = re.compile(
    r"^[ \t]*[-*][ \t]+\[([ xX])\][ \t]+Applied to nntunev2_staging and verified\b",
    re.MULTILINE,
)
HTML_COMMENT = re.compile(r"<!--.*?(?:-->|\Z)", re.DOTALL)
FENCE = re.compile(r"^[ \t]*(```|~~~)")
PLACEHOLDERS = {"", "-", "?", "...", "tbd", "tba", "todo", "n/a", "na", "none"}

# Template field labels, matched case-insensitively with any "(...)" hint dropped.
FIELDS = ("migration files", "target", "role", "who applies", "rollback")


def report(message):
    print(f"::error title=staging migration apply::{message}")


def touched_migrations(root, base_ref, head_ref):
    """Numbered SQL files directly under migrations/ added or modified by the PR."""
    output = git(
        root, "diff", "--name-only", "-z", "--no-renames", "--diff-filter=AMT",
        f"{base_ref}...{head_ref}", "--", "migrations", text=False,
    )
    names = set()
    for raw in output.split(b"\0"):
        if not raw:
            continue
        path = Path(os.fsdecode(raw))
        if path.parent == Path("migrations") and NUMBERED_SQL.fullmatch(path.name):
            names.add(path.name)
    return sorted(names)


def read_body(event_path):
    try:
        event = json.loads(Path(event_path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError) as exc:
        raise GateError(f"cannot read event payload: {exc}") from exc
    pull_request = event.get("pull_request") if isinstance(event, dict) else None
    if not isinstance(pull_request, dict):
        raise GateError("event payload has no pull_request object")
    body = pull_request.get("body")
    return body if isinstance(body, str) else ""


def extract_section(body):
    """Return the section's lines, or None when the heading is absent.

    HTML comments are dropped first so template hints and commented-out
    lines never count. Fenced code blocks are skipped for the same reason.
    """
    lines = HTML_COMMENT.sub("", body.replace("\r\n", "\n")).split("\n")
    section = None
    in_fence = False
    for line in lines:
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if section is None:
            if SECTION_HEADING.match(line.strip()):
                section = []
            continue
        if NEXT_HEADING.match(line):
            break
        section.append(line)
    return section


def parse_section(section):
    fields = {}
    for line in section:
        match = FIELD_LINE.match(line)
        if not match:
            continue
        label = re.sub(r"\([^)]*\)", "", match.group(1))
        key = re.sub(r"\s+", " ", label).strip().strip("*_`").strip().lower()
        if key in FIELDS and key not in fields:
            fields[key] = match.group(2)
    checkbox = CHECKBOX.search("\n".join(section))
    checked = bool(checkbox) and checkbox.group(1) in "xX"
    return fields, checked


def is_placeholder(value):
    stripped = value.strip().strip("`*_").strip()
    return stripped.lower() in PLACEHOLDERS or re.fullmatch(r"<[^>]*>", stripped) is not None


def check(body, migrations, targets):
    """Return a list of problems; empty means the declaration is complete."""
    section = extract_section(body)
    if section is None:
        return [
            "PR adds or modifies " + ", ".join(migrations)
            + " but the body has no '## Staging migration apply' section "
            "(see .github/pull_request_template.md and docs/ops.md)"
        ]
    fields, checked = parse_section(section)
    problems = []
    missing = [label for label in FIELDS if is_placeholder(fields.get(label, ""))]
    if missing:
        problems.append("missing or empty fields: " + ", ".join(missing))

    files_value = fields.get("migration files", "")
    if "migration files" not in missing:
        unlisted = [name for name in migrations if name not in files_value]
        if unlisted:
            problems.append("'Migration files' does not list: " + ", ".join(unlisted))

    required_targets = sorted(set(targets.values()))
    if "target" not in missing:
        declared = set(re.findall(r"\b(app|mirror)\b", fields["target"].lower()))
        undeclared = [target for target in required_targets if target not in declared]
        if undeclared:
            problems.append(
                "'Target' must name " + ", ".join(undeclared)
                + " (per migrations/manifest.toml: "
                + ", ".join(f"{name}={targets[name]}" for name in migrations) + ")"
            )

    # Mirror migrations live on the shared MB cluster, not nntunev2_staging,
    # so only app migrations need the staging apply confirmed here.
    if "app" in required_targets and not checked:
        problems.append(
            "checkbox '- [x] Applied to nntunev2_staging and verified "
            "(preflight/verify output linked)' is missing or unchecked"
        )
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--base-ref", required=True)
    parser.add_argument("--head-ref", default="HEAD")
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_PATH"))
    try:
        args = parser.parse_args(argv)
        root = args.root.resolve()
        migrations = touched_migrations(root, args.base_ref, args.head_ref)
        if not migrations:
            return 0
        if not args.event:
            raise GateError("no event payload (set GITHUB_EVENT_PATH or --event)")
        # Unknown files fail closed as app; migrations-gate reports them unlisted.
        manifest = {entry["file"]: entry["target"] for entry in load_manifest(root)}
        targets = {name: manifest.get(name, "app") for name in migrations}
        problems = check(read_body(args.event), migrations, targets)
        for problem in problems:
            report(problem)
        if problems:
            return 1
        print("staging-migration-apply: declaration complete for " + ", ".join(migrations))
        return 0
    except GateError as exc:
        print(f"staging-migration-apply: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
