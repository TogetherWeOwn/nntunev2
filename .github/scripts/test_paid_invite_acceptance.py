"""Exercise the runner entry point with disposable HTTP and Stripe fixtures."""

import contextlib
import http.client
import io
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from unittest.mock import patch

import paid_invite_acceptance as runner


SHA = "6" * 40
SESSION = "cs_test_privateSession"
ACCOUNT = "acct_privateAccount"
SUBSCRIPTION = "sub_privateSubscription"
SUBSCRIPTION_PATH = f"/v1/subscriptions/{SUBSCRIPTION}"
ENV = {
    "NNTUNE_PAID_INVITE_ACCEPTANCE": "1", "EXPECTED_BUILD_SHA": SHA,
    "CF_ACCESS_CLIENT_ID": "privateAccessId", "CF_ACCESS_CLIENT_SECRET": "privateAccessSecret",
    "PAID_INVITE_STRIPE_READ_KEY": "rk_test_x",
}


class AcceptanceTest(unittest.TestCase):
    def setUp(self):
        self.evidence = {
            "app": "nntune-v2", "environment": "staging", "origin": "https://staging.nntune.com",
            "build": SHA, "stripe_account": ACCOUNT, "checkout_session": SESSION,
            "checks": dict.fromkeys(runner.PREREQUISITES + runner.JOURNEY, SHA),
        }
        self.routes = {
            "/api/build": {"sha": SHA},
            "/healthz": {"ok": True, "build_sha": SHA},
            "/v1/balance": {"object": "balance", "livemode": False},
            "/v1/account": {"object": "account", "id": ACCOUNT},
            f"/v1/checkout/sessions/{SESSION}": {
                "object": "checkout.session", "id": SESSION, "livemode": False,
                "mode": "subscription", "status": "complete", "payment_status": "paid",
                "amount_total": 1500, "subscription": SUBSCRIPTION,
            },
            SUBSCRIPTION_PATH: {
                "object": "subscription", "id": SUBSCRIPTION, "livemode": False, "status": "canceled",
            },
        }
        self.env = ENV.copy()
        self.late_responses = {}
        self.connected = []
        self.requests = []
        fixture = self

        class Server(BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append((self.path, dict(self.headers)))
                reply = fixture.routes[self.path]
                if any(path == SUBSCRIPTION_PATH for path, _ in fixture.requests):
                    reply = fixture.late_responses.get(self.path, reply)
                self.send_response(reply if isinstance(reply, int) else 200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Location", "https://outside.invalid/privateRedirect")
                self.end_headers()
                self.wfile.write(json.dumps(reply).encode())

            def log_message(self, *_args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Server)
        worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01})
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(worker.join)
        self.addCleanup(server.shutdown)

        def disposable_connection(host, timeout):
            self.connected.append(host)
            return http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=timeout)

        self.enterContext(patch.object(runner.http.client, "HTTPSConnection", disposable_connection))

    def run_case(self, result, phase="finish"):
        status = {"admitted": 0, "accepted": 0}.get(result, 1)
        expected = {"result": result}
        if status == 0:
            expected.update(scope="operator-assisted-staging", build=SHA)
        out = io.StringIO()
        err = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            returned = runner.main([phase], self.env, io.BytesIO(json.dumps(self.evidence).encode()))
        self.assertEqual(returned, status)
        self.assertEqual(out.getvalue(), json.dumps(expected) + "\n")
        self.assertEqual(err.getvalue(), "")

    def test_admit_checks_build_and_mode_without_checkout(self):
        del self.evidence["checkout_session"]
        self.evidence["checks"] = dict.fromkeys(runner.PREREQUISITES, SHA)
        self.run_case("admitted", "admit")
        self.assertEqual([path for path, _ in self.requests], list(self.routes)[:4])

    def test_finish_verifies_payment_and_teardown_with_redacted_output(self):
        self.run_case("accepted")
        self.assertEqual([path for path, _ in self.requests],
                         list(self.routes) + ["/api/build", "/healthz"])
        self.assertEqual(self.connected,
                         ["staging.nntune.com"] * 2 + ["api.stripe.com"] * 4
                         + ["staging.nntune.com"] * 2)
        for path, headers in self.requests:
            if path.startswith("/v1/"):
                self.assertEqual(headers["Authorization"], "Bearer " + ENV["PAID_INVITE_STRIPE_READ_KEY"])
                self.assertNotIn("CF-Access-Client-Id", headers)
                self.assertNotIn("CF-Access-Client-Secret", headers)
            else:
                self.assertEqual(headers["CF-Access-Client-Id"], ENV["CF_ACCESS_CLIENT_ID"])
                self.assertEqual(headers["CF-Access-Client-Secret"], ENV["CF_ACCESS_CLIENT_SECRET"])
                self.assertNotIn("Authorization", headers)

    def test_finish_refuses_late_build_drift_or_read_failure(self):
        for path, response, code in (
            ("/api/build", {"sha": "7" * 40}, "wrong_build"),
            ("/healthz", {"ok": True, "build_sha": "7" * 40}, "wrong_build"),
            ("/api/build", 503, "upstream_refused"),
            ("/healthz", 503, "upstream_refused"),
        ):
            with self.subTest(path=path, code=code), patch.dict(self.late_responses, {path: response}):
                self.requests.clear()
                self.run_case(code)
                closing_paths = ["/api/build"] if path == "/api/build" else ["/api/build", "/healthz"]
                self.assertEqual([seen for seen, _ in self.requests], list(self.routes) + closing_paths)

    def test_default_off_and_invalid_admission_send_no_requests(self):
        for evidence in ({"app": "other-app"}, {"environment": "production"},
                         {"origin": "https://nntune.com"}):
            with self.subTest(evidence=evidence), patch.dict(self.evidence, evidence):
                self.run_case("wrong_target")
        for code, override in (
            ("disabled", {"NNTUNE_PAID_INVITE_ACCEPTANCE": ""}),
            ("stripe_mode", {"PAID_INVITE_STRIPE_READ_KEY": "sk_live_x"}),
            ("wrong_build", {"EXPECTED_BUILD_SHA": "7" * 40}),
            ("access_denied", {"CF_ACCESS_CLIENT_SECRET": ""}),
        ):
            with self.subTest(code=code), patch.dict(self.env, override):
                self.run_case(code)
        self.assertEqual(self.connected, [])

    def test_missing_or_drifted_journey_evidence_sends_no_requests(self):
        self.evidence["checks"]["teardown_verified"] = "7" * 40
        self.run_case("wrong_build")
        del self.evidence["checks"]["teardown_verified"]
        self.run_case("missing_evidence")
        self.assertEqual(self.connected, [])

    def test_finish_requires_invoice_history_even_with_paid_checkout(self):
        # An incomplete invoice comparison leaves this attestation absent.
        self.evidence["checks"].pop("invoice_history", None)
        self.run_case("missing_evidence")
        self.assertEqual(self.connected, [])

        # A complete comparison attests the same build for the same paid fixture.
        self.evidence["checks"]["invoice_history"] = SHA
        self.run_case("accepted")
        self.assertIn(f"/v1/checkout/sessions/{SESSION}", [path for path, _ in self.requests])

    def test_redirect_and_denied_access_stop_without_following(self):
        for status in (302, 401, 403):
            with self.subTest(status=status):
                self.routes["/api/build"] = status
                self.requests.clear()
                self.run_case("access_denied")
                self.assertEqual([path for path, _ in self.requests], ["/api/build"])

    def test_provider_and_build_refusals(self):
        for path, code, override in (
            ("/api/build", "wrong_build", {"sha": "7" * 40}),
            ("/healthz", "wrong_build", {"build_sha": "unknown"}),
            ("/v1/balance", "stripe_mode", {"livemode": True}),
            ("/v1/balance", "stripe_mode", {"livemode": None}),
            ("/v1/account", "stripe_account", {"id": "acct_other"}),
            (f"/v1/checkout/sessions/{SESSION}", "stripe_mode", {"livemode": True}),
            (f"/v1/checkout/sessions/{SESSION}", "payment_unverified", {"payment_status": "unpaid"}),
            (SUBSCRIPTION_PATH, "teardown_unverified", {"status": "active"}),
        ):
            with self.subTest(code=code), patch.dict(self.routes[path], override):
                self.run_case(code)


if __name__ == "__main__":
    unittest.main()
