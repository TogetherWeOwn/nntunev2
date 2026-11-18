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


def startup(database):
    params = b"user\0postgres\0database\0" + database + b"\0\0"
    return struct.pack("!II", len(params) + 8, 196608) + params


STARTUP = startup(b"nntune_billing_ack")
NATIVE_STARTUP = startup(b"nntune_ci")


def frame(kind, body):
    return kind + struct.pack("!I", len(body) + 4) + body


def fragments(feed, data):
    return [message for byte in data for message in feed(bytes([byte]))]


def drained_transaction(exchange=None, startup_frame=STARTUP):
    if exchange is None:
        exchange = relay.CommitExchange()
    exchange.frontend(startup_frame)
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

    def test_billing_and_native_startup_targets_are_distinct(self):
        for exchange, wrong_startup in ((relay.CommitExchange(), NATIVE_STARTUP),
                                        (relay.NativeExchange(), STARTUP)):
            with self.assertRaises(relay.ProtocolError):
                exchange.frontend(wrong_startup)

    def test_native_guard_rejects_before_creating_listener(self):
        hosted = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
        for env in (
            {"NNTUNE_TEST_DATABASE_URL": relay.NATIVE_CI_URL},
            dict(hosted, NNTUNE_TEST_DATABASE_URL=relay.NATIVE_CI_URL + "?hostaddr=203.0.113.10"),
            dict(hosted, NNTUNE_TEST_DATABASE_URL=relay.NATIVE_CI_URL.replace("nntune_ci", relay.BILLING_DB)),
        ):
            with (
                self.subTest(env=env),
                patch.dict(relay.os.environ, env, clear=True),
                patch.object(relay.sys, "argv", ["relay", "--native-ci"]),
                patch.object(relay.socket, "socket") as listener,
                self.assertRaises(SystemExit),
            ):
                relay.native_main()
            listener.assert_not_called()

    def test_native_commit_tag_alone_is_not_proof(self):
        exchange = drained_transaction(relay.NativeExchange(), NATIVE_STARTUP)
        exchange.frontend(frame(b"Q", b"COMMIT\0"))
        self.assertEqual(fragments(exchange.backend, frame(b"C", b"COMMIT\0")), [])
        self.assertFalse(exchange.withheld)
        self.assertEqual(fragments(exchange.backend, frame(b"Z", b"I")), [])
        self.assertTrue(exchange.withheld)

    def test_native_rejects_rollback_tag_without_tripping_fault(self):
        exchange = drained_transaction(relay.NativeExchange(), NATIVE_STARTUP)
        exchange.frontend(frame(b"Q", b"COMMIT\0"))
        rollback, ready = frame(b"C", b"ROLLBACK\0"), frame(b"Z", b"I")
        self.assertEqual(fragments(exchange.backend, rollback), [rollback])
        self.assertFalse(exchange.rejection_drained)
        self.assertEqual(fragments(exchange.backend, ready), [ready])
        self.assertTrue(exchange.rejection_drained)
        self.assertFalse(exchange.withheld)

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
