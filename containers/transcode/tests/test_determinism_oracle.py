#!/usr/bin/env python3
"""Exercise the real image self-test's oracles with controlled external tools.

This is not an image/encoder run. The doubles isolate command exit handling,
large encoder listings, and ffprobe metadata; image CI still runs real Docker.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("determinism_self_test.sh")
PROBE = {
    # Deliberately not CSV order; Ogg Opus stream bit_rate is unavailable.
    "streams": [{"channels": 2, "bit_rate": "N/A", "sample_rate": "48000", "codec_name": "opus"}],
    "format": {"format_name": "ogg", "duration": "5.006500"},
    "packets": [{"size": "160", "duration_time": "0.020000"}] * 250,
}

DOCKER = r'''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args[0] in ("run", "rm"):
    sys.exit(0)
if args[0] != "exec":
    sys.exit(90)
if args[2] == "ffmpeg":
    mode = os.environ.get("ORACLE_ENCODER", "ok")
    if mode == "misleading":
        print("# libopus is not installed\n A..... opus native Opus encoder")
    elif mode == "missing":
        print(" A..... opus native Opus encoder")
    else:
        try:
            sys.stdout.write(" A..... libopus libopus Opus (codec opus)\n")
            sys.stdout.flush()
            # Force writes after grep -q has closed its input under pipefail.
            for _ in range(128):
                sys.stdout.write(" A..... other another encoder\n" * 4096)
                sys.stdout.flush()
        except BrokenPipeError:
            os._exit(1)
    sys.exit(7 if mode == "failed" else 0)
if args[2] == "ffprobe":
    print(os.environ["ORACLE_PROBE"])
    sys.exit(int(os.environ.get("ORACLE_PROBE_EXIT", "0")))
sys.exit(91)
'''

CURL = r'''#!/usr/bin/env python3
import hashlib, json, sys
args = sys.argv[1:]
artifact = b"OggS synthetic oracle fixture (not encoded audio)"
url = next(a for a in args if a.startswith("http://"))
if url.endswith("/health"):
    print('{"ok":true}')
elif url.endswith("/transcode"):
    print(json.dumps({"status": "transcoded", "artifact_sha256": hashlib.sha256(artifact).hexdigest()}, separators=(",", ":")))
elif "-o" in args:
    with open(args[args.index("-o") + 1], "wb") as handle:
        handle.write(artifact)
else:
    print('{"size":882044}')
'''


class DeterminismOracleTests(unittest.TestCase):
    def run_oracle(self, *, encoder="ok", probe=None, probe_exit=0):
        with tempfile.TemporaryDirectory(prefix="transcode-oracle-") as root:
            root = Path(root)
            for name, body in (("docker", DOCKER), ("curl", CURL)):
                path = root / name
                path.write_text(body)
                path.chmod(0o755)
            env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}", TMPDIR=str(root),
                       ORACLE_ENCODER=encoder, ORACLE_PROBE=json.dumps(PROBE if probe is None else probe),
                       ORACLE_PROBE_EXIT=str(probe_exit))
            # Noninteractive shell bootstrap must not replace this test's PATH.
            env.pop("BASH_ENV", None)
            return subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True, timeout=20)

    def assert_rejected(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stderr)

    def test_large_encoder_listing_is_fully_consumed(self):
        result = self.run_oracle()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("all assertions passed", result.stdout)

    def test_missing_or_non_encoder_libopus_is_rejected(self):
        for encoder in ("missing", "misleading"):
            with self.subTest(encoder=encoder):
                self.assert_rejected(self.run_oracle(encoder=encoder), "no libopus encoder")

    def test_failed_encoder_listing_is_rejected_even_with_libopus(self):
        self.assert_rejected(self.run_oracle(encoder="failed"), "encoder listing failed")

    def test_ffprobe_failure_is_not_hidden_by_valid_output(self):
        self.assert_rejected(self.run_oracle(probe_exit=7), "ffprobe failed")

    def test_wrong_profile_fields_are_rejected(self):
        for field, value in (("codec_name", "vorbis"), ("sample_rate", "44100"), ("channels", 1)):
            with self.subTest(field=field):
                probe = copy.deepcopy(PROBE)
                probe["streams"][0][field] = value
                self.assert_rejected(self.run_oracle(probe=probe), "profile mismatch")

    def test_wrong_container_or_duration_is_rejected(self):
        for field, value in (("format_name", "matroska"), ("duration", "0"), ("duration", "50"), ("duration", "NaN")):
            with self.subTest(field=field, value=value):
                probe = copy.deepcopy(PROBE)
                probe["format"][field] = value
                self.assert_rejected(self.run_oracle(probe=probe), "profile mismatch")

    def test_stream_bitrate_cannot_mask_wrong_packet_bitrate(self):
        for size in (80, 240):  # 32k and 96k, not the pinned 64k profile.
            with self.subTest(size=size):
                probe = copy.deepcopy(PROBE)
                probe["streams"][0]["bit_rate"] = "64000"
                probe["packets"] = [{"size": str(size), "duration_time": "0.020000"}] * 250
                self.assert_rejected(self.run_oracle(probe=probe), "profile mismatch")

    def test_absent_malformed_or_truncated_packet_evidence_is_rejected(self):
        for packets in ([], [{"size": "N/A", "duration_time": "0.02"}],
                        [{"size": "160", "duration_time": "0"}], PROBE["packets"][:25]):
            with self.subTest(packets=packets[:1]):
                probe = copy.deepcopy(PROBE)
                probe["packets"] = packets
                self.assert_rejected(self.run_oracle(probe=probe), "profile mismatch")

    def test_multiple_streams_are_rejected(self):
        probe = copy.deepcopy(PROBE)
        probe["streams"] *= 2
        self.assert_rejected(self.run_oracle(probe=probe), "profile mismatch")


if __name__ == "__main__":
    unittest.main()
