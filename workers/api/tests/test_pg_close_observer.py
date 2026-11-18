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
    def test_client_eof_is_observed(self):
        upstream, peer = socket.socketpair()
        with (
            peer,
            patch.object(observer.socket, "create_connection", return_value=upstream),
            close_observer() as (closed, result),
        ):
            with socket.socket() as client:
                client.connect(("127.0.0.1", 15433))
                client.sendall(b"probe")
                self.assertEqual(peer.recv(5), b"probe")
                self.assertFalse(closed.is_set())
            result.result(timeout=2)
            self.assertTrue(closed.is_set())
            peer.settimeout(2)
            self.assertEqual(peer.recv(1), b"")

    def test_upstream_eof_is_not_client_eof(self):
        upstream, peer = socket.socketpair()
        with (
            peer,
            patch.object(observer.socket, "create_connection", return_value=upstream),
            close_observer() as (closed, result),
            socket.socket() as client,
        ):
            client.connect(("127.0.0.1", 15433))
            client.sendall(b"probe")
            self.assertEqual(peer.recv(5), b"probe")
            peer.close()
            result.result(timeout=2)
            self.assertFalse(closed.is_set())


if __name__ == "__main__":
    unittest.main()
