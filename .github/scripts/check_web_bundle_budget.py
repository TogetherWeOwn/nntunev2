#!/usr/bin/env python3
"""Client-JS bundle budget: fail when the web client build grows past budget.

Sums every ``*.js`` file under the SvelteKit client output
(``apps/web/.svelte-kit/output/client``, produced by ``pnpm build``) and
fails when the total exceeds the pinned budget below. Without this gate,
bundle bloat (a heavy dependency, an accidental full-library import) lands
silently — ``pnpm build`` itself asserts nothing about size.

Budget: ``BUDGET_CLIENT_JS_BYTES = 337_920`` (330 KiB, raw bytes, not gzip).
Measured 306,065 B (~299 KiB, 37 files) on 2026-10-27, so the budget leaves
~10% headroom for the next first-party features while catching any serious
addition (a charting or date library typically adds 50+ KiB). Growth since the
232,233 B baseline of 2026-10-23 is expected: first-party library features
only, no new runtime dependencies.

Admissions ship in the feature PR, never in a separate budget/literal PR.
Keep the base constant frozen. Add a uniquely named JSON decision under
``ci/budgets/web-client-js/`` with exactly these fields::

    {"unit": "web.client.js", "delta_bytes": 1000,
     "measured_before_bytes": 337000, "measured_after_bytes": 338000,
     "cause": "Feature/import cause; checked for accidental dependencies"}

Effective ceiling = frozen base + all decision deltas. CI rejects historical
edits/deletions and aggregates all new files per PR (max floor(base / 200)).
The seven-day allowance is floor(window-start ceiling / 50), including the
exact seven-day boundary. Dates come from first-parent main commit provenance,
not decision content. Pending admissions all count at the current check time.
Merge groups attribute each first-parent squash commit to one constituent PR
(the repository uses squash-only queues), then check their combined allowance.
Missing history or ambiguous merge-commit attribution fails closed.
Only request an explained, measured raise after checking current integrated
headroom; normal independent review and all other performance/security checks
still apply. This checker does not authorize raising user-facing latency/SLOs.
A nonblocking warning starts one block below the unchanged hard ceiling:
0.5% rounded down to 256 B, or byte precision for small budgets (minimum 1 B).
``--budget`` is for standalone measurement fixtures, not CI admissions.

Fail-closed: a missing build output dir or an empty bundle is an error, not
a pass — the check must run after ``pnpm build`` in the same job.

Wasm/script size gates are out of scope here; they are tracked separately
as Wasm enforcement.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

# Frozen starting allocation, raw bytes. Admissions add decision deltas.
BUDGET_CLIENT_JS_BYTES = 337_920  # 330 KiB; measured 306,065 B on 2026-10-27

CLIENT_DIR_PARTS = ("apps", "web", ".svelte-kit", "output", "client")

# Largest files listed on failure so the culprit is visible in the log.
TOP_FILES_ON_REPORT = 5


class BudgetError(Exception):
    """Bundle measurement or admission policy failed closed."""


LEDGER_DIR = "ci/budgets/web-client-js"
SCRIPT_PATH = ".github/scripts/check_web_bundle_budget.py"
WEEK_SECONDS = 7 * 86400


def _git(root: Path, *args: str, strip: bool = True) -> str:
    env = dict(os.environ)
    for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
        env.pop(key, None)
    try:
        output = subprocess.check_output(
            ["git", "--no-replace-objects", "-C", str(root), *args],
            env=env, stderr=subprocess.PIPE,
        ).decode("utf-8")
        return output.strip() if strip else output
    except (OSError, subprocess.CalledProcessError, UnicodeError) as exc:
        raise BudgetError("admission provenance unavailable; fetch full history") from exc


def _ledger(root: Path, ref: str | None) -> dict[str, str]:
    if ref is not None:
        records = {}
        tree = _git(root, "ls-tree", "-r", "-z", ref, "--", LEDGER_DIR)
        for entry in tree.split("\0"):
            if not entry:
                continue
            meta, name = entry.split("\t", 1)
            if meta.split()[0] != "100644":
                raise BudgetError("decisions must be regular files")
            records[name] = _git(root, "show", f"{ref}:{name}", strip=False)
        return records
    directory = root / LEDGER_DIR
    if any(path.is_symlink() for path in (directory, *directory.parents) if path != root):
        raise BudgetError("decision directory cannot be a symlink")
    if not directory.exists():
        return {}
    if not directory.is_dir():
        raise BudgetError("decision ledger must be a directory")
    records = {}
    for path in directory.rglob("*"):
        if path.is_symlink() or not path.is_file() or path.stat().st_mode & 0o111:
            raise BudgetError("decisions must be regular files in one directory")
        records[path.relative_to(root).as_posix()] = path.read_bytes().decode("utf-8")
    return records


def _unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise BudgetError("duplicate decision field")
        result[key] = value
    return result


def _deltas(records: dict[str, str]) -> dict[str, int]:
    deltas = {}
    measurements = []
    fields = {"unit", "delta_bytes", "measured_before_bytes",
              "measured_after_bytes", "cause"}
    for name, text in records.items():
        if not re.fullmatch(LEDGER_DIR + r"/[a-z0-9][a-z0-9-]*\.json", name):
            raise BudgetError(f"invalid decision identity: {name}")
        try:
            record = json.loads(text, object_pairs_hook=_unique_fields)
        except (ValueError, TypeError) as exc:
            raise BudgetError(f"malformed decision: {name}") from exc
        if not isinstance(record, dict) or set(record) != fields:
            raise BudgetError(f"invalid decision fields: {name}")
        if (record["unit"] != "web.client.js"
                or not isinstance(record["cause"], str)
                or not record["cause"].strip()):
            raise BudgetError(f"unknown unit or missing cause: {name}")
        for key in ("delta_bytes", "measured_before_bytes", "measured_after_bytes"):
            if type(record[key]) is not int or record[key] < 0:
                raise BudgetError(f"decision bytes must be nonnegative integers: {name}")
        if (record["delta_bytes"] == 0
                or record["measured_after_bytes"] <= record["measured_before_bytes"]):
            raise BudgetError(f"decision must describe positive growth: {name}")
        if record["delta_bytes"] > record["measured_after_bytes"] - record["measured_before_bytes"]:
            raise BudgetError(f"decision delta exceeds measured growth: {name}")
        deltas[name] = record["delta_bytes"]
        measurements.append(record["measured_after_bytes"])
    ceiling = BUDGET_CLIENT_JS_BYTES + sum(deltas.values())
    if any(size > ceiling for size in measurements):
        raise BudgetError("decision measurement exceeds admitted ceiling")
    return deltas


def _frozen_base(root: Path, ref: str | None) -> None:
    text = (_git(root, "show", f"{ref}:{SCRIPT_PATH}") if ref is not None
            else (root / SCRIPT_PATH).read_text(encoding="utf-8"))
    match = re.search(r"^BUDGET_CLIENT_JS_BYTES = ([0-9_]+)\b", text, re.M)
    if not match or int(match[1].replace("_", "")) != BUDGET_CLIENT_JS_BYTES:
        raise BudgetError("frozen base cannot change; add a decision in the feature PR")


def admitted_budget(root: Path, base: str, *, now: int, merge_group: bool = False) -> int:
    """Validate append-only admissions against a trusted main base, fail closed."""
    try:
        base = _git(root, "rev-parse", "--verify", "--end-of-options", f"{base}^{{commit}}")
        if _git(root, "rev-parse", "--is-shallow-repository") != "false":
            raise BudgetError("admission policy requires full history")
        if merge_group:
            if base not in _git(root, "rev-list", "--first-parent", "HEAD").splitlines():
                raise BudgetError("base is not on the first-parent integration history")
        elif _git(root, "merge-base", base, "HEAD") != base:
            raise BudgetError("base is not an ancestor of HEAD")
        _frozen_base(root, base)
        previous = _ledger(root, base)
        prior = _deltas(previous)
        ceiling = BUDGET_CLIENT_JS_BYTES + sum(prior.values())
        recent = 0
        for name, delta in prior.items():
            timestamp = _git(root, "log", "--first-parent", "--diff-filter=A",
                             "--format=%ct", base, "--", name).splitlines()
            if len(timestamp) != 1:
                raise BudgetError("ambiguous decision merge provenance")
            if int(timestamp[0]) >= now - WEEK_SECONDS:
                recent += delta
        window_base = ceiling - recent
        refs: list[str | None] = [None]
        if merge_group:
            commits = _git(root, "rev-list", "--first-parent", "--reverse",
                           f"{base}..HEAD").splitlines()
            for commit in commits:
                if len(_git(root, "rev-list", "--parents", "-n", "1", commit).split()) != 2:
                    raise BudgetError("queue requires one squash commit per PR")
            refs = commits + [None]  # also reject dirty/unattributed queue admissions
        for ref in refs:
            _frozen_base(root, ref)
            records = _ledger(root, ref)
            if any(records.get(name) != text for name, text in previous.items()):
                raise BudgetError("historical decisions are immutable")
            deltas = _deltas(records)
            added = sum(delta for name, delta in deltas.items() if name not in previous)
            if merge_group and ref is None and records != previous:
                raise BudgetError("unattributed queue decisions")
            if added > ceiling // 200:
                raise BudgetError("aggregate PR admissions exceed 0.5%")
            if added and recent + added > window_base // 50:
                raise BudgetError("rolling seven-day admissions exceed 2%")
            ceiling += added
            recent += added
            previous = records
        return ceiling
    except (OSError, UnicodeError, ValueError) as exc:
        raise BudgetError("cannot read admission policy/provenance") from exc


def policy_budget() -> int:
    root = Path(__file__).resolve().parents[2]
    event_path = os.environ.get("GITHUB_EVENT_PATH")
    try:
        if event_path:
            event = json.loads(Path(event_path).read_text(encoding="utf-8"))
            kind = os.environ.get("GITHUB_EVENT_NAME")
            if kind == "pull_request":
                base = event["pull_request"]["base"]["sha"]
                parents = _git(root, "rev-list", "--parents", "-n", "1", "HEAD").split()
                head = _git(root, "rev-parse", "--verify", "--end-of-options", f"{event['pull_request']['head']['sha']}^{{commit}}")
                # A PR head may itself merge main. Only a checkout distinct
                # from that head is GitHub's synthetic base/head merge.
                if parents[0] != head:
                    event_base = _git(root, "rev-parse", "--verify", "--end-of-options", f"{base}^{{commit}}")
                    if (len(parents) != 3 or head != parents[2]
                            or event_base not in _git(root, "rev-list", "--first-parent", parents[1]).splitlines()):
                        raise BudgetError("pull request merge provenance mismatch")
                    base = parents[1]
            elif kind == "merge_group":
                base = event["merge_group"]["base_sha"]
            elif kind == "workflow_dispatch":
                base = _git(root, "merge-base", "HEAD", "origin/main")
            elif kind == "push":
                base = "HEAD"  # already merged main; provenance is now authoritative
            else:
                raise BudgetError("unsupported admission event")
        else:
            if os.environ.get("GITHUB_ACTIONS") == "true":
                raise BudgetError("missing admission event")
            kind, base = "local", _git(root, "merge-base", "HEAD", "origin/main")
        return admitted_budget(
            root, base, now=int(time.time()), merge_group=kind == "merge_group",
        )
    except (OSError, ValueError, KeyError, TypeError) as exc:
        raise BudgetError("invalid admission event") from exc


def warning_block(budget: int) -> int:
    allowed = budget // 200
    return max(1, allowed // 256 * 256 if allowed >= 256 else allowed)


def default_client_dir() -> Path:
    return Path(__file__).resolve().parent.parent.parent.joinpath(*CLIENT_DIR_PARTS)


def client_js_files(client_dir: Path) -> list[Path]:
    if not client_dir.is_dir():
        raise BudgetError(
            f"client output dir is missing: {client_dir} "
            "(run `pnpm --dir apps/web build` first)"
        )
    files = sorted(p for p in client_dir.rglob("*.js") if p.is_file())
    if not files:
        raise BudgetError(
            f"no client JS files under {client_dir}; "
            "refusing to pass an empty bundle"
        )
    return files


def measure(client_dir: Path) -> list[tuple[Path, int]]:
    """Return ``(path, size_bytes)`` for every client JS file, largest first."""
    sized = [(path, path.stat().st_size) for path in client_js_files(client_dir)]
    sized.sort(key=lambda item: (-item[1], str(item[0])))
    return sized


def check(client_dir: Path, budget: int) -> tuple[int, list[tuple[Path, int]]]:
    """Return ``(total_bytes, sized_files)`` or raise ``BudgetError``.

    Raises ``BudgetError`` when the bundle cannot be measured or when the
    total exceeds ``budget``. A non-positive budget is a caller error.
    """
    if budget <= 0:
        raise BudgetError(f"budget must be positive, got {budget}")
    sized = measure(client_dir)
    total = sum(size for _, size in sized)
    if total > budget:
        raise BudgetError(
            f"client JS total {total} B exceeds budget {budget} B "
            f"(over by {total - budget} B)"
        )
    return total, sized


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--client-dir",
        type=Path,
        default=default_client_dir(),
        help="SvelteKit client output dir (default: %(default)s)",
    )
    parser.add_argument(
        "--budget",
        type=int,
        default=None,
        help="fixture-only byte ceiling override (default: validated decision ledger)",
    )
    args = parser.parse_args(argv)
    try:
        budget = args.budget if args.budget is not None else policy_budget()
        total, sized = check(args.client_dir, budget)
    except BudgetError as exc:
        print(f"bundle budget: FAIL: {exc}", flush=True)
        if args.client_dir.is_dir():
            try:
                over = measure(args.client_dir)
            except BudgetError:
                over = []
            for path, size in over[:TOP_FILES_ON_REPORT]:
                print(f"bundle budget:   {size:>8} B  {path.name}", flush=True)
        return 1
    headroom = budget - total
    print(
        f"bundle budget: PASS: client JS total {total} B "
        f"within budget {budget} B "
        f"({len(sized)} files, {headroom} B headroom)",
        flush=True,
    )
    if headroom <= warning_block(budget):
        print(f"bundle budget: WARN: within one block ({warning_block(budget)} B) "
              "of ceiling; plan measured feature headroom", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
