#!/usr/bin/env python3
"""Client-JS bundle budget: report when the web client build grows past budget.

Sums every ``*.js`` file under the SvelteKit client output
(``apps/web/.svelte-kit/output/client``, produced by ``pnpm build``) and
reports when the total exceeds the pinned budget below. Without this report,
bundle bloat (a heavy dependency, an accidental full-library import) lands
silently — ``pnpm build`` itself asserts nothing about size.

Advisory overage (owner order 2026-11-16: build first, optimise later): an
over-budget total prints a WARNING with the measured bytes and largest files,
emits a GitHub warning annotation and job-summary line, and exits 0. It never
claims the bundle fits. Measurement errors below still fail.

Budget: ``BUDGET_CLIENT_JS_BYTES = 368_640`` (360 KiB, raw bytes, not gzip).
Measured 339,150 B (~331 KiB, 52 files) on 2026-11-03 for the pending W10
app shell (PR #907: shell routes and furniture plus review-fix logic), so
the budget leaves ~8% headroom for the next first-party features while
catching any serious addition (a charting or date library typically adds
50+ KiB). Growth since the 232,233 B baseline of 2026-10-23 is expected:
first-party library features only, no new runtime dependencies.

How to update the budget (deliberately, in its own PR):
1. Run ``pnpm --dir apps/web build`` and note the new total this script prints.
2. Confirm the growth is expected (review the dependency/route change).
3. Bump ``BUDGET_CLIENT_JS_BYTES`` below with a comment citing the reason,
   and update the matching paragraph in ``apps/web/README.md``.
Never bump the budget just to make a red build green.

Fail-closed: a missing build output dir or an empty bundle is an error, not
a pass — the check must run after ``pnpm build`` in the same job.

Wasm/script size gates are out of scope here; they are tracked separately
as Wasm enforcement.
"""

from __future__ import annotations

import argparse
import os
import stat
import sys
from pathlib import Path

# Pinned budget, raw bytes. See module docstring for how to update.
BUDGET_CLIENT_JS_BYTES = (
    368_640  # 360 KiB; measured 339,150 B on 2026-11-03 (W10 app-shell client)
)

CLIENT_DIR_PARTS = ("apps", "web", ".svelte-kit", "output", "client")

# Largest files listed on an overage so the culprit is visible in the log.
TOP_FILES_ON_REPORT = 5


class BudgetError(Exception):
    """The client bundle could not be measured safely."""


class BudgetExceeded(BudgetError):
    """The client bundle was measured and its total exceeds the budget."""

    def __init__(self, message, sized):
        super().__init__(message)
        self.sized = sized


def default_client_dir() -> Path:
    return Path(__file__).resolve().parent.parent.parent.joinpath(*CLIENT_DIR_PARTS)


def client_js_files(client_dir: Path) -> list[Path]:
    if not client_dir.is_dir():
        raise BudgetError(
            f"client output dir is missing: {str(client_dir)!r} "
            "(run `pnpm --dir apps/web build` first)"
        )
    if client_dir.is_symlink():
        raise BudgetError(f"symlinked client output dir: {str(client_dir)!r}")

    def walk_error(exc):
        raise exc

    files = []
    for directory, dirs, names in os.walk(client_dir, onerror=walk_error):
        for name in dirs:
            path = Path(directory) / name
            if path.is_symlink():
                raise BudgetError(f"symlinked bundle directory: {str(path)!r}")
        files.extend(Path(directory) / name for name in names if name.endswith(".js"))
    if not files:
        raise BudgetError(
            f"no client JS files under {str(client_dir)!r}; refusing to pass an empty bundle"
        )
    return files


def measure(client_dir: Path) -> list[tuple[Path, int]]:
    """Return ``(path, size_bytes)`` for every client JS file, largest first."""
    sized = []
    try:
        for path in client_js_files(client_dir):
            info = path.lstat()
            if not stat.S_ISREG(info.st_mode):
                raise BudgetError(f"bundle entry is not a regular file: {str(path)!r}")
            sized.append((path, info.st_size))
    except OSError as exc:
        raise BudgetError(f"cannot measure bundle: {str(exc)!r}") from exc
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
        raise BudgetExceeded(
            f"client JS total {total} B exceeds budget {budget} B "
            f"(over by {total - budget} B)",
            sized,
        )
    return total, sized


def publish_warning(message: str) -> None:
    """Surface an advisory overage as an Actions annotation and job summary."""
    if os.environ.get("GITHUB_ACTIONS") != "true":
        return
    escaped = message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    print(f"::warning::{escaped}", flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        try:
            with open(summary, "a", encoding="utf-8") as handle:
                handle.write(f"## Client-JS bundle budget (advisory)\n\n- {message}\n")
        except OSError as exc:
            print(
                f"bundle budget: WARNING: could not write job summary: {exc}",
                flush=True,
            )


def report_top_files(sized: list[tuple[Path, int]]) -> None:
    for path, size in sized[:TOP_FILES_ON_REPORT]:
        name = path.name if path.name.isprintable() else repr(path.name)
        print(f"bundle budget:   {size:>8} B  {name}", flush=True)


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
        default=BUDGET_CLIENT_JS_BYTES,
        help="max total client JS bytes (default: %(default)s)",
    )
    args = parser.parse_args(argv)
    try:
        total, sized = check(args.client_dir, args.budget)
    except BudgetExceeded as exc:
        message = f"{exc} (advisory, not a failure)"
        print(f"bundle budget: WARNING: {message}", flush=True)
        report_top_files(exc.sized)
        publish_warning(message)
        return 0
    except BudgetError as exc:
        print(f"bundle budget: FAIL: {exc}", flush=True)
        return 1
    headroom = args.budget - total
    print(
        f"bundle budget: PASS: client JS total {total} B "
        f"within budget {args.budget} B "
        f"({len(sized)} files, {headroom} B headroom)",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
