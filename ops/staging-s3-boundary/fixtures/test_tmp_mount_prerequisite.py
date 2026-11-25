"""Reproduce the shared storage rollout's PrivateTmp prerequisite offline.

Run with python3 -B ops/staging-s3-boundary/fixtures/test_tmp_mount_prerequisite.py -v.
The input is synthetic mountinfo text and the source paths are disposable.
"""

from pathlib import Path
import re
from tempfile import TemporaryDirectory
import unittest


CADDY_PRIVATE_TMP_PATH = re.compile(r"/tmp/systemd-private-[^/]*-caddy\.service-[^/]*/tmp")


def caddy_private_tmp_source_exists(mountinfo: str) -> bool:
    """Require the first /tmp row's fourth field to be a Caddy private directory."""
    for line in mountinfo.splitlines():
        fields = line.split()
        if len(fields) >= 5 and fields[4] == "/tmp":
            source = fields[3]
            return CADDY_PRIVATE_TMP_PATH.fullmatch(source) is not None and Path(source).is_dir()
    return False


class TmpMountPrerequisiteTest(unittest.TestCase):
    def test_first_tmp_mount_source(self):
        caddy_prefix = "systemd-private-fixture-caddy.service-"
        with (
            TemporaryDirectory(prefix=caddy_prefix, dir="/tmp") as valid_root,
            TemporaryDirectory(prefix=caddy_prefix, dir="/tmp") as missing_root,
            TemporaryDirectory(prefix="systemd-private-fixture-other.service-", dir="/tmp") as wrong_root,
        ):
            Path(valid_root, "tmp").mkdir()
            Path(wrong_root, "tmp").mkdir()
            present = f"42 1 0:1 {valid_root}/tmp /tmp rw - tmpfs tmpfs rw"
            missing = f"41 1 0:1 {missing_root}/tmp /tmp rw - tmpfs tmpfs rw"
            wrong = f"43 1 0:1 {wrong_root}/tmp /tmp rw - tmpfs tmpfs rw"
            other_mount = f"44 1 0:1 {valid_root}/tmp /tmp/other rw - tmpfs tmpfs rw"

            for label, rows, expected in (
                ("valid first source", (present, missing), True),
                ("missing first source", (missing, present), False),
                ("invalid first source path", (wrong, present), False),
                ("no exact mount match", (other_mount,), False),
            ):
                with self.subTest(case=label):
                    self.assertIs(caddy_private_tmp_source_exists("\n".join(rows)), expected)


if __name__ == "__main__":
    unittest.main()
