#!/usr/bin/env python3
import hashlib
import runpy
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from dataclasses import replace
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from threading import Event, Thread, Timer
from unittest.mock import patch
from urllib.parse import parse_qs, quote_plus, urlsplit

runner = runpy.run_path(str(Path(__file__).with_name("subsonic-staging-runner.py")))
StagingModel, matrix, rehearse = (runner[key] for key in ("StagingModel", "MATRIX", "rehearse"))
BASELINE, EXPERIMENT = runner["BASELINE"], runner["EXPERIMENT"]
NEWER = replace(BASELINE, deployment_id="deploy-newer", version="newer", serving_version="newer")


class RecordingModel(StagingModel):
    delays = {}

    def __init__(self):
        super().__init__()
        self.starts = []
        self.cleanup_started_at = None

    def cancel_clients(self):
        self.cleanup_started_at = self.now
        super().cancel_clients()

    def start(self, slot):
        number = slot["slot"]
        self.starts.append((number, self.now))
        super().start(slot)
        if number in self.delays:
            self.pending[number] = self.now + self.delays[number]


class RequestContractTests(unittest.TestCase):
    bearer = "synthetic-bearer"
    api_key = "disposable+key/=with&reserved" * 16

    def test_posts_use_staging_and_separate_credentials(self):
        posts = list(runner["prepare_requests"](matrix, self.bearer, self.api_key))
        slots = runner["load_matrix"](matrix)["slots"]
        self.assertEqual([post.slot for post in posts], list(range(1, 49)))
        for post, slot in zip(posts, slots):
            with self.subTest(slot=post.slot):
                headers = dict(post.headers)
                url = urlsplit(post.url)
                self.assertEqual(post.method, "POST")
                self.assertEqual(
                    (url.scheme, url.netloc), ("https", "staging.nntune.com")
                )
                self.assertEqual(
                    (url.path, url.query, url.fragment),
                    (f"/rest/{slot['endpoint']}", "", ""),
                )
                self.assertEqual(
                    headers["Content-Type"], "application/x-www-form-urlencoded"
                )
                self.assertEqual(headers["Accept-Encoding"], "identity")
                self.assertEqual(headers["X-Subsonic-Measurement-Case"], slot["case"])
                auth = None if post.slot == 5 else f"Bearer {self.bearer}"
                self.assertEqual(headers.get("Authorization"), auth)
                self.assertNotIn(self.api_key, repr(post))
                self.assertNotIn(self.bearer, repr(post))
                self.assertLessEqual(len(post.body), 8192)
                params = {key: [value] for key, value in slot["params"].items()}
                if post.slot not in (5, 6):
                    params["apiKey"] = [self.api_key]
                self.assertEqual(
                    parse_qs(
                        post.body.decode("ascii"),
                        keep_blank_values=True,
                        strict_parsing=True,
                    ),
                    params,
                )

    def test_missing_or_unsafe_credentials_and_changed_matrix_refuse(self):
        prepare = runner["prepare_requests"]
        for bearer, key in (
            ("", self.api_key),
            ("a\r\nb", self.api_key),
            ("a" * 1025, self.api_key),
            (self.bearer, "a" * 31),
            (self.bearer, "a" * 513),
            (self.bearer, "a" * 32 + "雪"),
        ):
            with self.assertRaisesRegex(runner["Stop"], "^credential-shape$"):
                prepare(matrix, bearer, key)
        with tempfile.TemporaryDirectory() as tmp:
            tampered = Path(tmp) / "matrix.json"
            tampered.write_text(
                matrix.read_text().replace('"getAlbum"', '"unreviewed"', 1)
            )
            with self.assertRaisesRegex(runner["Stop"], "^unreviewed-matrix$"):
                prepare(tampered, self.bearer, self.api_key)

    def test_api_key_respects_worker_encoded_value_limit(self):
        prepare = runner["prepare_requests"]
        key = "+" * 341 + "a"  # 1024 encoded bytes, not including the field name.
        post = next(prepare(matrix, self.bearer, key))
        self.assertEqual(parse_qs(post.body.decode("ascii"))["apiKey"], [key])
        with self.assertRaisesRegex(runner["Stop"], "^credential-shape$"):
            prepare(matrix, self.bearer, key + "a")

    def test_fixed_rejections_require_exact_application_response(self):
        classify = runner["classify_fixed_rejection"]
        slots = runner["load_matrix"](matrix)["slots"]
        off = (
            b'<subsonic-response status="failed" version="1.16.1"><error code="70" '
            b'message="Endpoint not supported in staging."/></subsonic-response>'
        )
        unavailable = (
            b'{"subsonic-response":{"status":"failed","version":"1.16.1",'
            b'"error":{"code":0,"message":"Service unavailable."}}}'
        )
        oversize = unavailable.replace(
            b"Service unavailable.", b"Request or document too large."
        )
        oversize_xml = off.replace(b"70", b"0").replace(
            b"Endpoint not supported in staging.",
            b"Request or document too large.",
        )
        for number, status, fmt, retry, body in (
            (5, 404, "xml", None, off),
            *((n, 503, "json", "1", unavailable) for n in (9, 12, 15)),
            (18, 413, "json", None, oversize),
            (21, 413, "xml", None, oversize_xml),
            (24, 413, "json", None, oversize),
        ):
            slot = slots[number - 1]
            args = [status, body, f"application/{fmt}", "private, no-store", retry]
            with self.subTest(slot=number):
                self.assertEqual(classify(slot, *args), slot["expected"])
                for i, replacement in enumerate(
                    (302, b"x" * len(body), "text/javascript", "public", "2")
                ):
                    bad = args.copy()
                    bad[i] = replacement
                    with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
                        classify(slot, *bad)
                if retry is not None:
                    with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
                        classify(slot, *args[:-1], None)
        with self.assertRaisesRegex(runner["Stop"], "^body-limit$"):
            classify(
                slots[23],
                413,
                b"x" * 1025,
                "application/json",
                "private, no-store",
                None,
            )
        with self.assertRaisesRegex(runner["Stop"], "^response-mismatch$"):
            classify(slots[36], 503, unavailable, "application/json", "private, no-store", "1")


class TransportTests(unittest.TestCase):
    bearer = r'synthetic"\-bearer'
    api_key = RequestContractTests.api_key
    access_client_id = "fixture-client-id.access"
    access_client_secret = "fixture-client-secret"
    ok_head = b"HTTP/1.1 200 OK\r\n\r\n"

    def setUp(self):
        self.children = []
        self.cancel = Event()
        self.transport = runner["StagingPostTransport"](spawn=self.fake_curl)

    def fake_curl(self, argv, **options):
        self.assertEqual(options, {"stdin": subprocess.PIPE, "stdout": subprocess.PIPE,
                                   "stderr": subprocess.DEVNULL, "env": {}, "close_fds": True})
        self.argv = argv
        code = "import os, sys, time\nconfig = sys.stdin.buffer.read()\nout = sys.stdout.buffer\n"
        child = subprocess.Popen([sys.executable, "-I", "-c", code + self.program], **options)
        self.children.append(child)
        self.addCleanup(self.reap_on_failure, child)
        return child

    def reap_on_failure(self, child):
        child.kill()  # Popen does not signal an already-reaped process.
        child.wait(timeout=2)
        child.stdin.close()
        child.stdout.close()

    def send(self, slot=1, seconds=4):
        return self.transport.send(
            slot, self.bearer, self.api_key, deadline=time.monotonic() + seconds,
            cancel=self.cancel, access_client_id=self.access_client_id,
            access_client_secret=self.access_client_secret,
        )

    def assert_reaped(self, returncode=None):
        child = self.children[-1]
        self.assertTrue(child.returncode is not None and child.stdin.closed and child.stdout.closed)
        if returncode is not None:
            self.assertEqual(child.returncode, returncode)

    def test_fixed_https_post_passes_credentials_only_on_stdin(self):
        off = (
            b"HTTP/1.1 404 Not Found\r\nContent-Type: application/xml\r\n"
            b'Cache-Control: private, no-store\r\n\r\n<subsonic-response status="failed" version="1.16.1">'
            b'<error code="70" message="Endpoint not supported in staging."/></subsonic-response>'
        )
        for slot, case, has_bearer, form, reply in (
            (1, "empty", True, f"f=json&apiKey={quote_plus(self.api_key)}", self.ok_head),
            (5, "small", False, "f=json", off),
            (6, "small", True, "f=json", self.ok_head),
        ):
            with self.subTest(slot=slot):
                lines = [
                    'header = "Content-Type: application/x-www-form-urlencoded"',
                    'header = "Accept-Encoding: identity"',
                    f'header = "X-Subsonic-Measurement-Case: {case}"',
                ]
                if has_bearer:
                    lines.append(r'header = "Authorization: Bearer synthetic\"\\-bearer"')
                lines += [
                    f'header = "CF-Access-Client-Id: {self.access_client_id}"',
                    f'header = "CF-Access-Client-Secret: {self.access_client_secret}"',
                    f'data-binary = "{form}"',
                ]
                digest = hashlib.sha256(("\n".join(lines) + "\n").encode()).hexdigest()
                self.program = (
                    f"import hashlib\nassert hashlib.sha256(config).hexdigest() == {digest!r}\n"
                    f"out.write({reply!r})"
                )
                result = self.send(slot)
                self.assertEqual(self.argv, (
                    "/usr/bin/curl", "--disable", "--globoff", "--proto", "=https", "--noproxy", "*",
                    "--no-location", "--retry", "0", "--http1.1", "--silent", "--no-buffer", "--include",
                    "--ignore-content-length", "--header", "Connection: close",
                    "--connect-timeout", "2.0", "--max-time", "10.0", "--request", "POST", "--config", "-",
                    "--url", "https://staging.nntune.com/rest/getArtists",
                ))
                if slot == 5:
                    self.assertEqual(result, {"oracle": "fixed_staging_off", "http_status": 404})
                else:
                    self.assertEqual(repr(result), "PostResponse(status=200)")
                    self.assertEqual(result.body, b"")
                self.assert_reaped(0)

    def test_missing_or_malformed_access_never_spawns(self):
        valid = {"access_client_id": self.access_client_id,
                 "access_client_secret": self.access_client_secret}
        bad = (None, "", b"fixture", "has space", "a\r\nb", "a\tb", "a\x00b", "snow-雪",
               '"quoted"', r"a\b", "a" * 513)
        for field in valid:
            for value in bad:
                with self.subTest(field=field, value=value), self.assertRaisesRegex(
                    runner["Stop"], "^access-credential-shape$",
                ):
                    self.transport.send(1, self.bearer, self.api_key, deadline=time.monotonic() + 4,
                                        cancel=self.cancel, **(valid | {field: value}))
        ambient = {"CF_ACCESS_CLIENT_ID": self.access_client_id,
                   "CF_ACCESS_CLIENT_SECRET": self.access_client_secret}
        with (patch("os.environ", ambient),
              self.assertRaisesRegex(runner["Stop"], "^access-credential-shape$")):
            self.transport.send(1, self.bearer, self.api_key, deadline=time.monotonic() + 4,
                                cancel=self.cancel)
        self.assertEqual(self.children, [])

    def test_rejects_unreviewed_slot_and_alternative_destinations_before_spawn(self):
        for slot in (0, 49, True, "https://example.invalid/rest/getArtists"):
            with self.assertRaisesRegex(runner["Stop"], "^unreviewed-slot$"):
                self.send(slot)
        original = next(runner["prepare_requests"](matrix, self.bearer, self.api_key))
        for url in (
            original.url.replace("https:", "http:"),
            original.url.replace("staging.nntune.com", "example.invalid"),
            original.url.replace("getArtists", "getAlbum"), original.url + "?q=1", original.url + "#x",
        ):
            with (patch.dict(runner["_prepare_post"].__globals__, _prepare_post=lambda *_, url=url: replace(original, url=url)),
                  self.assertRaisesRegex(runner["Stop"], "^destination-refused$")):
                self.send()
        self.assertEqual(self.children, [])

    def test_access_denial_and_redirect_are_terminal(self):
        for status, reason in ((302, "redirect-refused"), (307, "redirect-refused"),
                               (401, "access-refused"), (403, "access-refused")):
            # A proxy reply may contain arbitrary text. Neither the location
            # nor the echoed stdin should be carried out of a failed request.
            head = (f"HTTP/1.1 {status} Refused\r\n"
                    "Location: https://example.invalid/private\r\n\r\n").encode()
            self.program = f"out.write({head!r} + config); out.flush(); time.sleep(60)"
            for slot in (1, 5, 6):
                with self.subTest(status=status, slot=slot):
                    previous = len(self.children)
                    with self.assertRaisesRegex(runner["Stop"], f"^{reason}$"):
                        self.send(slot)
                    self.assertEqual(len(self.children), previous + 1)
                    self.assert_reaped(-signal.SIGKILL)

    def test_reflected_credentials_are_not_returned(self):
        self.bearer = "fixture-worker-bearer"
        # Reflect exactly one value per reply, so no check can mask a missing one.
        for marker, decode, metadata in (
            (b"CF-Access-Client-Id: ", False, True),
            (b"CF-Access-Client-Secret: ", False, False),
            (b"Authorization: Bearer ", False, False),
            (b"&apiKey=", False, False),
            (b"&apiKey=", True, False),
        ):
            with self.subTest(marker=marker, decode=decode):
                head = b"HTTP/1.1 200 OK\r\nCache-Control: " if metadata else self.ok_head
                end = b"\r\n\r\n" if metadata else b""
                self.program = (
                    "from urllib.parse import unquote_to_bytes\n"
                    f"value = config.split({marker!r}, 1)[1].split(b'\"', 1)[0]\n"
                    f"if {decode}: value = unquote_to_bytes(value)\n"
                    f"out.write({head!r} + value + {end!r})"
                )
                with self.assertRaisesRegex(runner["Stop"], "^credential-reflected$"):
                    self.send()
                self.assert_reaped(0)

    def test_interim_headers_preserve_fixed_rejection_oracle_and_shared_budget(self):
        hint = b"HTTP/1.1 103 Early Hints\r\n\r\n"
        head = (hint + b"HTTP/1.1 404 Not Found\r\nContent-Type: application/xml\r\n"
                b"Cache-Control: private, no-store\r\n\r\n")
        off = (b'<subsonic-response status="failed" version="1.16.1"><error code="70" '
               b'message="Endpoint not supported in staging."/></subsonic-response>')
        self.program = f"out.write({(head + off)!r})"
        self.assertEqual(self.send(5), {"oracle": "fixed_staging_off", "http_status": 404})
        for payload, reason in ((head + b"wrong body", "response-mismatch"),
                                (hint * 400 + head + off, "response-headers"),
                                (b"HTTP/1.1 101 Switching Protocols\r\n\r\n", "response-headers")):
            self.program = f"out.write({payload!r})"
            with self.assertRaisesRegex(runner["Stop"], f"^{reason}$"):
                self.send(5)
            self.assert_reaped()

    def test_real_curl_reports_both_content_length_mismatches(self):
        class Fixture(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                self.wfile.write(self.server.response)

        with HTTPServer(("127.0.0.1", 0), Fixture) as server:
            # Only this trusted test hook substitutes loopback HTTP after URL validation.
            def loopback(argv, **options):
                local = (*argv[:-1], f"http://127.0.0.1:{server.server_port}/fixture")
                return subprocess.Popen(["=http" if arg == "=https" else arg for arg in local], **options)

            self.transport = runner["StagingPostTransport"](spawn=loopback)
            server.timeout = 2
            for length in (2, 1, 3):
                server.response = f"HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\nxy".encode()
                worker = Thread(target=server.handle_request, daemon=True)
                worker.start()
                try:
                    if length == 2:
                        self.assertEqual(self.send().body, b"xy")
                    else:
                        with self.assertRaisesRegex(runner["Stop"], "^transport-incomplete$"):
                            self.send()
                finally:
                    worker.join(timeout=2)
                    self.assertFalse(worker.is_alive())

    def test_response_limits_and_framing(self):
        cap = runner["load_matrix"](matrix)["limits"]["body_bytes"]
        self.program = f"out.write({self.ok_head!r} + b'x' * {cap})"
        self.assertEqual(len(self.send().body), cap)
        busy = b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\n"
        self.program = f"out.write({(busy + b'Content-Length: 1024')!r} + b'\\r\\n\\r\\n' + b'x' * 1024)"
        result = self.send(37)
        self.assertEqual((result.status, len(result.body), result.retry_after), (503, 1024, "1"))
        for slot, head, size, reason in (
            (1, self.ok_head, cap + 1, "body-limit"),
            (37, busy + b"\r\n", 1025, "body-limit"),
            (37, busy + b"Content-Length: 1025\r\n\r\n", 0, "body-limit"),
            (1, b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n", 1, "response-headers"),
            (1, b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\ncontent-length: 1\r\n\r\n", 1, "response-headers"),
        ):
            with self.subTest(slot=slot, size=size, reason=reason):
                self.program = f"out.write({head!r} + b'x' * {size})"
                with self.assertRaisesRegex(runner["Stop"], f"^{reason}$"):
                    self.send(slot)
                self.assert_reaped()

    def test_runner_deadline_and_curl_exit_28_are_both_timeouts(self):
        head = f"out.write({self.ok_head!r}); out.flush()\n"
        for program, seconds, returncode in (
            (head + "os.close(sys.stdout.fileno()); time.sleep(60)", .35, -signal.SIGKILL),
            (head + "while True: out.write(b'x'); out.flush(); time.sleep(.02)", .35, -signal.SIGKILL),
            ("sys.exit(28)", 4, 28)):
            self.program = program
            with self.assertRaisesRegex(runner["Stop"], "^request-or-reader-timeout$"):
                self.send(seconds=seconds)
            self.assert_reaped(returncode)

    def test_cancellation_reaps_live_client_even_during_hold(self):
        self.program = "time.sleep(60)"
        self.cancel.set()
        with self.assertRaisesRegex(runner["Stop"], "^cancelled$"):
            self.send()
        self.assertEqual(self.children, [])
        self.cancel.clear()

        def spawn_then_cancel(*args, **kwargs):
            child = self.fake_curl(*args, **kwargs)
            timer = Timer(.05, self.cancel.set)
            timer.start()
            self.addCleanup(timer.join)
            return child

        self.transport = runner["StagingPostTransport"](spawn=spawn_then_cancel)
        with self.assertRaisesRegex(runner["Stop"], "^cancelled$"):
            self.send(41)
        self.assert_reaped(-signal.SIGKILL)

    def test_held_reader_and_disconnect_are_distinct_client_outcomes(self):
        self.program = f"out.write({self.ok_head!r} + b'fixture')"
        start = time.monotonic()
        self.assertEqual(self.send(41).body, b"fixture")
        self.assertGreaterEqual(time.monotonic() - start, 1.5)
        self.assertIn("5.0", self.argv)
        self.assert_reaped(0)
        with self.assertRaisesRegex(runner["Stop"], "^disconnect-unconfirmed$"):
            self.send(45)  # Already exited: never claim we disconnected it.
        self.assert_reaped(0)
        self.program = f"out.write({self.ok_head!r}); out.flush(); out.write(b'x' * 106659); time.sleep(60)"
        self.assertEqual(self.send(45), {"oracle": "client-ended"})
        self.assert_reaped(-signal.SIGKILL)


class RunnerTests(unittest.TestCase):
    def test_reviewed_schedule(self):
        model = RecordingModel()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["cleanup"]), ("complete", "simulated-off-and-drained"))
        self.assertEqual([number for number, _ in model.starts], list(range(1, 49)))
        self.assertTrue(all(b - a >= 1000 for (_, a), (_, b) in zip(model.starts, model.starts[1:])))
        self.assertEqual([row["state"] for row in report["slots"].values()], ["accepted"] * 48)
        self.assertEqual(report["simulated_pair_overlap"],
                         {1: False, 2: False, 3: True, 4: True, 5: True, 6: True})
        self.assertEqual(report["max_simulated_clients"], 2)
        self.assertEqual(report["max_simulated_backends"], 2)
        self.assertLess(model.now, 120000)

    def test_modified_matrix_never_arms(self):
        for raw in (matrix.read_text().replace('"in_flight": 2', '"in_flight": 3'),
                    matrix.read_text().replace('"in_flight": 2', '"in_flight": 99, "in_flight": 2')):
            with tempfile.TemporaryDirectory() as tmp:
                tampered = Path(tmp) / "matrix.json"
                tampered.write_text(raw)
                model = RecordingModel()
                report = rehearse(tampered, model)
                self.assertEqual((report["stop"], report["cleanup"]), ("unreviewed-matrix", "not-armed"))
                self.assertEqual(model.readback(), BASELINE)
                self.assertEqual(model.starts, [])

    def test_telemetry_loss_and_pressure_stop_before_next_start(self):
        for starts, observation, reason in ((0, (-1, True, 0), "telemetry-stale-or-unsafe"),
                                            (1, (50, True, 3), "backend-pressure"),
                                            (1, (50, False, 1), "telemetry-stale-or-unsafe")):
            class Unsafe(RecordingModel):
                def telemetry(self):
                    return observation if self.state.lease and len(self.starts) >= starts else super().telemetry()

            report = rehearse(matrix, Unsafe())
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["max_simulated_backends"], 0 if starts else None)
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_pair_busy_requires_attribution_and_is_bounded(self):
        for fmt, size, attributed, delay, stop in (
            ("json", 112, False, 100, "unattributed-pair-result"),
            ("xml", 120, False, 250, "unattributed-pair-result"),
            ("xml", 120, True, 250, "complete"), ("xml", 120, True, 300, "busy-latency"),
        ):
            class Busy(RecordingModel):
                delays = {38: delay}

                def result(self, slot):
                    result = super().result(slot)
                    return {"oracle": f"fixed_capacity_{fmt}", "http_status": 503,
                            "body_bytes": size} if result and slot["slot"] == 38 else result

                def busy_observed(self, slot):
                    return attributed and slot["slot"] == 38

            report = rehearse(matrix, Busy())
            accepted = stop == "complete"
            self.assertEqual((report["stop"], report["starts"]), (stop, 48 if accepted else 38))
            self.assertEqual(report["slots"][38]["state"], "accepted" if accepted else "rejected")
            self.assertEqual(report["slots"][39]["state"], "accepted" if accepted else "unstarted")

    def test_reader_unfinished_at_five_seconds_stops_and_cleans_up(self):
        model = RecordingModel()
        model.delays = {41: 6000, 42: 6000}
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("request-or-reader-timeout", 42))
        self.assertEqual(model.cleanup_started_at - dict(model.starts)[41], 5000)
        self.assertEqual(report["simulated_pair_overlap"], {1: False, 2: False, 3: True})
        self.assertEqual([report["slots"][i]["state"] for i in (41, 42, 43)],
                         ["unfinished", "unfinished", "unstarted"])
        self.assertEqual(report["cleanup"], "simulated-off-and-drained")

    def test_partial_arm_and_cancellation_restore(self):
        class PartialArm(RecordingModel):
            def arm(self):
                super().arm()
                raise RuntimeError("not part of the report")

        class Cancelled(RecordingModel):
            def tick(self):
                if self.state == EXPERIMENT:
                    raise KeyboardInterrupt
                super().tick()

        for model, reason, starts in (
            (PartialArm(), "model-failure", 0), (Cancelled(), "cancelled", 1),
        ):
            report = rehearse(matrix, model)
            self.assertEqual((report["stop"], report["starts"]), (reason, starts))
            self.assertEqual(report["cleanup"], "simulated-off-and-drained")
            self.assertTrue(model.state.verified_off())
            self.assertEqual(model.state.version, BASELINE.version)

    def test_wrong_baseline_or_serving_version_cannot_start(self):
        for state in (NEWER, replace(BASELINE, measurement_binding=True),
                      replace(BASELINE, serving_version=EXPERIMENT.version)):
            model = RecordingModel()
            model.state = state
            report = rehearse(matrix, model)
            self.assertEqual((report["stop"], report["starts"], report["cleanup"]),
                             ("preflight-not-off-and-idle", 0, "not-armed"))

        class WrongServing(RecordingModel):
            def arm(self):
                self.state = replace(EXPERIMENT, serving_version=BASELINE.version)

        report = rehearse(matrix, WrongServing())
        self.assertEqual((report["stop"], report["starts"], report["cleanup"]),
                         ("serving-version-unverified", 0, "simulated-off-and-drained"))

    def test_restore_compares_deployment_identity_at_write(self):
        class Race(RecordingModel):
            def restore_if_current(self, expected_deployment_id):
                self.state = self.successor
                return super().restore_if_current(expected_deployment_id)

        for successor, expected in (
            (NEWER, "simulated-superseded-off-and-drained"),
            (replace(EXPERIMENT, deployment_id=NEWER.deployment_id), "unverified"),
        ):
            model = Race()
            model.successor = successor
            self.assertEqual(rehearse(matrix, model)["cleanup"], expected)
            self.assertEqual(model.state, successor)

    def test_missing_fence_or_uncertain_off_state_leaves_cleanup_unverified(self):
        class Uncertain(RecordingModel):
            def restore_if_current(self, expected_deployment_id):
                self.state = self.successor

        for state in (
            EXPERIMENT,
            replace(NEWER, enabled=True),
            replace(NEWER, lease=True),
            replace(NEWER, measurement_binding=True),
            replace(NEWER, serving_version=EXPERIMENT.version),
        ):
            model = Uncertain()
            model.successor = state
            self.assertEqual(rehearse(matrix, model)["cleanup"], "unverified")
            self.assertEqual(model.state, state)

    def test_restore_requires_drain_sample_after_off_readback(self):
        class DrainFence(RecordingModel):
            off_read_at = None
            zero_sample_at = None

            def readback(self):
                state = super().readback()
                if (self.cleanup_started_at is not None and state.verified_off()
                        and self.off_read_at is None):
                    self.off_read_at = self.now
                return state

            def telemetry(self):
                at, healthy, backends = super().telemetry()
                if (self.cleanup_started_at is not None and healthy and backends == 0
                        and self.zero_sample_at is None):
                    self.zero_sample_at = at
                return at, healthy, backends

        model = DrainFence()
        report = rehearse(matrix, model)
        self.assertEqual(report["cleanup"], "simulated-off-and-drained")
        self.assertIsNotNone(model.off_read_at)
        self.assertIsNotNone(model.zero_sample_at)
        self.assertGreater(model.zero_sample_at, model.off_read_at)

    def test_deployment_change_during_drain_invalidates_off_readback(self):
        class ChangedDuringDrain(RecordingModel):
            def telemetry(self):
                if self.state.verified_off() and self.starts:
                    self.state = replace(NEWER, measurement_binding=True)
                return super().telemetry()

        self.assertEqual(rehearse(matrix, ChangedDuringDrain())["cleanup"], "unverified")

    def test_client_cancellation_does_not_prove_backend_drain(self):
        class Lingering(RecordingModel):
            def telemetry(self):
                return self.now, True, int(bool(self.starts))

        model = Lingering()
        report = rehearse(matrix, model)
        self.assertEqual((report["stop"], report["starts"]), ("clock-or-window", 1))
        self.assertEqual(report["cleanup"], "unverified")
        self.assertTrue(model.state.verified_off())
        self.assertEqual(model.state.version, BASELINE.version)


if __name__ == "__main__":
    unittest.main()
