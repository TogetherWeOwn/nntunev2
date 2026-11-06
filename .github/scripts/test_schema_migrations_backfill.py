#!/usr/bin/env python3
"""Pin the committed backfill SQL to its frozen generator state."""
import os
import pathlib
import re
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import schema_migrations_backfill as backfill
import staging_api_schema_gate as gate

REPO = pathlib.Path(__file__).resolve().parents[2]
INSERT_RE = re.compile(
    r"VALUES \(\s*'(\d{4}_[^']+\.sql)',\s*'([0-9a-f]{64})',")


def rendered_rows(target):
    return INSERT_RE.findall(backfill.render(target))


class BackfillTests(unittest.TestCase):
    def test_committed_files_match_the_generator(self):
        for target in backfill.TARGETS:
            committed = (REPO / "docs" / "ops" /
                         f"schema-migrations-backfill-2026-11-03-{target}.sql")
            self.assertEqual(committed.read_text(encoding="utf-8"),
                             backfill.render(target))

    def test_app_backfill_satisfies_the_gate_immediately(self):
        # The app file records exactly the frozen (2026-11-03) merged
        # manifest — no pre-merge extras — so the staging gate is green
        # as soon as the operator applies it.
        expected = [(name, sha, ()) for name, sha in backfill.rows_for("app")]
        self.assertTrue(gate.compare(expected, rendered_rows("app")))
        self.assertEqual(len(rendered_rows("app")), 25)

    def test_source_metadata_cannot_supply_backfill_proof(self):
        before = {target: backfill.render(target) for target in backfill.TARGETS}
        # Any manifest read would be a regression: backfill must remain frozen,
        # not reinterpret arbitrary source metadata as an applied identity.
        with mock.patch.object(pathlib.Path, "read_text", side_effect=AssertionError("manifest read")):
            self.assertEqual(before, {target: backfill.render(target) for target in backfill.TARGETS})

    def test_frozen_state_covers_0001_to_0029(self):
        rows = rendered_rows("app") + rendered_rows("mirror")
        numbers = sorted({name[:4] for name, _ in rows})
        self.assertEqual(numbers, [f"{n:04d}" for n in range(1, 30)])
        names = [name for name, _ in rows]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(len(rendered_rows("mirror")), 4)

    def test_premerge_files_hold_the_unmerged_rows_per_pr(self):
        rows_917 = rendered_rows("premerge-917")
        rows_922 = rendered_rows("premerge-922")
        names = [name for name, _ in rows_917 + rows_922]
        # Both 0030 claimants were applied before merge; neither is in
        # the app backfill, and each lands only with its own PR.
        self.assertEqual(len([n for n in names if n.startswith("0030")]), 2)
        self.assertEqual([n for n in names if n.startswith("0031")],
                         ["0031_ledger_seconds_floor.sql"])
        app_names = {name for name, _ in rendered_rows("app")}
        self.assertTrue(app_names.isdisjoint(names))


if __name__ == "__main__":
    unittest.main()
