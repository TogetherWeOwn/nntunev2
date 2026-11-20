#!/usr/bin/env python3
"""Pure wire-framing tests for the disposable PostgreSQL harness."""
import importlib.util
import struct
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "pg_protocol_frames", Path(__file__).resolve().parent / "pg_protocol_frames.py")
frames = importlib.util.module_from_spec(spec)
spec.loader.exec_module(frames)


def frame(kind, payload=b""):
    return kind + struct.pack("!I", len(payload) + 4) + payload


class FrameDecoderTests(unittest.TestCase):
    def test_fragmented_and_coalesced_frames(self):
        expected = [frame(b"Q", b"COMMIT\0"), frame(b"Z", b"I"), frame(b"S")]
        wire = b"".join(expected)
        for split in range(len(wire) + 1):
            decoder = frames.FrameDecoder()
            self.assertEqual(decoder.feed(wire[:split]) + decoder.feed(wire[split:]), expected)
            decoder.finish()
        decoder = frames.FrameDecoder()
        self.assertEqual([f for byte in wire for f in decoder.feed(bytes([byte]))], expected)
        decoder.finish()

    def test_invalid_lengths_and_bounds(self):
        for length in (0, 3, 16, 0xffffffff):
            with self.subTest(length=length), self.assertRaises(frames.ProtocolError):
                frames.FrameDecoder(max_frame=16).feed(b"Q" + struct.pack("!I", length))
        decoder = frames.FrameDecoder(max_frame=16)
        self.assertEqual(decoder.feed(frame(b"Q", b"x" * 11)), [frame(b"Q", b"x" * 11)])
        decoder.finish()

    def test_finish_rejects_partial_header_or_body(self):
        wire = frame(b"C", b"COMMIT\0")
        for length in range(1, len(wire)):
            with self.subTest(length=length):
                decoder = frames.FrameDecoder()
                self.assertEqual(decoder.feed(wire[:length]), [])
                with self.assertRaises(frames.ProtocolError):
                    decoder.finish()

    def test_many_frames_do_not_exceed_single_frame_bound(self):
        decoder = frames.FrameDecoder(max_frame=5)
        self.assertEqual(decoder.feed(frame(b"S") * 100), [frame(b"S")] * 100)
        decoder.finish()
        with self.assertRaises(ValueError):
            frames.FrameDecoder(max_frame=4)


if __name__ == "__main__":
    unittest.main()
