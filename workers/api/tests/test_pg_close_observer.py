"""Controls for the disposable client-EOF observer."""

import importlib.util
import socket
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "pg_close_observer", Path(__file__).with_name("pg_close_observer.py")
)
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
close_observer = observer.close_observer


class CloseObserverTest(unittest.TestCase):
    def assert_receives(self, receiver, expected):
        received = bytearray()
        while len(received) < len(expected):
            chunk = receiver.recv(len(expected) - len(received))
            self.assertTrue(chunk, f"unexpected EOF while reading {expected!r}")
            received.extend(chunk)
        self.assertEqual(received, expected)

    def test_client_eof_is_observed(self):
        upstream, peer = socket.socketpair()
        with (
            peer,
            patch.object(observer.socket, "create_connection", return_value=upstream),
            close_observer() as (closed, result),
        ):
            peer.settimeout(2)
            with socket.socket() as client:
                client.settimeout(2)
                client.connect(("127.0.0.1", 15433))
                client.sendall(b"probe")
                self.assert_receives(peer, b"probe")
                peer.sendall(b"reply")
                self.assert_receives(client, b"reply")
                self.assertFalse(closed.is_set())
            result.result(timeout=2)
            self.assertTrue(closed.is_set())
            self.assertEqual(peer.recv(1), b"")

    def test_upstream_eof_is_not_client_eof(self):
        upstream, peer = socket.socketpair()
        with (
            peer,
            patch.object(observer.socket, "create_connection", return_value=upstream),
            close_observer() as (closed, result),
            socket.socket() as client,
        ):
            peer.settimeout(2)
            client.settimeout(2)
            client.connect(("127.0.0.1", 15433))
            client.sendall(b"probe")
            self.assert_receives(peer, b"probe")
            peer.close()
            result.result(timeout=2)
            self.assertFalse(closed.is_set())


if __name__ == "__main__":
    unittest.main()
