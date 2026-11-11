#!/usr/bin/env python3
"""Score release-search top-5 recall for the R2 snapshot spike (gate 1).

Usage: recall.py <golden.tsv> <pg-hits.tsv> <snapshot-hits.tsv> <out.json> [--exclude prior.tsv ...]

A query is recalled when any of its top 5 hits is in its acceptable set
(golden column 5). Hit rows with rank 0 are timing rows (baseline only).
Prints a Markdown table and writes the full breakdown (overall, per
variant, per title-frequency stratum, wins/losses) as JSON. Gate 1 passes
when snapshot recall >= baseline recall overall.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path

from golden import query_group, read_tsv


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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("golden", type=Path)
    parser.add_argument("pg_hits", type=Path)
    parser.add_argument("snapshot_hits", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--exclude", type=Path, action="append", default=[],
                        help="Refuse gate evidence if a query group occurs in this prior TSV (repeatable)")
    args = parser.parse_args()
    golden = [line.split("\t") for line in args.golden.read_text(encoding="utf-8").splitlines()]
    if not golden:
        raise SystemExit("recall: empty golden set is not gate evidence")
    excluded = {query_group(row[1]) for path in args.exclude for row in read_tsv(path)}
    overlap = {query_group(row[1]) for row in golden} & excluded
    if overlap:
        raise SystemExit(f"recall: {len(overlap)} query groups overlap an excluded set; not gate evidence")
    pg, pg_ms = load_hits(args.pg_hits)
    snap, _ = load_hits(args.snapshot_hits)
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
    overall = groups["overall"]
    result = {
        "gate_1_pass": overall["snapshot"][0] >= overall["pg"][0],
        "excluded_query_groups": len(excluded),
        "pg_hits": overall["pg"][0],
        "snapshot_hits": overall["snapshot"][0],
        "recall_top5": table,
        "snapshot_only_hits": len(wins),
        "pg_only_hits": len(losses),
        "pg_only_examples": losses[:50],
        "pg_trgm_query_ms": {"p50": pct(pg_ms, 50), "p95": pct(pg_ms, 95), "max": max(pg_ms, default=None)},
    }
    args.out.write_text(json.dumps(result, indent=2, ensure_ascii=False))
    print("| group | queries | pg_trgm top-5 | snapshot top-5 |")
    print("|---|---|---|---|")
    for name, row in table.items():
        print(f"| {name} | {row['queries']} | {row['pg_trgm_recall']:.3f} | {row['snapshot_recall']:.3f} |")
    print(f"\nGate 1 (snapshot >= pg_trgm overall): {'PASS' if result['gate_1_pass'] else 'FAIL'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
