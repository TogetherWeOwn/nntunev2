#!/usr/bin/env python3
"""Report per-Worker compressed Wasm sizes (warning-only, never fails).

CI checks wasm32 targets but reports no per-Worker sizes, so size
regressions stay invisible until a deploy breaks. This script builds each
Worker crate for ``wasm32-unknown-unknown --release``, measures the raw
and gzip-compressed bytes of the resulting cdylib ``.wasm``, and prints a
per-Worker markdown table to stdout (appended to ``$GITHUB_STEP_SUMMARY``
when set, so it shows on the CI run and its PR).

Warning-only by construction:

* per-Worker build or measurement failures become ``error`` rows, never a
  nonzero exit;
* the script exits 0 whenever it runs to completion;
* the CI job running it is kept out of the required-checks aggregate, so
  neither a size increase nor a broken report step can fail the build.

Size enforcement, if ever wanted, belongs to a separate gated lane with
its own thresholds -- not this report. No dependency gates are changed
here.

The sizes are a proxy, not the exact deploy bundle: plain
``cargo build --release`` cdylib output compressed with gzip level 9,
rather than the worker-build/wrangler bundle. They exist to make size
trends and regressions visible early.

Usage:
  python3 .github/scripts/report_worker_wasm_sizes.py [--root DIR] [--skip-build]

  --root DIR    repo root (default: current directory). All manifest and
                target paths resolve under it.
  --skip-build  measure whatever ``.wasm`` artifacts already exist without
                invoking cargo (local iteration; missing artifacts become
                ``error`` rows).
"""

from __future__ import annotations

import argparse
import gzip
import os
import subprocess
import sys
from pathlib import Path

TARGET = "wasm32-unknown-unknown"

# Per-Worker cargo timeout, in seconds. A hung build must not take the whole
# report down: five Workers x 300 s = 25 min worst case, inside the CI job's
# 30-minute timeout. Timeouts become ``error`` rows so collect_rows() keeps
# measuring the remaining Workers.
BUILD_TIMEOUT_SECONDS = 300

# One entry per Worker. ``kind`` selects how the crate is addressed:
# standalone crates (own Cargo.toml, own target dir) vs the stream crate
# (workspace member, repo-root target dir). Only the api crate has a
# committed, audited Cargo.lock, so only it builds with ``--locked`` --
# matching the required CI lanes for each crate.
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
        "locked": False,
        "target_dir": "workers/ingest/target",
        "wasm": "nntune_ingest.wasm",
    },
    {
        "worker": "mb",
        "kind": "manifest",
        "manifest": "workers/mb/Cargo.toml",
        "locked": False,
        "target_dir": "workers/mb/target",
        "wasm": "nntune_mb.wasm",
    },
    {
        "worker": "repair",
        "kind": "manifest",
        "manifest": "workers/repair/Cargo.toml",
        "locked": False,
        "target_dir": "workers/repair/target",
        "wasm": "nntune_repair.wasm",
    },
    {
        "worker": "stream",
        "kind": "package",
        "package": "nntune-stream",
        "locked": False,
        "target_dir": "target",
        "wasm": "nntune_stream.wasm",
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
        return False, f"build timed out after {BUILD_TIMEOUT_SECONDS}s"
    except OSError as exc:
        return False, f"cargo not runnable: {exc}"
    if proc.returncode != 0:
        return False, summarize_failure(proc.stdout, proc.returncode)
    return True, "ok"


def measure_worker(root: Path, spec: dict) -> tuple[int | None, int | None, str]:
    """Measure raw + gzip bytes for one worker's ``.wasm`` artifact."""
    path = wasm_path(root, spec)
    if not path.is_file():
        return None, None, f"missing artifact: {path.relative_to(root)}"
    try:
        raw = path.stat().st_size
        compressed = gzip_size(path)
    except OSError as exc:
        return None, None, f"unreadable artifact: {exc}"
    return raw, compressed, "ok"


def render_table(rows: list[dict]) -> str:
    """Render the per-Worker size rows as a markdown table."""
    lines = [
        "## Worker Wasm sizes (report-only, gzip-9 proxy)",
        "",
        "Release ``wasm32`` cdylib bytes per Worker with gzip level-9"
        " compression. Warning-only: increases never fail the build.",
        "",
        "| Worker | Raw | gzip | Raw bytes | gzip bytes | Status |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for row in rows:
        # The status cell must stay on one line or the markdown table
        # breaks; flatten any stray newlines defensively (build summaries
        # already arrive single-line from summarize_failure). Escape pipes
        # too: rustc diagnostics almost always contain `|` gutter lines, and
        # an unescaped pipe splits the cell and drops the error detail.
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
    return "\n".join(lines) + "\n"


def collect_rows(root: Path, skip_build: bool) -> list[dict]:
    """Build (unless skipped) and measure every Worker. Never raises."""
    rows = []
    for spec in WORKERS:
        status = "ok"
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
        if detail != "ok":
            status = f"error: {detail}"
        rows.append(
            {
                "worker": spec["worker"],
                "raw": raw,
                "compressed": compressed,
                "status": status,
            }
        )
    return rows


def publish(table: str) -> None:
    """Print the table; also append it to the CI job summary when set."""
    print(table, end="")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        try:
            with open(summary, "a", encoding="utf-8") as fh:
                fh.write(table)
        except OSError as exc:
            # The report itself must not fail: stdout already has the table.
            print(f"warning: could not append job summary: {exc}", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repo root directory")
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="measure existing artifacts without invoking cargo",
    )
    args = parser.parse_args(argv)
    root = Path(args.root).resolve()
    rows = collect_rows(root, skip_build=args.skip_build)
    publish(render_table(rows))
    # Warning-only: per-Worker failures are rows, not an exit code.
    return 0


if __name__ == "__main__":
    sys.exit(main())
