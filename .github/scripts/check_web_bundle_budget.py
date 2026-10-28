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
import sys
from pathlib import Path

# Pinned budget, raw bytes. See module docstring for how to update.
BUDGET_CLIENT_JS_BYTES = 337_920  # 330 KiB; measured 306,065 B on 2026-10-27

CLIENT_DIR_PARTS = ("apps", "web", ".svelte-kit", "output", "client")

# Largest files listed on failure so the culprit is visible in the log.
TOP_FILES_ON_REPORT = 5


class BudgetError(Exception):
    """The client bundle could not be measured safely."""


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
        default=BUDGET_CLIENT_JS_BYTES,
        help="max total client JS bytes (default: %(default)s)",
    )
    args = parser.parse_args(argv)
    try:
        total, sized = check(args.client_dir, args.budget)
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
