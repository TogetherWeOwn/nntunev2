#!/usr/bin/env python3
"""Run bounded comparison-boundary mutants against the tested PR diff."""

import ast
from collections import Counter
from datetime import datetime, timezone
import importlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest


ROLLOUT_PATH = ".github/scripts/check_changed_lines_mutants.py"
MAIN_REF = "refs/remotes/origin/main"
WEEK = 7 * 24 * 60 * 60
LIMIT = 12
SCOPES = {
    "scripts/account_export.py": (),
    "scripts/genre_root_coverage.py": (
        "fixtures/genre_root_coverage/synthetic.json",
        "fixtures/genre_root_coverage/synthetic.expected.json",
    ),
}
BOUNDARIES = {b"<": b"<=", b"<=": b"<", b">": b">=", b">=": b">"}


class GateError(Exception):
    pass


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL, timeout=30)


def blob(root, sha, path, *, optional=False):
    entry = git(root, "ls-tree", "-z", sha, "--", path)
    if not entry and optional:
        return b""
    if not entry.startswith((b"100644 blob ", b"100755 blob ")):
        raise GateError(f"expected a tracked regular file at {path}")
    return git(root, "show", f"{sha}:{path}")


def changed_ranges(root, base, merge, path):
    patch = git(root, "diff", "--no-color", "--no-renames", "--no-ext-diff", "--no-textconv",
                "--text", "--unified=0", "--inter-hunk-context=0", base, merge, "--", path)
    return [range(int(line), int(line) + (int(count) if count else 1))
            for line, count in re.findall(rb"^@@ -[0-9,]+ \+([0-9]+)(?:,([0-9]+))? @@", patch, re.M)]


def comparisons(source):
    for node in ast.walk(ast.parse(source)):
        if (isinstance(node, ast.Compare) and len(node.ops) == 1
                and isinstance(node.ops[0], (ast.Lt, ast.LtE, ast.Gt, ast.GtE))):
            yield ast.dump(node), node


def select_mutants(before, after, ranges):
    existing = Counter(key for key, _ in comparisons(before))
    unchanged = Counter()
    changed = []
    for key, node in comparisons(after):
        if any(lines.start <= node.end_lineno and node.lineno < lines.stop for lines in ranges if lines):
            changed.append((key, node))
        else:
            unchanged[key] += 1

    lines = after.split(b"\n")
    candidates = []
    preexisting = unsupported = 0
    for key, node in changed:
        # When identical occurrences are edited together, skip the ambiguous
        # group instead of attributing an existing mutant to the PR.
        if existing[key] > unchanged[key]:
            preexisting += 1
            continue
        start, end = node.left.end_col_offset, node.comparators[0].col_offset
        segment = lines[node.lineno - 1][start:end]
        operator = segment.strip()
        if node.lineno != node.end_lineno or operator not in BOUNDARIES:
            unsupported += 1
            continue
        replacement = segment.replace(operator, BOUNDARIES[operator], 1)
        candidates.append((node.lineno, start, end, replacement))
    return lines, sorted(candidates), preexisting, unsupported


class TestResult(unittest.TestResult):
    def __init__(self, source):
        super().__init__()
        self.source = source
        self.test_file = str(Path(source).with_name("test_" + Path(source).name))
        self.tests = []

    def startTest(self, test):
        super().startTest(test)
        self.tests.append(test.id())

    def source_error(self, error):
        """Recognize source calls into libraries while excluding test callbacks."""
        trace = error[2]
        reached_source = False
        while trace:
            filename = trace.tb_frame.f_code.co_filename
            if reached_source and filename == self.test_file:
                return False
            reached_source |= filename == self.source
            trace = trace.tb_next
        return reached_source

    def addError(self, test, error):
        if self.source_error(error):
            self.addFailure(test, error)
        else:
            super().addError(test, error)

    def addSubTest(self, test, subtest, error):
        if error and self.source_error(error):
            self.addFailure(subtest, error)
        else:
            super().addSubTest(test, subtest, error)


def test_worker(test, receipt):
    sys.path.insert(0, str(Path.cwd()))
    suite = unittest.defaultTestLoader.loadTestsFromModule(importlib.import_module(test))
    result = TestResult(str(Path.cwd() / (test.removeprefix("test_") + ".py")))
    suite.run(result)
    invalid = result.errors or result.skipped or result.expectedFailures or result.unexpectedSuccesses
    if invalid or not result.tests:
        return 2
    Path(receipt).write_text(json.dumps([result.tests, len(result.failures)]), encoding="utf-8")
    return 0


def run_tests(directory, test, label):
    with TemporaryDirectory() as receipt_dir:
        receipt = Path(receipt_dir) / "result.json"
        command = [sys.executable, "-I", "-B", str(Path(__file__).resolve()),
                   "--test-worker", test, str(receipt)]
        env = {key: os.environ[key] for key in ("GITHUB_ACTIONS", "GITHUB_EVENT_NAME")}
        env.update(PATH=os.defpath, PYTHONDONTWRITEBYTECODE="1")
        try:
            run = subprocess.run(command, cwd=directory, timeout=15, env=env,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except subprocess.TimeoutExpired as exc:
            raise GateError(f"{label}: test runner timed out") from exc
        if run.returncode or not receipt.is_file():
            raise GateError(f"{label}: test runner failed, skipped a test, or ran no tests")
        return json.loads(receipt.read_text(encoding="utf-8"))


def evaluate(root, base, merge, enforcing, report=print, warn=None):
    names = git(root, "diff", "--no-renames", "--name-only", "-z", base, merge).split(b"\0")[:-1]
    report(f"Supported source paths: {', '.join(SCOPES)}")
    report("Supported mutations: single-line <, <=, > and >= comparisons with two operands")
    report(f"Changed paths outside scope: {sum(name not in SCOPES for name in map(os.fsdecode, names))}")
    remaining = LIMIT
    survivors = killed = 0
    for path, fixtures in SCOPES.items():
        ranges = changed_ranges(root, base, merge, path)
        if not any(ranges):
            report(f"{path}: no added or edited lines")
            continue
        try:
            after = blob(root, merge, path)
            lines, candidates, previous, unsupported = select_mutants(
                blob(root, base, path, optional=True), after, ranges)
        except (SyntaxError, UnicodeError) as exc:
            raise GateError(f"{path}: source parsing failed") from exc
        selected = candidates[:remaining]
        remaining -= len(selected)
        report(f"{path}: selected {len(selected)}, existing or ambiguous {previous}, "
               f"unsupported boundary layout {unsupported}, over budget {len(candidates) - len(selected)}")
        if not selected:
            continue

        with TemporaryDirectory() as directory:
            scratch = Path(directory)
            source = scratch / Path(path).name
            test = "test_" + source.stem
            for name in (f"{test}.py", *fixtures):
                destination = scratch / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(blob(root, merge, f"scripts/{name}"))
            source.write_bytes(after)
            inventory, failures = run_tests(scratch, test, f"{path} baseline")
            if failures:
                raise GateError(f"{path}: baseline tests failed")
            for line, start, end, replacement in selected:
                label = f"{path}:{line} ({lines[line - 1][start:end].strip().decode()} → {replacement.strip().decode()})"
                mutant = lines.copy()
                original = lines[line - 1]
                mutant[line - 1] = original[:start] + replacement + original[end:]
                source.write_bytes(b"\n".join(mutant))
                tested, failures = run_tests(scratch, test, label)
                if tested != inventory:
                    raise GateError(f"{label}: changed the test inventory")
                report(f"{'Killed by tests' if failures else 'Survived'}: {label}")
                if failures:
                    killed += 1
                else:
                    survivors += 1
    report(f"Mutation result: {killed} killed, {survivors} survived")
    if survivors and not enforcing and warn:
        warn(f"{survivors} new changed-line mutants survived (warning phase)")
    return int(bool(survivors) and enforcing)


def rollout_start(root, main_ref):
    timestamps = git(root, "log", "--first-parent", "--diff-filter=A", "--format=%ct",
                     main_ref, "--", ROLLOUT_PATH).splitlines()
    return int(timestamps[-1]) if timestamps else None


def rollout(start, now):
    if start is None:
        return False, "Warning phase pending its first commit on main"
    end = start + WEEK
    enforcing = now >= end
    utc = lambda stamp: datetime.fromtimestamp(stamp, timezone.utc).isoformat()
    return enforcing, f"{'Enforcement' if enforcing else 'Warning'} phase, UTC window {utc(start)} to {utc(end)}"


def main():
    event = os.environ.get("GITHUB_EVENT_NAME")
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise GateError("this gate runs only in GitHub Actions")

    def report(message, *, warning=False):
        print(("::warning::" if warning else "") + message, flush=True)
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            print(f"- {message}", file=summary)

    main_push = event == "push" and os.environ.get("GITHUB_REF") == "refs/heads/main"
    if event != "pull_request" and not main_push:
        raise GateError("expected a pull request or a main push")
    root = Path.cwd()
    base = os.environ["MUTATION_BASE_SHA"]
    merge = os.environ["MUTATION_MERGE_SHA"]
    if not re.fullmatch(r"[0-9a-f]{40}", merge) or git(root, "rev-parse", "HEAD").strip().decode() != merge:
        raise GateError("checkout does not match resolved tested commit")
    if event == "pull_request" and (not re.fullmatch(r"[0-9a-f]{40}", base)
            or git(root, "rev-parse", "HEAD^1").strip().decode() != base):
        raise GateError("resolved comparison base does not match the tested commit's first parent")
    # Use main's rollout clock for every target while keeping the mutation
    # diff pinned to the tested merge and its own first parent.
    start = rollout_start(root, merge if main_push else MAIN_REF)
    if main_push and start is None:
        raise GateError("cannot locate the rollout commit on main")
    enforcing, phase = rollout(start, datetime.now(timezone.utc).timestamp())
    report(phase)
    if main_push:
        report(f"Mutation skipped on main at {merge}: execution requires a pull request diff")
        return 0
    report(f"Tested diff: {base}..{merge}")
    return evaluate(root, base, merge, enforcing, report, lambda message: report(message, warning=True))


if __name__ == "__main__":
    if sys.argv[1:2] == ["--test-worker"]:
        if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("GITHUB_EVENT_NAME") != "pull_request":
            sys.exit(2)
        sys.exit(test_worker(*sys.argv[2:]))
    try:
        sys.exit(main())
    except GateError as exc:
        print(f"::error::Mutation gate infrastructure/test failure: {exc}", file=sys.stderr)
        sys.exit(2)
    except (KeyError, OSError, ValueError, subprocess.SubprocessError) as exc:
        print(f"::error::Mutation gate infrastructure/test failure ({type(exc).__name__})", file=sys.stderr)
        sys.exit(2)
