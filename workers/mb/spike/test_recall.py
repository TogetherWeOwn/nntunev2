#!/usr/bin/env python3
"""Gate decisions use hits, not rounded display values; baseline rows stay intact."""

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pg_baseline
import recall


class RecallTest(unittest.TestCase):
    def test_one_hit_regression_below_display_precision_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            golden, pg, snap, out = [root / name for name in ("golden", "pg", "snap", "out")]
            golden.write_text("".join(f"{i}\tAlbum\tplain\tunique\tid\n" for i in range(100000)))
            pg.write_text("".join(f"{i}\t1\tid\n" for i in range(74001)))
            snap.write_text("".join(f"{i}\t1\tid\n" for i in range(74000)))
            with patch.object(sys, "argv", ["recall.py", *map(str, (golden, pg, snap, out))]):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(recall.main(), 0)
            result = json.loads(out.read_text())
            self.assertEqual(result["recall_top5"]["overall"]["pg_trgm_recall"], 0.74)
            self.assertEqual(result["recall_top5"]["overall"]["snapshot_recall"], 0.74)
            self.assertFalse(result["gate_1_pass"])

    def test_empty_golden_is_not_gate_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            empty = root / "empty"
            empty.touch()
            with patch.object(sys, "argv", ["recall.py", str(empty), str(empty), str(empty), str(root / "out")]):
                with self.assertRaisesRegex(SystemExit, "empty golden"):
                    recall.main()

    def test_baseline_newlines_do_not_consume_ranks(self):
        rows = [{"mbid": f"id{i}", "label": "Album\n@@not-a-query\tTime: 9",
                 "score": 0.5, "artist_mbid": None, "artist_name": "Artist\r\nName"}
                for i in range(5)]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            golden, out = root / "golden", root / "hits"
            golden.write_text("1\tAlbum\tplain\tunique\tid4\n")
            stdout = "@@1\n" + "\n".join(json.dumps(row) for row in rows) + "\nTime: 1 ms\n"
            with patch.object(pg_baseline.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, stdout)) as run:
                pg_baseline.search(golden, out)
            self.assertIn("row_to_json", run.call_args.kwargs["input"])
            hits, timings = recall.load_hits(out)
            self.assertEqual(hits["1"], [f"id{i}" for i in range(5)])
            self.assertEqual(timings, [1.0])
            self.assertEqual(len(out.read_text().splitlines()), 6)


if __name__ == "__main__":
    unittest.main()
