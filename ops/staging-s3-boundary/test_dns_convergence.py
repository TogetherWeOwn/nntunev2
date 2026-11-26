"""Exercise the executor policy with a synthetic clock and DNS responses."""

from dataclasses import replace
from datetime import datetime, timedelta, timezone
import unittest
from unittest.mock import patch

from dns_convergence import Change, Observation, Readback, Verdict, check_convergence


NAME = "fixture.example.invalid."
RECORD_ID = "fixture-id"
SERVERS = ("ns1.example.invalid.", "ns2.example.invalid.")
UTC_START = datetime(2000, 1, 1, tzinfo=timezone.utc)
ABSENT = ("NXDOMAIN", True, 0, 0)
PRESENT = ("NOERROR", True, 2, 1)


class SyntheticDNS:
    def __init__(self, plan):
        self.plan = plan
        self.now = 10
        self.readback = Readback(Change.CREATE, RECORD_ID, NAME, True, True, self.now)
        self.calls = []
        self.receipt = []
        self.duration = 0
        self.edit = lambda replies: replies

    def read_pair(self, servers, name, timeout):
        self.calls.append((servers, name, self.now - self.readback.at, timeout))
        self.now += self.duration
        utc = UTC_START + timedelta(seconds=self.now - self.readback.at)
        observations = [
            Observation(server, name, *shape, utc, self.now)
            for server, shape in zip(servers, self.plan(len(self.calls) - 1), strict=True)
        ]
        return self.edit(observations)

    def sleep(self, delay):
        self.now += delay

    def record(self, reply, elapsed):
        self.receipt.append((reply, elapsed))

    def run(self, direction, servers=SERVERS, **proof):
        readback = replace(self.readback, **{"change": direction, **proof})
        return check_convergence(
            direction, readback=readback, record_id=RECORD_ID, name=NAME, servers=servers,
            read_pair=self.read_pair, monotonic=lambda: self.now, sleep=self.sleep, record=self.record,
        )


class DNSConvergenceTest(unittest.TestCase):
    def assert_rounds(self, dns, times):
        self.assertEqual(dns.calls, [(SERVERS, NAME, time, 2) for time in times])

    def test_delayed_create_and_delete(self):
        for change, old, new in ((Change.CREATE, ABSENT, PRESENT), (Change.DELETE, PRESENT, ABSENT)):
            with self.subTest(change=change):
                dns = SyntheticDNS(lambda i: (old if i < 2 else new, old if i < 3 else new))
                dns.duration = 0.5
                result = dns.run(change)
                self.assertIs(result.verdict, Verdict.PASSED)
                self.assertFalse(result.retain_writer)
                self.assert_rounds(dns, range(0, 10, 2))
                self.assertEqual(len(dns.receipt), 10)
                for index, (reply, elapsed) in enumerate(dns.receipt):
                    round_index, server_index = divmod(index, 2)
                    expected = old if round_index < 2 + server_index else new
                    self.assertEqual(reply.server, SERVERS[server_index])
                    self.assertEqual((reply.rcode, reply.authoritative, reply.answer_count), expected[:3])
                    self.assertEqual(elapsed, round_index * 2 + 0.5)
                    self.assertEqual(reply.utc, UTC_START + timedelta(seconds=elapsed))

    def test_one_authority_cannot_pass_and_deadlines_are_bounded(self):
        for change, old, new, deadline in (
            (Change.CREATE, ABSENT, PRESENT, 90), (Change.DELETE, PRESENT, ABSENT, 300),
        ):
            for first in (old, new):
                with self.subTest(change=change, first=first):
                    dns = SyntheticDNS(lambda _: (first, old))
                    result = dns.run(change)
                    self.assertIs(result.verdict, Verdict.EXPIRED)
                    self.assertIs(result.retain_writer, change is Change.DELETE)
                    self.assertEqual(dns.now - dns.readback.at, deadline)
                    self.assert_rounds(dns, range(0, deadline, 2))
                    self.assertEqual(len(dns.receipt), deadline)

    def test_late_delete_containment_keeps_writer(self):
        for first_absent in (44, 46):
            with self.subTest(first_absent=first_absent):
                dns = SyntheticDNS(lambda i: (ABSENT, PRESENT if i < first_absent else ABSENT))
                result = dns.run(Change.DELETE)
                self.assertIs(result.verdict, Verdict.CONTAINED)
                self.assertTrue(result.retain_writer)
                self.assert_rounds(dns, range(0, (first_absent + 2) * 2, 2))

    def test_authority_regression_stops_both_directions(self):
        for change, old, new in ((Change.CREATE, ABSENT, PRESENT), (Change.DELETE, PRESENT, ABSENT)):
            with self.subTest(change=change):
                dns = SyntheticDNS(lambda i: (new if i < 2 else old, old if i == 0 else new))
                result = dns.run(change)
                self.assertIs(result.verdict, Verdict.STOPPED)
                self.assertIn("regressed", result.reason)
                self.assertIs(result.retain_writer, change is Change.DELETE)
                self.assert_rounds(dns, range(0, 6, 2))

    def test_matching_responses_must_be_consecutive(self):
        for change, old, new in ((Change.CREATE, ABSENT, PRESENT), (Change.DELETE, PRESENT, ABSENT)):
            with self.subTest(change=change):
                dns = SyntheticDNS(lambda i: (old if i == 1 else new, new))
                self.assertIs(dns.run(change).verdict, Verdict.PASSED)
                self.assert_rounds(dns, range(0, 8, 2))

    def test_authority_failures_stop_and_record_both_observations(self):
        for change in Change:
            for failure in (("NXDOMAIN", False, 0, 0), ("SERVFAIL", True, 0, 0),
                            ("REFUSED", True, 0, 0), ("TIMEOUT", False, 0, 0)):
                with self.subTest(change=change, failure=failure):
                    dns = SyntheticDNS(lambda _: (failure, ABSENT))
                    result = dns.run(change)
                    self.assertIs(result.verdict, Verdict.STOPPED)
                    self.assertIs(result.retain_writer, change is Change.DELETE)
                    self.assert_rounds(dns, [0])
                    self.assertEqual([reply.rcode for reply, _ in dns.receipt], [failure[0], "NXDOMAIN"])

    def test_wrong_name_wrong_authority_and_wrong_owner_stop(self):
        for changes in (dict(name="other.invalid."), dict(server=SERVERS[0]),
                        dict(exact_a_count=0), dict(at=9), dict(at=11)):
            with self.subTest(changes=changes):
                dns = SyntheticDNS(lambda _: (PRESENT, PRESENT))
                dns.edit = lambda replies: [replies[0], replace(replies[1], **changes)]
                self.assertIs(dns.run(Change.CREATE).verdict, Verdict.STOPPED)
                self.assert_rounds(dns, [0])

    def test_exact_rest_readback_and_distinct_authorities_required(self):
        for proof in (dict(exact_id=False), dict(exact_name=False), dict(record_id="other-id"),
                      dict(name="other.invalid."), dict(change=Change.CREATE), dict(at=11)):
            with self.subTest(proof=proof):
                dns = SyntheticDNS(lambda _: (ABSENT, ABSENT))
                result = dns.run(Change.DELETE, **proof)
                self.assertIs(result.verdict, Verdict.STOPPED)
                self.assertTrue(result.retain_writer)
                self.assertEqual(dns.calls, [])
        for change, servers in (("delete", SERVERS), (Change.DELETE, (SERVERS[0], SERVERS[0].upper()))):
            with self.subTest(change=change, servers=servers):
                dns = SyntheticDNS(lambda _: (PRESENT, PRESENT))
                result = dns.run(change, servers)
                self.assertIs(result.verdict, Verdict.STOPPED)
                self.assertTrue(result.retain_writer)
                self.assertEqual(dns.calls, [])

    def test_callback_failure_stops_and_keeps_delete_writer(self):
        for hook in ("read_pair", "record", "sleep"):
            with self.subTest(hook=hook):
                dns = SyntheticDNS(lambda _: (PRESENT, PRESENT))
                with patch.object(dns, hook, side_effect=OSError("synthetic callback failure")):
                    result = dns.run(Change.DELETE)
                self.assertIs(result.verdict, Verdict.STOPPED)
                self.assertTrue(result.retain_writer)
                self.assertLessEqual(len(dns.calls), 1)

    def test_reply_at_deadline_cannot_pass(self):
        for change, shape, start in ((Change.CREATE, PRESENT, 87), (Change.DELETE, ABSENT, 297)):
            with self.subTest(change=change):
                dns = SyntheticDNS(lambda _: (shape, shape))
                dns.now += start
                dns.duration = 1
                result = dns.run(change)
                self.assertIs(result.verdict, Verdict.EXPIRED)
                self.assertEqual([call[2:] for call in dns.calls], [(start, 2), (start + 2, 1)])
                self.assertEqual([elapsed for _, elapsed in dns.receipt], [start + 1] * 2 + [start + 3] * 2)

    def test_late_round_stops_and_records_replies(self):
        dns = SyntheticDNS(lambda _: (ABSENT, ABSENT))
        dns.duration = 2.1
        result = dns.run(Change.DELETE)
        self.assertIs(result.verdict, Verdict.STOPPED)
        self.assertTrue(result.retain_writer)
        self.assert_rounds(dns, [0])
        self.assertEqual(len(dns.receipt), 2)

    def test_timely_delete_replies_can_be_batched_after_qualification_deadline(self):
        dns = SyntheticDNS(lambda _: (ABSENT, ABSENT))
        dns.now += 87
        dns.duration = 1.25
        dns.edit = lambda replies: [replace(reply, at=reply.at - 0.5, utc=reply.utc - timedelta(seconds=0.5)) for reply in replies]
        self.assertIs(dns.run(Change.DELETE).verdict, Verdict.PASSED)
        self.assertEqual(dns.now - dns.readback.at, 90.25)
        self.assertEqual([elapsed for _, elapsed in dns.receipt], [87.75, 87.75, 89.75, 89.75])


if __name__ == "__main__":
    unittest.main()
