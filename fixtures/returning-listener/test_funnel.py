"""Synthetic-only contract tests. No network or persistent database."""
import sqlite3
import unittest
from pathlib import Path

AS_OF = 10_000_000
DAY = 86400
SQL = Path(__file__).with_name("query.sql")


def database():
    db = sqlite3.connect(":memory:")
    db.executescript("""
        CREATE TABLE accounts (account_id TEXT, human_key TEXT, account_class TEXT, consent INTEGER);
        CREATE TABLE first_journeys (account_id TEXT, journey_id TEXT, completed_at INTEGER, session_id TEXT,
            authenticated INTEGER, provider_setup INTEGER, owned_content INTEGER, advancing_audio INTEGER, range_seek INTEGER);
        CREATE TABLE returns (account_id TEXT, return_id TEXT, completed_at INTEGER, session_id TEXT,
            authenticated INTEGER, listener_initiated INTEGER, advancing_audio INTEGER);
    """)
    return db


_OMITTED = object()


def account(db, name, human=_OMITTED, kind="eligible", consent=1):
    db.execute("INSERT INTO accounts VALUES (?, ?, ?, ?)",
               (name, name if human is _OMITTED else human, kind, consent))


def first(db, name, when, session="first", *, journey="journey", authenticated=1,
          provider=1, content=1, audio=1, seek=1):
    db.execute("INSERT INTO first_journeys VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
               (name, journey, when, session, authenticated, provider, content, audio, seek))


def ret(db, name, when, session="return", *, authenticated=1, initiated=1, audio=1):
    db.execute("INSERT INTO returns VALUES (?, ?, ?, ?, ?, ?, ?)",
               (name, "receipt", when, session, authenticated, initiated, audio))


def classifications(db, as_of=AS_OF):
    rows = db.execute(SQL.read_text(), {"as_of": as_of}).fetchall()
    result = dict(rows)
    if len(rows) != len(result):
        raise AssertionError(f"duplicate subjects in query output: {rows!r}")
    return result


class FunnelTests(unittest.TestCase):
    def test_window_boundaries_and_seven_day_limit(self):
        db = database()
        for name, returned, delta in [
            ("upper", AS_OF, 7 * DAY),
            ("inside", AS_OF - 30 * DAY + 1, 1),
            ("lower", AS_OF - 30 * DAY, 1),
            ("too_late", AS_OF - 1, 7 * DAY + 1),
            ("same_time", AS_OF - 1, 0),
            ("future", AS_OF + 1, 1),
        ]:
            account(db, name)
            first(db, name, returned - delta)
            ret(db, name, returned)
        self.assertEqual(classifications(db), {
            "human:upper": "counted", "human:inside": "counted",
            "human:lower": "returned_outside_window", "human:too_late": "non_return",
            "human:same_time": "pending", "human:future": "pending",
        })

    def test_session_eligibility_and_incomplete_first(self):
        db = database()
        for name in ("same_session", "not_initiated", "no_audio", "pending_edge", "expired", "incomplete"):
            account(db, name)
            first(db, name, AS_OF - (7 * DAY if name == "pending_edge" else 8 * DAY if name == "expired" else DAY), seek=0 if name == "incomplete" else 1)
        ret(db, "same_session", AS_OF, "first")
        ret(db, "not_initiated", AS_OF, initiated=0)
        ret(db, "no_audio", AS_OF, audio=0)
        ret(db, "incomplete", AS_OF)
        self.assertEqual(classifications(db), {
            "human:same_session": "pending", "human:not_initiated": "pending",
            "human:no_audio": "pending", "human:pending_edge": "pending",
            "human:expired": "non_return", "human:incomplete": "no_qualifying_first",
        })

    def test_blank_identities_and_sessions(self):
        db = database()
        expected = {}
        for label, value in (("empty", ""), ("space", "   "), ("tab", "\t"),
                             ("newline", "\n"), ("mixed", " \t\n\r\v\f")):
            identity = "identity_" + label
            db.execute("INSERT INTO accounts VALUES (?, ?, 'eligible', 1)", (identity, value))
            first(db, identity, AS_OF - DAY)
            ret(db, identity, AS_OF)
            expected["account:" + identity] = "uncertain_identity"
            first_name = "first_" + label
            account(db, first_name)
            first(db, first_name, AS_OF - DAY, value)
            ret(db, first_name, AS_OF)
            expected["human:" + first_name] = "no_qualifying_first"
            return_name = "return_" + label
            account(db, return_name)
            first(db, return_name, AS_OF - DAY)
            ret(db, return_name, AS_OF, value)
            expected["human:" + return_name] = "pending"
        self.assertEqual(classifications(db), expected)

    def test_each_receipt_requirement_independently(self):
        db = database()
        expected = {}
        for flag in ("authenticated", "provider", "content", "audio", "seek"):
            name = "first_" + flag
            account(db, name)
            first(db, name, AS_OF - DAY, **{flag: 0})
            ret(db, name, AS_OF)
            expected["human:" + name] = "no_qualifying_first"
        for flag in ("authenticated", "initiated", "audio"):
            name = "return_" + flag
            account(db, name)
            first(db, name, AS_OF - DAY)
            ret(db, name, AS_OF, **{flag: 0})
            expected["human:" + name] = "pending"
        self.assertEqual(classifications(db), expected)

    def test_earliest_qualifying_first_is_selected(self):
        db = database()
        account(db, "ordered")
        first(db, "ordered", AS_OF - 8 * DAY, "early", journey="early")
        first(db, "ordered", AS_OF - DAY, "late", journey="late")
        ret(db, "ordered", AS_OF, "return")
        # This return is too late for the earliest first, but valid for the later one.
        self.assertEqual(classifications(db), {"human:ordered": "non_return"})

    def test_identity_dedup_exclusions_and_unknown(self):
        db = database()
        account(db, "a1", "shared")
        account(db, "a2", "shared")
        first(db, "a1", AS_OF - DAY)
        first(db, "a1", AS_OF - DAY)
        ret(db, "a2", AS_OF)
        ret(db, "a2", AS_OF)
        for kind in ("agent", "synthetic", "demo", "unclassified"):
            account(db, kind, kind=kind)
            first(db, kind, AS_OF - DAY)
            ret(db, kind, AS_OF)
        account(db, "unconsented", consent=0)
        db.execute("INSERT INTO accounts VALUES ('unknown', NULL, 'eligible', 1)")
        self.assertEqual(classifications(db), {
            "human:shared": "counted", "account:agent": "excluded",
            "account:synthetic": "excluded", "account:demo": "excluded",
            "account:unclassified": "excluded", "account:unconsented": "excluded",
            "account:unknown": "uncertain_identity",
        })


if __name__ == "__main__":
    unittest.main()
