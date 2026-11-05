#!/usr/bin/env python3
"""Verify migration rename provenance using pinned Git and complete PR coverage.

Net API mappings are coverage checks, not historical events. Chronological
per-parent Git edges prove source lineage; only genuine byte-identical
renamed_from aliases remain positive applied identities. Source history is
never staging proof. See docs/ops.md, "Staging migrations".
"""
import argparse
import json
import pathlib
import sys

from migration_history import HistoryError, api_coverage, read_input, verify


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--files-file", required=True, help="gh api --paginate --slurp files JSON")
    parser.add_argument("--metadata-file", required=True, help="pinned PR metadata JSON")
    parser.add_argument("--manifest", default="migrations/manifest.toml")
    parser.add_argument("--output", help="write verified chronological edges, never ledger rows")
    args = parser.parse_args(argv)
    try:
        coverage = api_coverage(read_input(args.files_file), read_input(args.metadata_file),
                                args.base_sha, args.head_sha)
        edges = verify(args.root, args.base_sha, args.head_sha,
                       read_input(args.root / args.manifest), coverage)
        if args.output:
            # Event count and field lengths are bounded by verification.
            pathlib.Path(args.output).write_text(json.dumps(edges, indent=2) + "\n", encoding="utf-8")
    except (HistoryError, OSError, UnicodeError) as exc:
        print(f"migrations-staging: {exc}", file=sys.stderr)
        return 1
    print("migrations-staging: pinned Git source lineage and applied aliases verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
