"""Rehearse the opening and closing reads around a disposable clock."""

import unittest
from unittest.mock import patch

import test_paid_invite_acceptance as acceptance


class ClockReadbackTest(unittest.TestCase):
    def test_clock_readback_precedes_retirement(self):
        fixture = acceptance.AcceptanceTest()
        self.addCleanup(fixture.doCleanups)
        fixture.setUp()

        finish = fixture.evidence
        fixture.evidence = {
            **finish,
            "checks": dict.fromkeys(acceptance.runner.PREREQUISITES, acceptance.SHA),
        }
        del fixture.evidence["checkout_session"]
        with patch.dict(fixture.routes["/v1/balance"], livemode=True):
            fixture.run_case("stripe_mode", "admit")
        with patch.dict(fixture.routes["/v1/account"], id="acct_unrelated"):
            fixture.run_case("stripe_account", "admit")
        fixture.run_case("admitted", "admit")
        self.assertTrue(all("/checkout/" not in path for path, _ in fixture.requests))

        fixture.evidence = finish
        session_path = f"/v1/checkout/sessions/{finish['checkout_session']}"
        subscription_path = f"/v1/subscriptions/{acceptance.SUBSCRIPTION}"
        subscription = fixture.routes[subscription_path]
        # The selected subscription has to be canceled even with the finish
        # attestation present. The paid session is retained for all reads.
        with patch.dict(subscription, status="active"):
            fixture.run_case("teardown_unverified")
        fixture.requests.clear()
        fixture.run_case("accepted")
        selected_reads = [
            path for path, _ in fixture.requests
            if path.startswith(("/v1/checkout/sessions/", "/v1/subscriptions/"))
        ]
        self.assertEqual(selected_reads, [session_path, subscription_path])
        # Model loss of subscription readback if the clock is retired first.
        with patch.dict(fixture.routes, {subscription_path: 404}):
            fixture.run_case("upstream_refused")


if __name__ == "__main__":
    unittest.main()
