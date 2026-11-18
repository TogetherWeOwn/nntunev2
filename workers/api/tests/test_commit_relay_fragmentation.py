"""Fragmented protocol control for the disposable COMMIT relay."""

import importlib.util
import socket
import struct
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "relay", Path(__file__).with_name("pg_commit_relay.py")
)
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)


STARTUP_PARAMS = b"user\0postgres\0database\0nntune_billing_ack\0\0"
STARTUP = struct.pack("!II", len(STARTUP_PARAMS) + 8, 196608) + STARTUP_PARAMS


def frame(kind, body):
    return kind + struct.pack("!I", len(body) + 4) + body


def fragments(feed, data):
    return [message for byte in data for message in feed(bytes([byte]))]


def drained_transaction():
    exchange = relay.CommitExchange()
    exchange.frontend(STARTUP)
    exchange.backend(frame(b"Z", b"I"))
    exchange.frontend(frame(b"Q", b"BEGIN\0"))
    exchange.backend(frame(b"Z", b"T"))
    return exchange


class CommitRelayFragmentation(unittest.TestCase):
    def test_relay_rejects_frontend_queued_after_readiness_snapshot(self):
        client, frontend = socket.socketpair()
        server, backend = socket.socketpair()
        listener = Mock(accept=Mock(return_value=(client, None)))
        exchange = drained_transaction()
        exchange.frontend(frame(b"Q", b"COMMIT\0"))
        select = relay.select.select

        def snapshot(readers, *args):
            if readers == (listener,):
                return [listener], [], []
            ready = select(readers, *args)
            if ready[0] == [server]:
                frontend.sendall(b"Q")  # Arrives after the server-only snapshot.
            return ready

        with client, frontend, server, backend:
            backend.sendall(frame(b"C", b"COMMIT\0") + frame(b"Z", b"I"))
            with (
                patch.object(relay, "CommitExchange", return_value=exchange),
                patch.object(relay.socket, "create_connection", return_value=server),
                patch.object(relay.select, "select", side_effect=snapshot),
                self.assertRaisesRegex(relay.ProtocolError, "frontend data"),
            ):
                relay.run(listener, relay.threading.Event())
            self.assertTrue(exchange.withheld)

    def test_relay_exit_before_connection_releases_listener(self):
        with relay.commit_relay() as result:
            pass
        with self.assertRaises(relay.ProtocolError):
            result.result(timeout=1)
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", 15432))

    def test_relay_exit_during_exchange_closes_both_connections(self):
        upstream, backend = socket.socketpair()
        with backend, socket.socket() as client:
            backend.settimeout(2)
            client.settimeout(2)
            with patch.object(relay.socket, "create_connection", return_value=upstream):
                with relay.commit_relay() as result:
                    client.connect(("127.0.0.1", 15432))
                    client.sendall(STARTUP)
                    self.assertEqual(
                        backend.recv(len(STARTUP), socket.MSG_WAITALL), STARTUP
                    )
                with self.assertRaises(relay.ProtocolError):
                    result.result(timeout=1)
            self.assertEqual(backend.recv(1), b"")
            self.assertEqual(client.recv(1), b"")
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("127.0.0.1", 15432))

    def test_fragmented_commit_and_response_are_withheld(self):
        exchange = relay.CommitExchange()
        self.assertEqual(fragments(exchange.frontend, STARTUP), [STARTUP])
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
        exchange = drained_transaction()
        exchange.frontend(frame(b"Q", b"SELECT 1\0"))

        with self.assertRaises(relay.ProtocolError):
            exchange.frontend(frame(b"Q", b"COMMIT\0"))
        self.assertIsNone(exchange.gate)

    def test_commit_with_trailing_partial_frame_is_rejected(self):
        exchange = drained_transaction()

        with self.assertRaises(relay.ProtocolError):
            exchange.frontend(frame(b"Q", b"COMMIT\0") + b"Q")
        self.assertIsNone(exchange.gate)


if __name__ == "__main__":
    unittest.main()
