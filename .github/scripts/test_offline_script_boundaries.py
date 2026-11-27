"""Focused synthetic replays for the release and trial script boundaries."""

from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock

import staging_trial_schema as schema
from production_backend_identity import readback
from test_production_backend_identity import ACCOUNT, API, STREAM

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from staging_trial_c0_readback import replay_hyperdrive_count, verified_hyperdrive_count


class OfflineBoundaryTests(unittest.TestCase):
    def test_backend_pins_require_independent_identities(self):
        reader = Mock()
        for field in ("version_id", "deployment_id"):
            alias = STREAM._replace(**{field: getattr(API, field)})
            self.assertEqual(readback(account=ACCOUNT, api=API, stream=alias, read=reader),
                             {"identity": "unavailable", "reason": "invalid-pins"})
        reader.assert_not_called()

    def test_schema_refuses_foreign_sources_and_invalid_targets(self):
        with tempfile.TemporaryDirectory() as scratch:
            with self.assertRaises(schema.migrator.ApplyError):
                schema.read_source(Path(scratch), ROOT / "README.md")
        for role, database in ((None, "nntunev2_staging"), ("pg_write_all_data", "nntunev2_staging"),
                               ("trial_writer", "nntunev2_production")):
            with self.assertRaises(schema.migrator.ApplyError):
                schema.compose(ROOT, ROOT, role, "trial_reader", database=database)

    def test_c0_replay_rejects_contradictory_page_counts_and_non_json(self):
        info = {"page": 1, "per_page": 100, "count": 0, "total_count": 0, "total_pages": 1}
        page = {"success": True, "errors": [], "result": [], "result_info": info}
        self.assertEqual(verified_hyperdrive_count([page]), 0)
        for invalid in (True, 2, "1"):
            info["total_pages"] = invalid
            self.assertIsNone(verified_hyperdrive_count([page]))
        with tempfile.TemporaryDirectory() as scratch:
            replay = Path(scratch) / "pages.json"
            replay.write_text('[{"success":true,"errors":[],"result":[],"messages":[NaN],'
                              '"result_info":{"page":1,"per_page":100,"count":0,"total_count":0}}]')
            self.assertIsNone(replay_hyperdrive_count(replay))
