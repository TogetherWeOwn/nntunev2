"""Exercise boundary readiness failure using loopback and simulated Docker commands."""

from http.server import BaseHTTPRequestHandler
from socketserver import TCPServer
from threading import Thread
from types import SimpleNamespace
import unittest
from unittest.mock import call, patch

import qualify
from storage import Storage


class ReadinessTest(unittest.TestCase):
    def test_boundary_readiness_timeout_stops_and_disposes(self):
        probes = []

        class UnreadyBoundary(BaseHTTPRequestHandler):
            def do_GET(self):
                probes.append((self.command, self.path, sorted(self.headers.items())))
                self.send_response_only(503)
                self.end_headers()

        with TCPServer(("127.0.0.1", 0), UnreadyBoundary) as boundary:
            port = boundary.server_address[1]
            worker = Thread(target=boundary.serve_forever, kwargs={"poll_interval": 0.01})
            worker.start()

            def stop_boundary():
                boundary.shutdown()
                worker.join()
                boundary.server_close()

            def simulate_docker(command, _env, **_options):
                if command[-3:] == ["port", "boundary", "8080"]:
                    return SimpleNamespace(stdout=f"127.0.0.1:{port}\n".encode())
                if command[-3:] == ["down", "--volumes", "--remove-orphans"]:
                    stop_boundary()
                return SimpleNamespace(stdout=b"")

            # Keep the real retry count and HTTP exchange, but skip the pauses.
            with (
                patch.object(qualify, "time") as clock,
                patch.object(qualify, "run", side_effect=simulate_docker) as docker,
                patch.object(qualify, "connect", wraps=qualify.connect) as verify_closed_port,
                patch.object(Storage, "provision", autospec=True,
                             return_value=("FixturePlaceholder", "synthetic-placeholder")) as provision,
                patch.object(Storage, "qualify", autospec=True) as evaluate,
            ):
                clock.sleep.side_effect = [None] * 80
                try:
                    with self.assertRaisesRegex(RuntimeError, "^boundary did not start$"):
                        qualify.qualify(recording=False, bundle=None)
                finally:
                    stop_boundary()

        self.assertEqual(provision.call_count, 1)
        storage, = provision.call_args.args
        stack = storage.stack
        evaluate.assert_not_called()
        probe = ("GET", "/", [("Connection", "close"), ("Host", "fixtures.invalid")])
        self.assertEqual(probes, [probe] * 80)
        self.assertEqual(clock.sleep.call_args_list, [call(0.25)] * 80)

        label = f"label=com.docker.compose.project={stack.project}"
        self.assertEqual([invocation.args[0] for invocation in docker.call_args_list], [
            [*stack.cmd, "up", "-d", "backend"],
            [*stack.cmd, "up", "-d", "--wait", "--wait-timeout", "25"],
            [*stack.cmd, "port", "boundary", "8080"],
            [*stack.cmd, "down", "--volumes", "--remove-orphans"],
            [*qualify.DOCKER, "container", "ls", "-q", "--all", "--filter", label],
            [*qualify.DOCKER, "network", "ls", "-q", "--filter", label],
            [*qualify.DOCKER, "volume", "ls", "-q", "--filter", label],
        ])
        verify_closed_port.assert_called_once_with(port)
        self.assertFalse(stack.scratch.exists())
