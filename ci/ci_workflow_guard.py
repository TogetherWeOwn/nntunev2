#!/usr/bin/env python3
"""Check GitHub Actions triggers against the shared CI demand policy.

Derived from t3-handoff/bin/ci-workflow-guard. Accept a repository root or
workflow directory. Use one top-level guard declaration per watched workflow.
"""

import argparse
from fnmatch import fnmatchcase
from pathlib import Path
import re
import sys

import yaml


PR_EVENTS = {"pull_request", "pull_request_target"}
PATH_EVENTS = PR_EVENTS | {"push"}
WATCHED_EVENTS = PATH_EVENTS | {"workflow_run"}
GUARD = re.compile(r"(paths|(?:required|release):[^\s|][^|]*)")


class WorkflowLoader(yaml.BaseLoader):
    """Keep GitHub's unquoted 'on' key as text and reject ambiguous keys."""

    def construct_mapping(self, node, deep=False):
        mapping = super().construct_mapping(node, deep)
        if len(mapping) != len(node.value):
            raise yaml.YAMLError("duplicate workflow mapping key")
        if "<<" in mapping:
            raise yaml.YAMLError("YAML merge keys are unsupported by Actions")
        return mapping


def workflows(text):
    doc = yaml.load(text, Loader=WorkflowLoader)
    if not isinstance(doc, dict) or "on" not in doc:
        raise ValueError("expected a workflow mapping with an on trigger")
    events = doc["on"]
    if isinstance(events, str) and events:
        return {events: {}}
    if isinstance(events, list) and all(isinstance(event, str) for event in events):
        return dict.fromkeys(events, {})
    if isinstance(events, dict) and all(settings == "" or isinstance(settings, dict)
                                        for event, settings in events.items() if event in WATCHED_EVENTS):
        return {event: settings or {} for event, settings in events.items()}
    raise ValueError("invalid on trigger")


def pushes_main(settings):
    if "branches-ignore" in settings:
        return not any(fnmatchcase("main", pattern) for pattern in settings["branches-ignore"]
                       if not any(char in pattern for char in "?+\\"))
    if "branches" not in settings:
        return "tags" not in settings and "tags-ignore" not in settings

    matches = False
    for pattern in settings["branches"]:
        if any(char in pattern for char in "?+\\"):
            # Account conservatively for Actions glob syntax outside fnmatch.
            if not pattern.startswith("!"):
                matches = True
        elif fnmatchcase("main", pattern.lstrip("!")):
            matches = not pattern.startswith("!")
    return matches


def findings(text):
    events = workflows(text)
    watched = WATCHED_EVENTS & events.keys()
    if not watched:
        return

    guards = re.findall(r"^#[ \t]*guards:[ \t]*(.*)$", text, re.MULTILINE)
    guard = guards[0].strip() if len(guards) == 1 else ""
    if not GUARD.fullmatch(guard):
        yield "expected exactly one '# guards: required:<check> | release:<path> | paths' line"

    for event in sorted(watched):
        settings = events[event]
        types = settings.get("types", [])
        if isinstance(types, str):
            types = [types]
        if event in PR_EVENTS and "edited" in types:
            yield f"{event} types include 'edited'"
        if event == "workflow_run" and types != ["completed"]:
            yield "workflow_run must declare types: [completed]"
        if guard == "paths" and (event not in PATH_EVENTS or
                                 not (settings.get("paths") or settings.get("paths-ignore"))):
            yield f"{event} declares guards: paths without a paths filter"

    if watched & PR_EVENTS and "push" in watched and pushes_main(events["push"]) and not guard.startswith("release:"):
        yield "PR and main-push runs require a release or rollback dependency"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("roots", nargs="+", type=Path)
    failed = False
    for root in parser.parse_args().roots:
        directory = root / ".github/workflows" if (root / ".github").is_dir() else root
        try:
            paths = sorted(path for path in directory.iterdir() if path.suffix in {".yml", ".yaml"})
            if not paths:
                raise ValueError("no workflow YAML files found")
        except (OSError, ValueError) as exc:
            print(f"{directory}: {exc}", file=sys.stderr)
            failed = True
            continue
        for path in paths:
            try:
                errors = list(findings(path.read_text(encoding="utf-8-sig")))
            except (OSError, ValueError, TypeError, yaml.YAMLError) as exc:
                errors = [f"unreadable workflow ({exc})"]
            for error in errors:
                print(f"{path.name}: {error}")
                failed = True
    if not failed:
        print("OK")
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
