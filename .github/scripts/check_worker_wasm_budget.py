#!/usr/bin/env python3
"""Enforce the per-Worker compressed Wasm size budget (fail-closed).

Each backend Worker ships as a WebAssembly module subject to the hosting
platform's compressed script-size ceiling. This script builds every Worker
crate for ``wasm32-unknown-unknown --release``, gzip-compresses the
resulting cdylib ``.wasm`` (level 9, fixed mtime for determinism), and
fails (exit 1) when any Worker is missing, unbuildable, unreadable, or
over budget.

Byte convention (explicit, never loosened without a reviewed policy
change): ``BUDGET_BYTES = 3_000_000`` decimal bytes of gzip-9 output per
Worker module. Measured 2026-10-17 headroom: api 773,637, ingest 874,834,
mb 615,300, repair 310,452, stream 285,144 -- every Worker under a third
of the ceiling, so the budget bites on real regressions, not on noise.
Measured 2026-10-22: youtube 207,435 (same ceiling).

Fail-closed by construction:

* a failed build, a missing/unreadable artifact, or an over-budget module
  is a table row AND a nonzero exit -- never a warning-only row;
* the CI job running this script carries no ``continue-on-error`` and is a
  required member of the ``ci-ok`` aggregate, so a red, cancelled, or
  skipped budget job fails the build.

Usage:
  python3 .github/scripts/check_worker_wasm_budget.py [--root DIR]
      [--skip-build] [--budget BYTES]

  --root DIR     repo root (default: current directory). All manifest and
                 target paths resolve under it.
  --skip-build   measure whatever ``.wasm`` artifacts already exist without
                 invoking cargo (local iteration; missing artifacts fail).
  --budget BYTES override the budget (tests and experiments only; CI uses
                 the compiled-in BUDGET_BYTES).
"""

from __future__ import annotations

import argparse
import gzip
import os
import subprocess
import sys
from pathlib import Path

TARGET = "wasm32-unknown-unknown"

# Compressed-size ceiling per Worker module, in decimal bytes. This is the
# release gate: raise it only with a reviewed policy change, never to make
# a red build green.
BUDGET_BYTES = 3_000_000

# One entry per Worker. ``kind`` selects how the crate is addressed:
# standalone crates (own Cargo.toml, own target dir) vs the stream/youtube
# crates (workspace members, repo-root target dir). Every crate has a committed,
# cargo-deny-audited Cargo.lock and builds with ``--locked`` so the
# measured module matches the audited dependency graph -- matching the
# required CI lanes.
WORKERS = (
    {
        "worker": "api",
        "kind": "manifest",
        "manifest": "workers/api/Cargo.toml",
        "locked": True,
        "target_dir": "workers/api/target",
        "wasm": "nntune_api.wasm",
    },
    {
        "worker": "ingest",
        "kind": "manifest",
        "manifest": "workers/ingest/Cargo.toml",
        "locked": True,
        "target_dir": "workers/ingest/target",
        "wasm": "nntune_ingest.wasm",
    },
    {
        "worker": "mb",
        "kind": "manifest",
        "manifest": "workers/mb/Cargo.toml",
        "locked": True,
        "target_dir": "workers/mb/target",
        "wasm": "nntune_mb.wasm",
    },
    {
        "worker": "repair",
        "kind": "manifest",
        "manifest": "workers/repair/Cargo.toml",
        "locked": True,
        "target_dir": "workers/repair/target",
        "wasm": "nntune_repair.wasm",
    },
    {
        "worker": "stream",
        "kind": "package",
        "package": "nntune-stream",
        "locked": True,
        "target_dir": "target",
        "wasm": "nntune_stream.wasm",
    },
    {
        "worker": "youtube",
        "kind": "package",
        "package": "nntune-youtube",
        "locked": True,
        "target_dir": "target",
        "wasm": "nntune_youtube.wasm",
    },
)


def cargo_args(spec: dict) -> list[str]:
    """Build the ``cargo build`` argv for one worker spec."""
    args = ["cargo", "build", "--release", "--target", TARGET]
    if spec["kind"] == "manifest":
        args += ["--manifest-path", spec["manifest"]]
    else:
        args += ["-p", spec["package"]]
    if spec.get("locked"):
        args.append("--locked")
    return args


def wasm_path(root: Path, spec: dict) -> Path:
    """Expected release ``.wasm`` artifact for one worker spec."""
    return root / spec["target_dir"] / TARGET / "release" / spec["wasm"]


def format_bytes(n: int | None) -> str:
    """Human-readable byte count for the size table (``n/a`` when unknown)."""
    if n is None:
        return "n/a"
    if n < 1024:
        return f"{n} B"
    if n < 1024 * 1024:
        return f"{n / 1024:.1f} KiB"
    return f"{n / (1024 * 1024):.1f} MiB"


def gzip_size(path: Path) -> int:
    """gzip level-9 compressed size of a file (fixed mtime: deterministic)."""
    return len(gzip.compress(path.read_bytes(), compresslevel=9, mtime=0))


def summarize_failure(output: str, returncode: int) -> str:
    """One-line summary of a failed build for the table status cell."""
    tail = "; ".join(
        seg for seg in (line.strip() for line in output.splitlines()[-5:]) if seg
    )
    if len(tail) > 300:
        tail = tail[:297] + "..."
    return f"exit {returncode}" + (f": {tail}" if tail else "")


# Generous per-Worker ceiling: a hung cargo (registry stall, stuck linker)
# must surface as a fail-closed build failure with the table still printed,
# never as a job wedged until the GitHub workflow timeout.
BUILD_TIMEOUT_SECONDS = 1800


def build_worker(root: Path, spec: dict) -> tuple[bool, str]:
    """Run the release wasm build for one worker. Returns (ok, detail)."""
    try:
        proc = subprocess.run(
            cargo_args(spec),
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=BUILD_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return False, f"timed out after {BUILD_TIMEOUT_SECONDS}s"
    except OSError as exc:
        return False, f"cargo not runnable: {exc}"
    if proc.returncode != 0:
        return False, summarize_failure(proc.stdout, proc.returncode)
    return True, "ok"


def measure_worker(root: Path, spec: dict) -> tuple[int | None, int | None, str]:
    """Measure raw + gzip bytes for one worker's ``.wasm`` artifact."""
    path = wasm_path(root, spec)
    if not path.is_file():
        try:
            shown = path.relative_to(root)
        except ValueError:
            shown = path
        return None, None, f"missing artifact: {shown}"
    try:
        raw = path.stat().st_size
        compressed = gzip_size(path)
    except OSError as exc:
        return None, None, f"unreadable artifact: {exc}"
    return raw, compressed, "ok"


def collect_rows(root: Path, skip_build: bool) -> list[dict]:
    """Build (unless skipped) and measure every Worker. Never raises."""
    rows = []
    for spec in WORKERS:
        if not skip_build:
            ok, detail = build_worker(root, spec)
            if not ok:
                rows.append(
                    {
                        "worker": spec["worker"],
                        "raw": None,
                        "compressed": None,
                        "status": f"error: build failed ({detail})",
                    }
                )
                continue
        raw, compressed, detail = measure_worker(root, spec)
        status = "ok" if detail == "ok" else f"error: {detail}"
        rows.append(
            {
                "worker": spec["worker"],
                "raw": raw,
                "compressed": compressed,
                "status": status,
            }
        )
    return rows


def evaluate(rows: list[dict], budget: int) -> list[str]:
    """Fail-closed verdict over measured rows. Returns failure lines."""
    failures = []
    for row in rows:
        if row["status"] != "ok":
            failures.append(f"{row['worker']}: {row['status']}")
        elif row["compressed"] is not None and row["compressed"] > budget:
            failures.append(
                f"{row['worker']}: {row['compressed']} compressed bytes "
                f"exceeds budget of {budget}"
            )
    return failures


def render_table(rows: list[dict], budget: int, failures: list[str]) -> str:
    """Render the per-Worker budget rows as a markdown table."""
    verdict = (
        "PASS" if not failures else f"FAIL ({len(failures)} over budget or error)"
    )
    lines = [
        f"## Worker Wasm budget (enforcing, gzip-9, ceiling {budget} B): {verdict}",
        "",
        "Release ``wasm32`` cdylib bytes per Worker with gzip level-9"
        f" compression. Every module must compress to at most {budget} bytes;",
        "a missing, unbuildable, or over-budget module fails the build.",
        "",
        "| Worker | Raw | gzip | Raw bytes | gzip bytes | Status |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for row in rows:
        # The status cell must stay on one line and one column or the
        # markdown table breaks; flatten stray newlines defensively and
        # escape pipes (rustc gutter lines like ``1 | foo`` would otherwise
        # open extra cells). Build summaries already arrive single-line
        # from summarize_failure.
        status = row["status"].replace("\n", " ").replace("|", "\\|")
        lines.append(
            "| `{worker}` | {raw_h} | {gz_h} | {raw} | {gz} | {status} |".format(
                worker=row["worker"],
                raw_h=format_bytes(row["raw"]),
                gz_h=format_bytes(row["compressed"]),
                raw=row["raw"] if row["raw"] is not None else "n/a",
                gz=row["compressed"] if row["compressed"] is not None else "n/a",
                status=status,
            )
        )
    if failures:
        lines += ["", "Failures:", ""]
        lines += [f"- {failure}" for failure in failures]
    return "\n".join(lines) + "\n"


def publish(table: str) -> None:
    """Print the table; also append it to the CI job summary when set."""
    print(table, end="")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        try:
            with open(summary, "a", encoding="utf-8") as fh:
                fh.write(table)
        except OSError as exc:
            # The table already reached stdout; a summary-append failure
            # must not mask a budget failure, so report and continue --
            # the process exit code below still carries the verdict.
            print(f"warning: could not append job summary: {exc}", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repo root directory")
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="measure existing artifacts without invoking cargo",
    )
    parser.add_argument(
        "--budget",
        type=int,
        default=BUDGET_BYTES,
        help="compressed-size ceiling in bytes (default: BUDGET_BYTES)",
    )
    args = parser.parse_args(argv)
    root = Path(args.root).resolve()
    rows = collect_rows(root, skip_build=args.skip_build)
    failures = evaluate(rows, args.budget)
    publish(render_table(rows, args.budget, failures))
    # Fail-closed: any missing, failed, or over-budget Worker is exit 1.
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
