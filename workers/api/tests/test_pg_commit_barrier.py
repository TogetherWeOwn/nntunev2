#!/usr/bin/env python3
"""Pure tests for the disposable PostgreSQL acknowledgement barrier."""
import importlib.util
import struct
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "pg_commit_barrier", Path(__file__).resolve().parent / "pg_commit_barrier.py")
barrier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(barrier)


def frame(kind, payload=b""):
    return kind + struct.pack("!I", len(payload) + 4) + payload


class CommitBarrierTests(unittest.TestCase):
    def test_holds_success_until_ready_and_explicit_release(self):
        gate = barrier.CommitBarrier()
        notice = frame(b"N", b"notice\0")
        commit, ready = frame(b"C", b"COMMIT\0"), frame(b"Z", b"I")
        self.assertEqual(gate.accept(notice), notice)
        self.assertEqual(gate.accept(commit), b"")
        with self.assertRaises(barrier.BarrierError):
            gate.release()
        self.assertEqual(gate.accept(ready), b"")
        self.assertEqual(gate.state, "ready")
        self.assertEqual(gate.release(), commit + ready)
        with self.assertRaises(barrier.BarrierError):
            gate.accept(ready)

    def test_unexpected_response_fails_closed(self):
        for message in (frame(b"E"), frame(b"Z", b"I"),
                        frame(b"C", b"ROLLBACK\0"), b"C\0\0\0\x10x"):
            with self.subTest(message=message):
                gate = barrier.CommitBarrier()
                with self.assertRaises(barrier.BarrierError):
                    gate.accept(message)
                with self.assertRaises(barrier.BarrierError):
                    gate.release()

    def test_non_idle_or_extra_completion_cannot_release(self):
        for message in (frame(b"Z", b"T"), frame(b"C", b"SELECT 1\0"), frame(b"E")):
            with self.subTest(message=message):
                gate = barrier.CommitBarrier()
                gate.accept(frame(b"C", b"COMMIT\0"))
                with self.assertRaises(barrier.BarrierError):
                    gate.accept(message)
                with self.assertRaises(barrier.BarrierError):
                    gate.release()

    def test_held_ancillary_messages_are_released_in_order(self):
        gate = barrier.CommitBarrier()
        messages = (frame(b"C", b"COMMIT\0"), frame(b"N", b"notice\0"),
                    frame(b"S", b"key\0value\0"), frame(b"A", b"notification"),
                    frame(b"Z", b"I"))
        for message in messages:
            self.assertEqual(gate.accept(message), b"")
        self.assertEqual(gate.release(), b"".join(messages))

    def test_incompatible_held_messages_fail_closed(self):
        for message in (frame(b"D", b"\0\0"), frame(b"?")):
            with self.subTest(message=message):
                gate = barrier.CommitBarrier()
                gate.accept(frame(b"C", b"COMMIT\0"))
                with self.assertRaises(barrier.BarrierError):
                    gate.accept(message)
                with self.assertRaises(barrier.BarrierError):
                    gate.accept(frame(b"Z", b"I"))
                with self.assertRaises(barrier.BarrierError):
                    gate.release()

    def test_held_response_is_bounded(self):
        gate = barrier.CommitBarrier(max_held=18)
        gate.accept(frame(b"C", b"COMMIT\0"))
        with self.assertRaises(barrier.BarrierError):
            gate.accept(frame(b"N", b"too large"))
        with self.assertRaises(barrier.BarrierError):
            gate.release()
        with self.assertRaises(ValueError):
            barrier.CommitBarrier(max_held=17)

    def test_no_messages_after_idle_before_release(self):
        gate = barrier.CommitBarrier()
        gate.accept(frame(b"C", b"COMMIT\0"))
        gate.accept(frame(b"Z", b"I"))
        with self.assertRaises(barrier.BarrierError):
            gate.accept(frame(b"N"))
        with self.assertRaises(barrier.BarrierError):
            gate.release()


if __name__ == "__main__":
    unittest.main()
