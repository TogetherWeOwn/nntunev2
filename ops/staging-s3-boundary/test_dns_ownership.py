"""Exercise DNS cleanup selection with synthetic provider readbacks."""

from dataclasses import replace
import unittest

from dns_ownership import DnsRecord, select_owned_record


CREATED = DnsRecord(
    zone_id="fixture-zone",
    record_id="created-record",
    name="fixture.example.invalid",
    record_type="A",
    content="192.0.2.10",
    proxied=True,
    ttl=1,
    tags=(),
    comment=None,
)


class DnsOwnershipTest(unittest.TestCase):
    def test_selects_only_the_recorded_zone_and_id(self):
        self.assertEqual(
            select_owned_record(CREATED, replace(CREATED), (replace(CREATED),)),
            ("fixture-zone", "created-record"),
        )

    def test_same_name_replacement_never_selects_a_delete(self):
        replacement = replace(CREATED, record_id="replacement-record")
        for by_id, by_name in (
            (None, (replacement,)),
            (replacement, (replacement,)),
            (replacement, (CREATED,)),
            (CREATED, (replacement,)),
            (CREATED, (CREATED, replacement)),
        ):
            with self.subTest(by_id=by_id, by_name=by_name):
                self.assertIsNone(select_owned_record(CREATED, by_id, by_name))

    def test_each_read_must_match_the_receipt(self):
        for changes in (
            {"zone_id": "other-zone"},
            {"name": "sibling.example.invalid"},
            {"record_type": "AAAA"},
            {"content": "192.0.2.11"},
            {"proxied": False},
            {"ttl": 60},
            {"tags": ("scope:other",)},
            {"comment": "another rollout"},
        ):
            with self.subTest(changes=changes):
                changed = replace(CREATED, **changes)
                self.assertIsNone(select_owned_record(CREATED, changed, (CREATED,)))
                self.assertIsNone(select_owned_record(CREATED, CREATED, (changed,)))

    def test_missing_or_ambiguous_evidence_stays_pending(self):
        for by_id, by_name in (
            (None, (CREATED,)),
            (CREATED, None),
            (CREATED, ()),
            (None, ()),
            (CREATED, (CREATED, CREATED)),
        ):
            with self.subTest(by_id=by_id, by_name=by_name):
                self.assertIsNone(select_owned_record(CREATED, by_id, by_name))

        for field in ("zone_id", "record_id", "name"):
            with self.subTest(field=field):
                incomplete = replace(CREATED, **{field: ""})
                self.assertIsNone(select_owned_record(incomplete, incomplete, (incomplete,)))


if __name__ == "__main__":
    unittest.main()
