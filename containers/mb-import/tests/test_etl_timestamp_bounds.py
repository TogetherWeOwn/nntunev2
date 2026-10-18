#!/usr/bin/env python3
"""UTC normalization failures must not create or replace curated snapshots."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


TEST_DIR = Path(__file__).resolve().parent
ETL = TEST_DIR.parent / "etl.sh"


class TimestampBoundsTests(unittest.TestCase):
    def test_utc_overflow_is_cleanly_refused_before_publication(self):
        for timestamp in (
            "0001-01-01T00:00:00+15:00",
            "9999-12-31T23:59:59-15:00",
        ):
            with self.subTest(timestamp=timestamp), tempfile.TemporaryDirectory() as work:
                raw = Path(work) / "mbdump"
                shutil.copytree(TEST_DIR / "fixtures" / "mbdump", raw)

                def run(value):
                    return subprocess.run(
                        ["bash", str(ETL), str(raw)],
                        env=dict(os.environ, DUMP_TS=value),
                        capture_output=True, text=True, check=False,
                    )

                def state():
                    return {
                        str(path.relative_to(raw)): (
                            ("link", os.readlink(path)) if path.is_symlink()
                            else ("file", path.read_bytes()) if path.is_file()
                            else ("directory",)
                        )
                        for path in raw.rglob("*")
                    }

                def assert_refused():
                    before = state()
                    result = run(timestamp)
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("DUMP_TS must be representable as a UTC timestamp", result.stderr)
                    self.assertNotIn("Traceback", result.stderr)
                    self.assertEqual(state(), before)

                assert_refused()
                seeded = run("2026-10-18T12:00:00Z")
                self.assertEqual(seeded.returncode, 0, seeded.stderr)
                self.assertTrue((raw / "curated").is_symlink())
                assert_refused()


if __name__ == "__main__":
    unittest.main()
