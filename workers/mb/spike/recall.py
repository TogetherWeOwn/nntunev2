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
import math
from collections import defaultdict
from pathlib import Path

from golden import query_group, read_tsv


def load_hits(path: Path, query_ids: set[str] | None = None, *, baseline=False) -> tuple[dict[str, list[str]], list[float]]:
    hits: dict[str, list[str]] = defaultdict(list)
    timings: list[float] = []
    timed = set()
    for cols in read_tsv(path):
        if len(cols) < 3 or not cols[0] or (query_ids is not None and cols[0] not in query_ids):
            raise SystemExit(f"recall: invalid hit row in {path}")
        qid = cols[0]
        if cols[1] == "0":
            ms = float(cols[5]) if len(cols) == 6 else math.nan
            if qid in timed or not math.isfinite(ms) or ms < 0 or (query_ids is not None and not baseline):
                raise SystemExit(f"recall: invalid timing row in {path}")
            timed.add(qid)
            timings.append(ms)
            continue
        if qid in timed or cols[1] != str(len(hits[qid]) + 1) or len(hits[qid]) == 5 or not cols[2]:
            raise SystemExit(f"recall: invalid hit rank or identity in {path}")
        hits[qid].append(cols[2])
    if baseline and timed != query_ids:
        raise SystemExit("recall: baseline timing rows do not cover the golden set")
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
    golden = list(read_tsv(args.golden))
    if not golden:
        raise SystemExit("recall: empty golden set is not gate evidence")
    if any(len(row) != 5 or not all(row) or not all(row[4].split(",")) for row in golden):
        raise SystemExit("recall: invalid golden row")
    query_ids = {row[0] for row in golden}
    if len(query_ids) != len(golden):
        raise SystemExit("recall: duplicate golden query ID")
    excluded = {query_group(row[1]) for path in args.exclude for row in read_tsv(path)}
    overlap = {query_group(row[1]) for row in golden} & excluded
    if overlap:
        raise SystemExit(f"recall: {len(overlap)} query groups overlap an excluded set; not gate evidence")
    pg, pg_ms = load_hits(args.pg_hits, query_ids, baseline=True)
    snap, _ = load_hits(args.snapshot_hits, query_ids)
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
