#!/usr/bin/env python3
"""Boundary and fail-closed coverage for the temporary Rust advisory exception."""

import contextlib
import datetime as dt
import io
import tempfile
import tomllib
import unittest
from pathlib import Path

from check_advisory_expiry import ADVISORY, EXPIRY, check, main, parse_review_deadline

HARD_EXPIRY = dt.datetime(2027, 2, 17, 0, 0, 0, tzinfo=dt.timezone.utc)
REPO_DENY_TOML = Path(__file__).resolve().parents[2] / "deny.toml"


def _policy_with_reason(reason: str) -> dict:
    return {"advisories": {"ignore": [{"id": ADVISORY, "reason": reason}]}}


class AdvisoryExpiryTests(unittest.TestCase):
    def test_hard_deadline_is_pinned(self):
        self.assertEqual(ADVISORY, "RUSTSEC-2026-0543")
        self.assertEqual(EXPIRY, HARD_EXPIRY)

    def test_before_boundary(self):
        policy = _policy_with_reason("owner exception 2026-11-17; review by 2027-02-17")
        check(policy, HARD_EXPIRY - dt.timedelta(microseconds=1))
        # Equivalent non-UTC timezone instant just before the UTC boundary.
        plus_two = dt.timezone(dt.timedelta(hours=2))
        check(policy, (HARD_EXPIRY - dt.timedelta(seconds=1)).astimezone(plus_two))

    def test_at_and_after_boundary(self):
        policy = _policy_with_reason("owner exception 2026-11-17; review by 2027-02-17")
        minus_five = dt.timezone(dt.timedelta(hours=-5))
        for now in (
            HARD_EXPIRY,
            HARD_EXPIRY + dt.timedelta(microseconds=1),
            HARD_EXPIRY + dt.timedelta(days=1),
            HARD_EXPIRY.astimezone(minus_five),
        ):
            with self.subTest(now=now), self.assertRaisesRegex(ValueError, "expired"):
                check(policy, now)

    def test_earlier_recorded_review_deadline_is_enforced(self):
        earlier = dt.datetime(2027, 1, 15, 0, 0, 0, tzinfo=dt.timezone.utc)
        policy = _policy_with_reason("dependency review bypass approved; review by 2027-01-15")
        check(policy, earlier - dt.timedelta(microseconds=1))
        with self.assertRaisesRegex(ValueError, "expired"):
            check(policy, earlier)

    def test_silently_extended_review_deadline_fails_closed(self):
        before_expiry = HARD_EXPIRY - dt.timedelta(days=30)
        for reason in (
            "owner exception; review by 2027-02-18",
            "owner exception; review by 2027-02-17T00:00:01Z",
            "owner exception; review by 2028-02-17",
        ):
            with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, "exceeds hard expiry"):
                check(_policy_with_reason(reason), before_expiry)

    def test_malformed_or_missing_review_deadline_fails_closed(self):
        before_expiry = HARD_EXPIRY - dt.timedelta(days=30)
        bad_reasons = (
            "",
            "chrono unmaintained without deadline",
            "review by",
            "review by soon",
            "review by 2027-02-30",
            "review by 2027-13-01",
            "review by 2027-2-17",
            "review by 2027/02/17",
            "review by 2027-02-17T00:00:00",
            "review by 2027-02-17T00:00:00+00:00",
            "review by 2027-02-17T25:00:00Z",
            "review by 2027-02-17 00:00:01Z",
            "review by 2027-02-17\t00:00:00",
            "review by 2027-02-17\n00:00:00Z",
            "review by 2027-02-17 00 :00:00Z",
            "review by 2027-02-17..",
            "review by 2027-02-17; review by 2027-02-17",
            "review by 2027-02-17; review by 2028-01-01",
            "review by2027-01-15; review by 2027-02-17",
            "review by 2027-02-17; review by2028-01-01",
            "review by 2027-02-17; review by",
            "review by\n2027-02-17",
            "review\nby 2027-02-17",
            "revıew by 2027-02-17",
            "review by 2027-02-17; review\nby 2028-01-01",
            "review by 2027-02-17; review\r\nby",
        )
        for reason in bad_reasons:
            with self.subTest(reason=reason), self.assertRaises(ValueError):
                check(_policy_with_reason(reason), before_expiry)
        for bad_policy in (
            {"advisories": {"ignore": [ADVISORY]}},
            {"advisories": {"ignore": [{"id": ADVISORY}]}},
            {"advisories": {"ignore": [{"id": ADVISORY, "reason": 20270217}]}},
        ):
            with self.subTest(policy=bad_policy), self.assertRaises(ValueError):
                check(bad_policy, before_expiry)

    def test_parse_review_deadline_accepts_canonical_date_and_utc_timestamp(self):
        self.assertEqual(
            parse_review_deadline("owner exception; review by 2027-02-17"),
            HARD_EXPIRY,
        )
        self.assertEqual(
            parse_review_deadline("owner exception (review by 2027-02-17)."),
            HARD_EXPIRY,
        )
        self.assertEqual(
            parse_review_deadline("review by 2027-02-17; owner exception"),
            HARD_EXPIRY,
        )
        self.assertEqual(
            parse_review_deadline("owner exception; review by 2027-02-17T00:00:00Z; notes"),
            HARD_EXPIRY,
        )

    def test_non_canonical_advisory_id_fails_closed(self):
        before_expiry = HARD_EXPIRY - dt.timedelta(days=30)
        for variant in ("rustsec-2026-0543", " RUSTSEC-2026-0543 ", "RustSec-2026-0543"):
            policy = {
                "advisories": {
                    "ignore": [{"id": variant, "reason": "owner exception; review by 2027-02-17"}]
                }
            }
            with self.subTest(variant=variant), self.assertRaisesRegex(ValueError, "canonical"):
                check(policy, before_expiry)

    def test_malformed_now_timestamp_fails_closed(self):
        policies = (
            {"advisories": {"ignore": []}},
            _policy_with_reason("owner exception; review by 2027-02-17"),
        )
        for policy in policies:
            for bad_now in (
                dt.datetime(2026, 11, 27, 0, 0, 0),  # naive datetime
                dt.date(2026, 11, 27),  # date without time/tz
                "2026-11-27T00:00:00Z",
                None,
            ):
                with self.subTest(policy=policy, now=bad_now), self.assertRaises(ValueError):
                    check(policy, bad_now)

    def test_removed_exception_does_not_block(self):
        check({"advisories": {}}, HARD_EXPIRY)
        check({"advisories": {"ignore": []}}, HARD_EXPIRY)
        check({"advisories": {"ignore": []}}, HARD_EXPIRY + dt.timedelta(days=365))

    def test_duplicate_entries_validate_every_match(self):
        before_expiry = HARD_EXPIRY - dt.timedelta(days=30)
        valid_entry = {"id": ADVISORY, "reason": "owner exception; review by 2027-02-17"}
        for second in (
            ADVISORY,
            {"id": ADVISORY, "reason": "owner exception; review by 2027-02-18"},
            {"id": ADVISORY, "reason": "owner exception; review by 2027-02-30"},
        ):
            policy = {"advisories": {"ignore": [valid_entry, second]}}
            with self.subTest(second=second), self.assertRaises(ValueError):
                check(policy, before_expiry)

    def test_invalid_policy_fails_closed(self):
        for policy in (
            None,
            [],
            {},
            {"advisories": None},
            {"advisories": []},
            {"advisories": {"ignore": None}},
            {"advisories": {"ignore": "invalid"}},
            {"advisories": {"ignore": [None]}},
            {"advisories": {"ignore": [{}]}},
            {"advisories": {"ignore": [""]}},
            {"advisories": {"ignore": ["   "]}},
            {"advisories": {"ignore": [{"id": "   "}]}},
            {"advisories": {"ignore": [{"id": "RUSTSEC-2099-0001", "reason": 123}]}},
        ):
            with self.subTest(policy=policy), self.assertRaises((KeyError, ValueError)):
                check(policy, HARD_EXPIRY)

    def test_checked_in_deny_toml_enforces_recorded_expiry_when_present(self):
        with REPO_DENY_TOML.open("rb") as policy_file:
            policy = tomllib.load(policy_file)
        matching_entries = [
            entry
            for entry in policy.get("advisories", {}).get("ignore", [])
            if (isinstance(entry, str) and entry.strip().upper() == ADVISORY)
            or (
                isinstance(entry, dict)
                and isinstance(entry.get("id"), str)
                and entry["id"].strip().upper() == ADVISORY
            )
        ]
        if not matching_entries:
            check(policy, HARD_EXPIRY)
            return
        deadlines = [
            min(HARD_EXPIRY, parse_review_deadline(entry["reason"]))
            for entry in matching_entries
            if isinstance(entry, dict) and isinstance(entry.get("reason"), str)
        ]
        self.assertEqual(len(deadlines), len(matching_entries))
        expected_expiry = min(deadlines)
        check(policy, expected_expiry - dt.timedelta(microseconds=1))
        with self.assertRaisesRegex(ValueError, "expired"):
            check(policy, expected_expiry)
        with self.assertRaisesRegex(ValueError, "expired"):
            check(policy, expected_expiry + dt.timedelta(microseconds=1))

    def test_main_fails_closed_on_missing_or_malformed_file_and_expiry(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            tmp = Path(tmpdir)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(tmp / "missing.toml", HARD_EXPIRY - dt.timedelta(days=1)), 1)
            self.assertIn("advisory expiry check failed:", stderr.getvalue())

            bad_toml = tmp / "bad.toml"
            bad_toml.write_text("[advisories\nignore = [", encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(bad_toml, HARD_EXPIRY - dt.timedelta(days=1)), 1)
            self.assertIn("advisory expiry check failed:", stderr.getvalue())

            active_toml = tmp / "active.toml"
            active_toml.write_text(
                '[advisories]\nignore = [\n  { id = "RUSTSEC-2026-0543", '
                'reason = "owner exception; review by 2027-02-17" },\n]\n',
                encoding="utf-8",
            )
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(active_toml, HARD_EXPIRY - dt.timedelta(seconds=1)), 0)
            self.assertEqual(stderr.getvalue(), "")

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(active_toml, HARD_EXPIRY), 1)
            self.assertIn("expired", stderr.getvalue())

            extended_toml = tmp / "extended.toml"
            extended_toml.write_text(
                '[advisories]\nignore = [\n  { id = "RUSTSEC-2026-0543", '
                'reason = "owner exception; review by 2027-02-18" },\n]\n',
                encoding="utf-8",
            )
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(extended_toml, HARD_EXPIRY - dt.timedelta(days=30)), 1)
            self.assertIn("exceeds hard expiry", stderr.getvalue())

            removed_toml = tmp / "removed.toml"
            removed_toml.write_text("[advisories]\nignore = []\n", encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(main(removed_toml, HARD_EXPIRY), 0)
            self.assertEqual(stderr.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
