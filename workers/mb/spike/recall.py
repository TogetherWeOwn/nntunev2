#!/usr/bin/env python3
"""Score release-search top-5 recall for the R2 snapshot spike (gate 1).

Usage: recall.py <golden.tsv> <pg-hits.tsv> <snapshot-hits.tsv> <out.json>

A query is recalled when any of its top 5 hits is in its acceptable set
(golden column 5). Hit rows with rank 0 are timing rows (baseline only).
Prints a Markdown table and writes the full breakdown (overall, per
variant, per title-frequency stratum, wins/losses) as JSON. Gate 1 passes
when snapshot recall >= baseline recall overall.
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path


def load_hits(path: Path) -> tuple[dict[str, list[str]], list[float]]:
    hits: dict[str, list[str]] = defaultdict(list)
    timings: list[float] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        cols = line.split("\t")
        if cols[1] == "0":
            timings.append(float(cols[5]))
            continue
        if int(cols[1]) <= 5:
            hits[cols[0]].append(cols[2])
    return hits, timings


def pct(values: list[float], p: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round(p / 100 * (len(ordered) - 1))))]


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__.splitlines()[2], file=sys.stderr)
        return 2
    golden = [line.split("\t") for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()]
    pg, pg_ms = load_hits(Path(sys.argv[2]))
    snap, _ = load_hits(Path(sys.argv[3]))
    groups: dict[str, dict[str, list[int]]] = defaultdict(lambda: {"pg": [0, 0], "snapshot": [0, 0]})
    wins, losses = [], []
    for qid, q, variant, stratum, acceptable in golden:
        ok = set(acceptable.split(","))
        got_pg = bool(ok & set(pg.get(qid, [])))
        got_snap = bool(ok & set(snap.get(qid, [])))
        for group in ("overall", f"variant:{variant}", f"stratum:{stratum}"):
            groups[group]["pg"][0] += got_pg
            groups[group]["pg"][1] += 1
            groups[group]["snapshot"][0] += got_snap
            groups[group]["snapshot"][1] += 1
        if got_snap and not got_pg:
            wins.append({"id": qid, "q": q, "variant": variant, "stratum": stratum})
        if got_pg and not got_snap:
            losses.append({"id": qid, "q": q, "variant": variant, "stratum": stratum})
    table = {
        name: {
            "queries": values["pg"][1],
            "pg_trgm_recall": round(values["pg"][0] / values["pg"][1], 4),
            "snapshot_recall": round(values["snapshot"][0] / values["snapshot"][1], 4),
        }
        for name, values in sorted(groups.items())
    }
    overall = table["overall"]
    result = {
        "gate_1_pass": overall["snapshot_recall"] >= overall["pg_trgm_recall"],
        "recall_top5": table,
        "snapshot_only_hits": len(wins),
        "pg_only_hits": len(losses),
        "pg_only_examples": losses[:50],
        "pg_trgm_query_ms": {"p50": pct(pg_ms, 50), "p95": pct(pg_ms, 95), "max": max(pg_ms, default=None)},
    }
    Path(sys.argv[4]).write_text(json.dumps(result, indent=2, ensure_ascii=False))
    print("| group | queries | pg_trgm top-5 | snapshot top-5 |")
    print("|---|---|---|---|")
    for name, row in table.items():
        print(f"| {name} | {row['queries']} | {row['pg_trgm_recall']:.3f} | {row['snapshot_recall']:.3f} |")
    print(f"\nGate 1 (snapshot >= pg_trgm overall): {'PASS' if result['gate_1_pass'] else 'FAIL'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
