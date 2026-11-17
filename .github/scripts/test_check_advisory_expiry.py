#!/usr/bin/env python3
"""Boundary coverage for the temporary Rust advisory exception."""

import datetime as dt
import unittest

from check_advisory_expiry import ADVISORY, EXPIRY, check


class AdvisoryExpiryTests(unittest.TestCase):
    def test_before_boundary(self):
        check({"advisories": {"ignore": [{"id": ADVISORY}]}}, EXPIRY - dt.timedelta(microseconds=1))

    def test_at_and_after_boundary(self):
        for now in (EXPIRY, EXPIRY + dt.timedelta(microseconds=1)):
            with self.subTest(now=now), self.assertRaises(ValueError):
                check({"advisories": {"ignore": [ADVISORY]}}, now)

    def test_removed_exception_does_not_block(self):
        check({"advisories": {"ignore": []}}, EXPIRY)

    def test_invalid_policy_fails_closed(self):
        for policy in ({}, {"advisories": {"ignore": "invalid"}}, {"advisories": {"ignore": [{}]}}):
            with self.subTest(policy=policy), self.assertRaises((KeyError, ValueError)):
                check(policy, EXPIRY)


if __name__ == "__main__":
    unittest.main()
