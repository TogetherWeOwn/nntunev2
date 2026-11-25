"""Reproduce the shared storage rollout's PrivateTmp prerequisite offline.

Run with python3 -B ops/staging-s3-boundary/fixtures/test_tmp_mount_prerequisite.py -v.
The input is synthetic mountinfo text and the source paths are disposable.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest


def tmp_mount_source_exists(mountinfo: str) -> bool:
    """Check field four of the first row whose fifth field is /tmp."""
    for line in mountinfo.splitlines():
        fields = line.split()
        if len(fields) >= 5 and fields[4] == "/tmp":
            return Path(fields[3]).is_dir()
    return False


class TmpMountPrerequisiteTest(unittest.TestCase):
    def test_first_tmp_mount_source(self):
        with TemporaryDirectory(prefix="nntune-mountinfo-", dir="/tmp") as scratch:
            present = f"42 1 0:1 {scratch} /tmp rw - tmpfs tmpfs rw"
            missing = f"41 1 0:1 {scratch}/missing /tmp rw - tmpfs tmpfs rw"
            other = f"43 1 0:1 {scratch} /tmp/other rw - tmpfs tmpfs rw"

            for label, rows, expected in (
                ("valid first source", (present, missing), True),
                ("missing first source", (missing, present), False),
                ("no exact mount match", (other,), False),
            ):
                with self.subTest(case=label):
                    self.assertIs(tmp_mount_source_exists("\n".join(rows)), expected)


if __name__ == "__main__":
    unittest.main()
