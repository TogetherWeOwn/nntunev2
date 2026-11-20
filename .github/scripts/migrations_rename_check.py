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

from migration_history import HistoryError, UnappliedRename, api_coverage, read_input, verify
from migration_snapshot import event_metadata, resumed_budget, snapshot_coverage


# Trusted event-base code owns these ordered pins. The custody observations
# confirmed both retired purge filenames and their digest were unapplied.
# Key: GitHub repository ID/name, PR number, base ref and head ref. Both
# sides must be from this repository. No PR or ledger input can add pins.
REVIEWED_UNAPPLIED = {
    (1417703572, "TogetherWeOwn/nntunev2", 1335, "main", "fix/purge-legacy-provider-credentials"): (
        UnappliedRename(
            reviewed_head="10c9e67b73c59efed95c2c693fd2fd1971dbf2e6",
            accepted_base="a61c8c0e3af8f73c308e8577694e6e1d774f97b2",
            old_path="migrations/0051_purge_legacy_provider_credentials.sql",
            new_path="migrations/0052_purge_legacy_provider_credentials.sql",
            sha256="ccd14876ce8ed7379c0a03ad1fb4404f12b32f0d31cb470b6b9f1817c8b0e0ca",
            target="app",
        ),
        UnappliedRename(
            reviewed_head="34e66e1a96605d71b75e22b2ace3baea29383ff8",
            accepted_base="499af8db0b61d416289971b8d39f276fcd61c8d9",
            reviewed_merge="c7d8ebf73749dd629296e7e3e7d59dd0db51a727",
            old_path="migrations/0052_purge_legacy_provider_credentials.sql",
            new_path="migrations/0053_purge_legacy_provider_credentials.sql",
            sha256="ccd14876ce8ed7379c0a03ad1fb4404f12b32f0d31cb470b6b9f1817c8b0e0ca",
            target="app",
        ),
    ),
}


def unapplied_policy(expected):
    base, head = expected["base"], expected["head"]
    repo = base["repo"]
    identity = repo["id"], repo["full_name"]
    # Deny new aliases for known-unapplied identities even when this PR
    # (or a fork) is not authorized to use their source-only pins.
    aliases = {(pin.old_path.removeprefix("migrations/"), pin.sha256)
               for key, pins in REVIEWED_UNAPPLIED.items() if key[:2] == identity for pin in pins}
    key = (*identity, expected["number"], base["ref"], head["ref"])
    pins = REVIEWED_UNAPPLIED.get(key, ()) if repo == head["repo"] else ()
    return {"unapplied": pins, "unapplied_aliases": aliases}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--files-file", required=True, help="gh api --paginate --slurp files JSON")
    parser.add_argument("--metadata-file", required=True, help="pinned PR metadata JSON")
    parser.add_argument("--event-file", help="require bracketed evidence against the original Actions event")
    parser.add_argument("--validation-output", help="write both pinned history validation outcomes")
    parser.add_argument("--manifest", default="migrations/manifest.toml")
    parser.add_argument("--output", help="write verified chronological edges, never ledger rows")
    args = parser.parse_args(argv)
    outcomes = {"event": {"outcome": "not_run"}, "effective": {"outcome": "not_run"}}
    active_pass = None
    status = 0
    try:
        files_text, metadata_text = read_input(args.files_file), read_input(args.metadata_file)
        manifest_text = read_input(args.root / args.manifest)
        if args.event_file:
            budget = resumed_budget(metadata_text)
            budget.consume(len(files_text.encode()) + len(metadata_text.encode()))
            expected = event_metadata(args.event_file, args.base_sha, args.head_sha)
            effective, coverage = snapshot_coverage(args.root, args.base_sha, args.head_sha,
                                                    files_text, metadata_text, expected, budget)
            for name, base in (("event", args.base_sha), ("effective", effective)):
                outcomes[name].update(base=base, head=args.head_sha)
            policy = unapplied_policy(expected)
            # Never let B1 absorb an event-range edge or the original baseline.
            active_pass = "event"
            edges = verify(args.root, args.base_sha, args.head_sha, manifest_text,
                           budget=budget, audit=outcomes["event"], **policy)
            active_pass = "effective"
            verify(args.root, effective, args.head_sha, manifest_text, coverage,
                   budget=budget, audit=outcomes["effective"], **policy)
            budget.remaining(1)
            active_pass = None
        else:
            coverage = api_coverage(files_text, metadata_text, args.base_sha, args.head_sha)
            edges = verify(args.root, args.base_sha, args.head_sha, manifest_text, coverage)
    except (HistoryError, OSError, UnicodeError) as exc:
        diagnostic = str(exc)[:1024]
        if active_pass:
            outcomes[active_pass]["outcome"] = "failed"
        outcomes["diagnostic"] = diagnostic
        print(f"migrations-staging: {diagnostic}", file=sys.stderr)
        status = 1
    try:
        if args.event_file:
            # A fixed two-pass summary and capped diagnostic survive even an
            # exhausted evidence budget; no more Git/API reads occur here.
            report = json.dumps(outcomes, sort_keys=True)
            print("migrations-staging: dual history validation " + report)
            if args.validation_output:
                pathlib.Path(args.validation_output).write_text(report + "\n", encoding="utf-8")
        if status:
            return status
        if args.output:
            # Event count and field lengths are bounded by verification.
            pathlib.Path(args.output).write_text(json.dumps(edges, indent=2) + "\n", encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        print(f"migrations-staging: {str(exc)[:1024]}", file=sys.stderr)
        return 1
    print("migrations-staging: pinned Git source lineage and applied aliases verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
