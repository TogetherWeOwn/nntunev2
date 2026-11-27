"""Exercise boundary readiness failure using loopback and simulated Docker commands."""

from http.server import BaseHTTPRequestHandler
from io import BytesIO
import json
from pathlib import Path
from socketserver import TCPServer
from subprocess import CompletedProcess
from tempfile import TemporaryDirectory
from threading import Thread
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, call, patch

import qualify
from contract import Boundary, KEY, OTHER, PATH, bulk_body, denied_matrix
import launcher
from render import render
from storage import Storage
from wire import request_bytes, response


class ReadinessTest(unittest.TestCase):
    def test_readiness_checks_isolation_before_probing(self):
        stack = object.__new__(qualify.Stack)
        stack.port = None
        stack.run = Mock(side_effect=["", "127.0.0.1:49152\n"])
        stack.verify_isolation = Mock(side_effect=RuntimeError("isolation failed"))
        with patch.object(qualify, "exchange", return_value=(403, {}, b"")) as exchange:
            with self.assertRaisesRegex(RuntimeError, "isolation failed"):
                stack.start()
        exchange.assert_not_called()
        self.assertEqual(stack.port, 49152)

    def test_compose_override_preserves_literal_bind_paths(self):
        with TemporaryDirectory() as scratch:
            source = Path(scratch) / 'literal-${FIXTURE_DATA_DIR}-"quote":dir'
            with patch.object(qualify, "RECORDING_CONFIG", source):
                stack = qualify.Stack(Path(scratch), recording=True)
            override = Path(stack.cmd[-1]).read_text()
        sources = [line.split("source: ", 1)[1] for line in override.splitlines() if "source: " in line]
        self.assertEqual([json.loads(item) for item in sources], [str(source).replace("$", "$$")])

    def test_invalid_port_and_container_ids_never_reach_consumers(self):
        stack = object.__new__(qualify.Stack)
        stack.port = None
        stack.scoped = ("Synthetic", "synthetic")
        stack.verify_isolation = Mock()
        for address in ("127.0.0.1:0", "127.0.0.1:65536", "127.0.0.1:+49152", "127.0.0.1:１２３"):
            with self.subTest(address=address), patch.object(qualify, "exchange", return_value=(403, {}, b"")) as exchange:
                stack.run = Mock(side_effect=["", address])
                with self.assertRaisesRegex(RuntimeError, "invalid boundary loopback port"):
                    stack.start()
                exchange.assert_not_called()
                self.assertIsNone(stack.port)
        del stack.verify_isolation
        stack.env = {}
        stack.run = Mock(side_effect=["--help", "a" * 64])
        with patch.object(qualify, "run", side_effect=RuntimeError("docker invoked")) as docker:
            with self.assertRaisesRegex(RuntimeError, "invalid disposable container IDs"):
                stack.verify_isolation()
            docker.assert_not_called()

    def test_process_failure_does_not_report_arguments(self):
        command = ["node", "synthetic-private-argument"]
        with patch.object(qualify.subprocess, "run", return_value=CompletedProcess(command, 1, b"", b"")):
            with self.assertRaisesRegex(RuntimeError, "^node command failed with exit 1$"):
                qualify.run(command, {})
        with patch.object(qualify.subprocess, "run", side_effect=OSError("synthetic-private-argument")):
            with self.assertRaisesRegex(RuntimeError, "^node command could not start$") as caught:
                qualify.run(command, {})
            self.assertTrue(caught.exception.__suppress_context__)

    def test_launcher_rejects_extra_output_after_success_token(self):
        stack = SimpleNamespace(env={}, scratch=Path("."))  # No files are opened by the mocks.
        boundary = Boundary(49152, ("Synthetic", "synthetic"))
        result = CompletedProcess([], 0, b"Private staging fixture provider preflight: qualified. extra\n", b"")
        with (
            patch.object(launcher, "run", side_effect=[None, result]),
            patch.object(launcher.ssl, "SSLContext"),
            patch.object(launcher.socketserver, "TCPServer"),
            patch.object(launcher.threading, "Thread"),
        ):
            with self.assertRaisesRegex(RuntimeError, "reviewed launcher did not qualify"):
                launcher.qualified_launcher(stack, Path("bundle.js"), boundary, "selected", "sibling")

    def test_relay_headers_cannot_exceed_byte_or_time_limits(self):
        with (
            patch.object(launcher, "run", side_effect=[None, RuntimeError("stop test")]),
            patch.object(launcher.ssl, "SSLContext") as tls,
            patch.object(launcher.socketserver, "TCPServer") as server,
            patch.object(launcher.threading, "Thread"),
            patch.object(launcher, "connect") as connect,
            patch.object(launcher, "read_to_close", return_value=b""),
        ):
            stack = SimpleNamespace(env={}, scratch=Path("."))
            with self.assertRaisesRegex(RuntimeError, "stop test"):
                launcher.qualified_launcher(stack, Path("bundle.js"), Boundary(49152, ("A", "B")), "v", "w")
            handler = server.call_args.args[1]
            client = tls.return_value.wrap_socket.return_value.__enter__.return_value
            for size, elapsed, extra in ((16_361, 0, b""), (1, 9, b""), (1, 0, b"x")):
                with self.subTest(size=size, elapsed=elapsed, extra=extra):
                    wire = b"HEAD / HTTP/1.1\r\nX: " + b"x" * size + b"\r\n\r\n" + extra
                    stream = BytesIO(wire)
                    client.makefile.return_value.__enter__.return_value = BytesIO(wire)
                    client.recv.side_effect = lambda limit: stream.read(min(limit, len(wire) - 2))
                    with patch.object(launcher, "monotonic", create=True, side_effect=(0, 0, elapsed)):
                        handler(Mock(), None, None)
                    connect.assert_not_called()
                connect.reset_mock()

    def test_version_is_carried_as_one_xml_value_and_one_query_parameter(self):
        version = "value&other=1"
        with self.subTest("body"):
            self.assertIn(b"<VersionId>value&amp;other=1</VersionId>", bulk_body(version))
        deny = (403, {"x-nntune-boundary": ["deny"], "cache-control": ["no-store"]}, b"")
        boundary = Mock(port=49152, credential=("Synthetic", "synthetic"))
        boundary.request.return_value = deny
        with patch("contract.exchange", return_value=deny):
            denied_matrix(boundary, version)
        query = "?versionId=value%26other%3D1"
        expected = {("GET", PATH + query), ("DELETE", OTHER + query), ("DELETE", PATH + query + "%")}
        self.assertEqual(expected - {c.args[:2] for c in boundary.request.call_args_list}, set())

    def test_renderer_requires_complete_template(self):
        for template in ("@ACCESS@ only", "@PATHS@ @ACCESS@ @ACCESS@"):
            with self.subTest(template=template), patch("render.Path.read_text", return_value=template):
                with self.assertRaises(ValueError):
                    render("Synthetic", [KEY])

    def test_explicit_empty_identity_does_not_use_default_credential(self):
        boundary = Boundary(49152, ("Synthetic", "synthetic"))
        storage = Storage(SimpleNamespace(admin=boundary.credential))
        with (
            patch("contract.exchange") as exchange,
            patch.object(storage, "request") as request,
        ):
            for call_request in (boundary.request, storage.s3):
                with self.subTest(call_request=call_request), self.assertRaises(ValueError):
                    call_request("GET", "/", identity=())
            exchange.assert_not_called()
            request.assert_not_called()

    def test_inventory_requires_complete_unambiguous_xml(self):
        uploads = b"<ListMultipartUploadsResult><IsTruncated>false</IsTruncated></ListMultipartUploadsResult>"
        for versions in (
            b"<Error><IsTruncated>false</IsTruncated></Error>",
            b"<ListVersionsResult><nested><IsTruncated>false</IsTruncated></nested></ListVersionsResult>",
            b"<ListVersionsResult><IsTruncated>false</IsTruncated><IsTruncated>true</IsTruncated></ListVersionsResult>",
            b"<ListVersionsResult><IsTruncated>false</IsTruncated><Version><Key>x</Key></Version></ListVersionsResult>",
        ):
            with self.subTest(versions=versions):
                storage = Storage(None)
                storage.s3 = Mock(side_effect=[(200, {}, versions), (200, {}, uploads)])
                with self.assertRaises(RuntimeError):
                    storage.inventory()

    def test_wire_rejects_malformed_http_evidence(self):
        for fields in (b"HTTP/1.2 403 Forbidden", b"HTTP/1.1 0403 Forbidden",
                       b"HTTP/1.1 403 Forbidden\r\nBad Name: value",
                       b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nContent-Length: 1",
                       b"HTTP/1.1 403 Forbidden\r\nX-Test: \x00"):
            with self.subTest(fields=fields), self.assertRaises(RuntimeError):
                response(fields + b"\r\n\r\n")
        for target, headers in (("/two words", []), ("/", [("X-Test", "line\n")])):
            with self.subTest(target=target, headers=headers), self.assertRaises(ValueError):
                request_bytes("GET", target, headers)

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
                patch.object(qualify.Stack, "verify_isolation", autospec=True),
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
