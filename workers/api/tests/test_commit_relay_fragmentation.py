"""Fragmented protocol control for the disposable COMMIT relay."""

import importlib.util
import socket
import struct
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "relay", Path(__file__).with_name("pg_commit_relay.py")
)
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)


def frame(kind, body):
    return kind + struct.pack("!I", len(body) + 4) + body


def fragments(feed, data):
    forwarded = []
    for byte in data:
        forwarded.extend(feed(bytes([byte])))
    return forwarded


class CommitRelayFragmentation(unittest.TestCase):
    def test_relay_exit_before_connection_releases_listener(self):
        with relay.commit_relay() as result:
            pass
        with self.assertRaises(relay.ProtocolError):
            result.result(timeout=1)
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", 15432))

    def test_fragmented_commit_and_response_are_withheld(self):
        exchange = relay.CommitExchange()
        params = b"user\0postgres\0database\0nntune_billing_ack\0\0"
        startup = struct.pack("!II", len(params) + 8, 196608) + params
        self.assertEqual(fragments(exchange.frontend, startup), [startup])
        ready_idle = frame(b"Z", b"I")
        self.assertEqual(fragments(exchange.backend, ready_idle), [ready_idle])

        begin = frame(b"Q", b"BEGIN\0")
        self.assertEqual(fragments(exchange.frontend, begin), [begin])
        ready_transaction = frame(b"Z", b"T")
        self.assertEqual(
            fragments(exchange.backend, ready_transaction), [ready_transaction]
        )

        commit = frame(b"Q", b"COMMIT\0")
        self.assertEqual(fragments(exchange.frontend, commit), [commit])
        response = frame(b"C", b"COMMIT\0") + ready_idle
        self.assertEqual(fragments(exchange.backend, response), [])
        self.assertTrue(exchange.withheld)
        self.assertEqual(bytes(exchange.gate.held), response)

    def test_commit_before_previous_response_drains_is_rejected(self):
        exchange = relay.CommitExchange()
        params = b"user\0postgres\0database\0nntune_billing_ack\0\0"
        startup = struct.pack("!II", len(params) + 8, 196608) + params
        exchange.frontend(startup)
        exchange.backend(frame(b"Z", b"I"))
        exchange.frontend(frame(b"Q", b"BEGIN\0"))
        exchange.backend(frame(b"Z", b"T"))
        exchange.frontend(frame(b"Q", b"SELECT 1\0"))

        with self.assertRaises(relay.ProtocolError):
            exchange.frontend(frame(b"Q", b"COMMIT\0"))
        self.assertIsNone(exchange.gate)

    def test_commit_with_trailing_partial_frame_is_rejected(self):
        exchange = relay.CommitExchange()
        params = b"user\0postgres\0database\0nntune_billing_ack\0\0"
        startup = struct.pack("!II", len(params) + 8, 196608) + params
        exchange.frontend(startup)
        exchange.backend(frame(b"Z", b"I"))
        exchange.frontend(frame(b"Q", b"BEGIN\0"))
        exchange.backend(frame(b"Z", b"T"))

        with self.assertRaises(relay.ProtocolError):
            exchange.frontend(frame(b"Q", b"COMMIT\0") + b"Q")
        self.assertIsNone(exchange.gate)


if __name__ == "__main__":
    unittest.main()
