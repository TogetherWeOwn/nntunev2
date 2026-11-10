#!/usr/bin/env python3
"""Bounded, bracketed PR evidence; never a transactional API snapshot."""
import argparse
import json
import pathlib
import re
import selectors
import subprocess
import sys
import time

from migration_history import (COMMIT_RE, MAX_FILES, MAX_INPUT, MAX_OUTPUT,
                               EvidenceBudget, GitHistory, HistoryError,
                               api_coverage, read_input, unique_object)

MAX_ATTEMPTS = 3
REQUEST_TIMEOUT = 15
COLLECTION_TIMEOUT = 60
REPO_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")


def decode(text):
    try:
        return json.loads(text, object_pairs_hook=unique_object)
    except HistoryError:
        raise
    except (ValueError, RecursionError) as exc:
        raise HistoryError("invalid API JSON") from exc


def project_metadata(data):
    try:
        result = {"number": data["number"], "changed_files": data["changed_files"]}
        for side in ("base", "head"):
            value = data[side]
            repo = value["repo"]
            if (not COMMIT_RE.fullmatch(value["sha"])
                    or not isinstance(value["ref"], str) or not value["ref"]
                    or type(repo["id"]) is not int or repo["id"] <= 0
                    or not REPO_RE.fullmatch(repo["full_name"])):
                raise HistoryError("invalid PR repository/ref/commit identity")
            result[side] = {"sha": value["sha"], "ref": value["ref"],
                            "repo": {"id": repo["id"], "full_name": repo["full_name"]}}
        if type(result["number"]) is not int or result["number"] <= 0:
            raise HistoryError("invalid PR number")
        count = result["changed_files"]
        if type(count) is not int or not 0 <= count < MAX_FILES:
            raise HistoryError("API file count missing or at truncation bound")
        return result
    except (KeyError, TypeError) as exc:
        raise HistoryError("invalid PR repository/ref/commit identity") from exc


def event_metadata(event_file, base, head):
    event = decode(read_input(event_file))
    try:
        expected = project_metadata(event["pull_request"])
        if (expected["base"]["sha"] != base or expected["head"]["sha"] != head
                or event["number"] != expected["number"]
                or event["repository"]["id"] != expected["base"]["repo"]["id"]
                or event["repository"]["full_name"] != expected["base"]["repo"]["full_name"]):
            raise HistoryError("event identity differs from pinned commits/repository")
        return expected
    except (KeyError, TypeError) as exc:
        raise HistoryError("missing pinned event identity") from exc


def check_identity(metadata, expected):
    if metadata["number"] != expected["number"]:
        raise HistoryError("API PR identity differs from event")
    if metadata["head"] != expected["head"]:
        raise HistoryError("API metadata does not match pinned event commits/head identity")
    if any(metadata["base"][key] != expected["base"][key] for key in ("ref", "repo")):
        raise HistoryError("API base repository/ref retargeted")


def check_base(root, base, head, effective, budget):
    # Full checkout supplies immutable objects, never executable files from B1.
    # An inaccessible object/ancestry fails, rather than fetching a mutable ref.
    history = GitHistory(root, effective, head, budget=budget)
    if history.git("merge-base", base, effective).decode().strip() != base:
        raise HistoryError("effective base is not a descendant of event base/previous observation")


def request(path, budget):
    """Read each raw response with duplicate-key, byte and wall-time bounds."""
    deadline = time.monotonic() + budget.remaining(REQUEST_TIMEOUT)
    child = subprocess.Popen(["gh", "api", path], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise HistoryError("API request timed out")
                chunk = child.stdout.read1(min(65536, MAX_OUTPUT + 1 - len(output)))
                if not chunk:
                    selector.unregister(child.stdout)
                    continue
                output.extend(chunk)
                budget.consume(len(chunk))
                if len(output) > MAX_OUTPUT:
                    raise HistoryError("API response byte bound exceeded")
        if child.wait(timeout=max(0.01, deadline - time.monotonic())) != 0:
            raise HistoryError("API request failed; no evidence accepted")
        return decode(output.decode("utf-8"))
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as exc:
        raise HistoryError("API request failed") from exc
    finally:
        if child.poll() is None:
            child.kill()
        child.wait()
        child.stdout.close()


def compact(value):
    text = json.dumps(value, separators=(",", ":"))
    if len(text.encode()) > MAX_INPUT:
        raise HistoryError("input byte bound exceeded")
    return text


def collect(root, expected, budget):
    shared_deadline = budget.deadline
    collection_deadline = time.monotonic() + budget.remaining(COLLECTION_TIMEOUT)
    budget.deadline = min(shared_deadline, collection_deadline)
    try:
        pages, snapshot = collect_attempts(root, expected, budget, collection_deadline)
        snapshot["deadline"] = shared_deadline
        return pages, snapshot
    finally:
        budget.deadline = shared_deadline


def collect_attempts(root, expected, budget, collection_deadline):
    endpoint = f"repos/{expected['base']['repo']['full_name']}/pulls/{expected['number']}"
    observed_base = expected["base"]["sha"]
    for attempt in range(1, MAX_ATTEMPTS + 1):
        budget.remaining(COLLECTION_TIMEOUT)
        before = project_metadata(request(endpoint, budget))
        check_identity(before, expected)
        check_base(root, observed_base, expected["head"]["sha"], before["base"]["sha"], budget)
        observed_base = before["base"]["sha"]
        pages = []
        for page in range(1, MAX_FILES // 100 + 1):
            if time.monotonic() >= collection_deadline:
                raise HistoryError("API collection timed out")
            raw = request(f"{endpoint}/files?per_page=100&page={page}", budget)
            if not isinstance(raw, list) or len(raw) > 100 or any(not isinstance(v, dict) for v in raw):
                raise HistoryError("API pagination is malformed or incomplete")
            pages.append([{key: value[key] for key in ("filename", "status", "previous_filename")
                           if key in value} for value in raw])
            compact(pages)
            if len(raw) < 100:
                break
        else:
            raise HistoryError("API pagination reached truncation bound")
        after = project_metadata(request(endpoint, budget))
        check_identity(after, expected)
        check_base(root, observed_base, expected["head"]["sha"], after["base"]["sha"], budget)
        observed_base = after["base"]["sha"]
        # Structural defects are hard failures even during a moving bracket.
        # Validate against this attempt's declared initial count before retrying.
        api_coverage(compact(pages), compact(before), before["base"]["sha"], expected["head"]["sha"])
        if time.monotonic() >= collection_deadline:
            raise HistoryError("API collection timed out")
        if before != after:
            continue
        return pages, {"before": before, "after": after, "attempt": attempt,
                       "event": expected, "deadline": budget.deadline,
                       "total_output": budget.total_output}
    raise HistoryError("API snapshot changed during all bounded attempts")


def snapshot_coverage(root, base, head, files_text, snapshot_text, expected, budget):
    snapshot = decode(snapshot_text)
    try:
        before = project_metadata(snapshot["before"])
        after = project_metadata(snapshot["after"])
        if before != after or snapshot["event"] != expected:
            raise HistoryError("API snapshot brackets/event differ")
        if type(snapshot["attempt"]) is not int or not 1 <= snapshot["attempt"] <= MAX_ATTEMPTS:
            raise HistoryError("invalid snapshot attempt")
        check_identity(before, expected)
        effective = before["base"]["sha"]
        check_base(root, base, head, effective, budget)
        return effective, api_coverage(files_text, compact(before), effective, head)
    except (KeyError, TypeError) as exc:
        raise HistoryError("invalid API snapshot evidence") from exc


def resumed_budget(snapshot_text):
    snapshot = decode(snapshot_text)
    try:
        deadline, used = snapshot["deadline"], snapshot["total_output"]
        if (type(deadline) not in (int, float) or not time.monotonic() < deadline <= time.monotonic() + 180
                or type(used) is not int or used < 0):
            raise HistoryError("invalid or expired shared evidence budget")
        budget = EvidenceBudget(deadline=deadline, total_output=used)
        budget.consume(0)
        return budget
    except (KeyError, TypeError) as exc:
        raise HistoryError("missing shared evidence budget") from exc


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--event-file", required=True)
    parser.add_argument("--output-dir", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    try:
        budget = EvidenceBudget()
        expected = event_metadata(args.event_file, args.base_sha, args.head_sha)
        pages, snapshot = collect(args.root, expected, budget)
        (args.output_dir / "pr-files.json").write_text(compact(pages), encoding="utf-8")
        (args.output_dir / "pr-metadata.json").write_text(compact(snapshot), encoding="utf-8")
    except (HistoryError, OSError, UnicodeError) as exc:
        print(f"migrations-staging: {exc}", file=sys.stderr)
        return 1
    print(f"migrations-staging: acquired bounded API attempt {snapshot['attempt']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
