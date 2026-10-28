"""Builder-owned regressions for the synthetic window-arithmetic projection."""
import unittest

from test_funnel import AS_OF, DAY, account, classifications, database, first, ret


class ReviewRegressions(unittest.TestCase):
    def setUp(self):
        self.db = database()
        self.addCleanup(self.db.close)

    def test_explicit_missing_identity_is_not_defaulted_to_account(self):
        for name, human in (("empty", ""), ("null", None)):
            account(self.db, name, human=human)
            first(self.db, name, AS_OF - DAY)
            ret(self.db, name, AS_OF)
        account(self.db, "omitted")
        first(self.db, "omitted", AS_OF - DAY)
        ret(self.db, "omitted", AS_OF)
        self.assertEqual(classifications(self.db), {
            "account:empty": "uncertain_identity",
            "account:null": "uncertain_identity",
            "human:omitted": "counted",
        })

    def test_excluded_accounts_cannot_supply_first_or_return_for_shared_human(self):
        expected = {}
        for label, kind, consent in (("agent", "agent", 1), ("no_consent", "eligible", 0),
                                      ("null_consent", "eligible", None), ("partner", "partner", 1)):
            for side in ("first", "return"):
                human = label + "_" + side
                good, bad = human + "_good", human + "_bad"
                account(self.db, good, human=human)
                account(self.db, bad, human=human, kind=kind, consent=consent)
                first(self.db, bad if side == "first" else good, AS_OF - DAY)
                ret(self.db, good if side == "first" else bad, AS_OF)
                expected["human:" + human] = "no_qualifying_first" if side == "first" else "pending"
                expected["account:" + bad] = "excluded"
        self.assertEqual(classifications(self.db), expected)

    def test_unknown_classes_and_null_consent_are_excluded_before_identity(self):
        expected = {}
        for label, kind, consent in (("null_class", None, 1), ("partner", "partner", 1),
                                     ("null_consent", "eligible", None),
                                     ("no_consent", "eligible", 0)):
            for suffix, human in (("resolved", label), ("blank", ""), ("null", None)):
                name = label + "_" + suffix
                account(self.db, name, human=human, kind=kind, consent=consent)
                first(self.db, name, AS_OF - DAY)
                ret(self.db, name, AS_OF)
                expected["account:" + name] = "excluded"
        self.assertEqual(classifications(self.db), expected)

    def test_missing_receipt_evidence_never_qualifies(self):
        expected = {}
        for flag in ("authenticated", "provider", "content", "audio", "seek"):
            name = "first_" + flag
            account(self.db, name)
            first(self.db, name, AS_OF - DAY, **{flag: None})
            ret(self.db, name, AS_OF)
            expected["human:" + name] = "no_qualifying_first"
        for flag in ("authenticated", "initiated", "audio"):
            name = "return_" + flag
            account(self.db, name)
            first(self.db, name, AS_OF - DAY)
            ret(self.db, name, AS_OF, **{flag: None})
            expected["human:" + name] = "pending"
        for side in ("first", "return"):
            name = side + "_null_session"
            account(self.db, name)
            first(self.db, name, AS_OF - DAY, session=None if side == "first" else "first")
            ret(self.db, name, AS_OF, session=None if side == "return" else "return")
            expected["human:" + name] = "no_qualifying_first" if side == "first" else "pending"
        self.assertEqual(classifications(self.db), expected)

    def test_invalid_earlier_first_does_not_hide_valid_first(self):
        account(self.db, "filtered")
        first(self.db, "filtered", AS_OF - 2 * DAY, journey="invalid", seek=0)
        first(self.db, "filtered", AS_OF - DAY, journey="valid")
        ret(self.db, "filtered", AS_OF)
        self.assertEqual(classifications(self.db), {"human:filtered": "counted"})

    def test_earliest_first_is_per_human_not_account(self):
        account(self.db, "old_account", human="shared")
        account(self.db, "new_account", human="shared")
        first(self.db, "old_account", AS_OF - 8 * DAY, session="old")
        first(self.db, "new_account", AS_OF - DAY, session="new")
        ret(self.db, "new_account", AS_OF)
        self.assertEqual(classifications(self.db), {"human:shared": "non_return"})

    def test_first_ties_use_journey_then_session_not_insertion_order(self):
        expected = {"human:journey": "pending", "human:session": "pending"}
        # Insert the losing journey/session first. The chosen first's session
        # matches the return, so a missing tie-breaker would falsely count it.
        for name, earlier_journey, later_journey, chosen_session, other_session in (
                ("journey", "a", "z", "z", "a"),
                ("session", "same", "same", "a", "z")):
            account(self.db, name)
            first(self.db, name, AS_OF - DAY, session=other_session, journey=later_journey)
            first(self.db, name, AS_OF - DAY, session=chosen_session, journey=earlier_journey)
            ret(self.db, name, AS_OF, session=chosen_session)
        self.assertEqual(classifications(self.db), expected)

    def test_as_of_moves_the_window_and_is_not_a_fixed_fixture_constant(self):
        account(self.db, "rolling")
        first(self.db, "rolling", AS_OF - DAY)
        ret(self.db, "rolling", AS_OF)
        self.assertEqual(classifications(self.db, as_of=AS_OF), {"human:rolling": "counted"})
        self.assertEqual(classifications(self.db, as_of=AS_OF + 30 * DAY), {
            "human:rolling": "returned_outside_window",
        })
        self.assertEqual(classifications(self.db, as_of=AS_OF + 31 * DAY), {
            "human:rolling": "returned_outside_window",
        })

    def test_timing_edges_reject_backwards_return_and_future_first(self):
        for name in ("backwards", "future_first", "pending_end", "pending_expired", "old_first"):
            account(self.db, name)
        first(self.db, "backwards", AS_OF - DAY)
        ret(self.db, "backwards", AS_OF - DAY - 1)
        first(self.db, "future_first", AS_OF + 1)
        ret(self.db, "future_first", AS_OF + 2)
        first(self.db, "pending_end", AS_OF - 7 * DAY)
        first(self.db, "pending_expired", AS_OF - 7 * DAY - 1)
        first(self.db, "old_first", AS_OF - 35 * DAY)
        ret(self.db, "old_first", AS_OF - 29 * DAY)
        self.assertEqual(classifications(self.db), {
            "human:backwards": "pending", "human:future_first": "no_qualifying_first",
            "human:pending_end": "pending", "human:pending_expired": "non_return",
            "human:old_first": "counted",
        })

    def test_any_valid_in_window_return_wins_over_earlier_return(self):
        account(self.db, "window")
        first(self.db, "window", AS_OF - 31 * DAY)
        ret(self.db, "window", AS_OF - 30 * DAY)
        ret(self.db, "window", AS_OF - 30 * DAY + 1)
        account(self.db, "valid_later")
        first(self.db, "valid_later", AS_OF - DAY)
        ret(self.db, "valid_later", AS_OF - 1, session="first")
        ret(self.db, "valid_later", AS_OF)
        self.assertEqual(classifications(self.db), {
            "human:window": "counted", "human:valid_later": "counted",
        })

    def test_nonblank_identity_keys_are_opaque_and_case_sensitive(self):
        for name, human in (("base", "Key"), ("padded", " Key "), ("case", "key")):
            account(self.db, name, human=human)
            first(self.db, name, AS_OF - DAY)
            ret(self.db, name, AS_OF)
        self.assertEqual(classifications(self.db), {
            "human:Key": "counted", "human: Key ": "counted", "human:key": "counted",
        })

    def test_distinct_human_spellings_cannot_borrow_a_first_for_their_return(self):
        # Each borrower's return is 8 days after its own first, so it is
        # outside the 7-day limit. It must not qualify against the other
        # spelling's first from 1 day ago under a trimmed or case-folded join.
        account(self.db, "pad_borrower", human="Key")
        first(self.db, "pad_borrower", AS_OF - 8 * DAY)
        ret(self.db, "pad_borrower", AS_OF)
        account(self.db, "pad_owner", human=" Key ")
        first(self.db, "pad_owner", AS_OF - DAY)
        account(self.db, "case_borrower", human="Case")
        first(self.db, "case_borrower", AS_OF - 8 * DAY)
        ret(self.db, "case_borrower", AS_OF)
        account(self.db, "case_owner", human="case")
        first(self.db, "case_owner", AS_OF - DAY)
        self.assertEqual(classifications(self.db), {
            "human:Key": "non_return", "human: Key ": "pending",
            "human:Case": "non_return", "human:case": "pending",
        })

    def test_distinct_human_spellings_cannot_borrow_a_return_or_a_first(self):
        # Return owners count on their own histories; the other spelling
        # with a first but no return stays pending, and a spelling with no
        # first at all must not borrow the other spelling's first presence.
        account(self.db, "ret_owner", human="Ret")
        first(self.db, "ret_owner", AS_OF - DAY)
        ret(self.db, "ret_owner", AS_OF)
        account(self.db, "ret_borrower", human=" Ret ")
        first(self.db, "ret_borrower", AS_OF - DAY)
        account(self.db, "retcase_owner", human="RetCase")
        first(self.db, "retcase_owner", AS_OF - DAY)
        ret(self.db, "retcase_owner", AS_OF)
        account(self.db, "retcase_borrower", human="retcase")
        first(self.db, "retcase_borrower", AS_OF - DAY)
        account(self.db, "join_borrower", human="Join")
        account(self.db, "join_owner", human=" Join ")
        first(self.db, "join_owner", AS_OF - DAY)
        account(self.db, "joincase_borrower", human="JoinCase")
        account(self.db, "joincase_owner", human="joincase")
        first(self.db, "joincase_owner", AS_OF - DAY)
        self.assertEqual(classifications(self.db), {
            "human:Ret": "counted", "human: Ret ": "pending",
            "human:RetCase": "counted", "human:retcase": "pending",
            "human:Join": "no_qualifying_first", "human: Join ": "pending",
            "human:JoinCase": "no_qualifying_first", "human:joincase": "pending",
        })

    def test_nonblank_sessions_are_opaque_and_case_sensitive(self):
        for name, session in (("padded", " first "), ("case", "FIRST")):
            account(self.db, name)
            first(self.db, name, AS_OF - DAY, session="first")
            ret(self.db, name, AS_OF, session=session)
        self.assertEqual(classifications(self.db), {
            "human:padded": "counted", "human:case": "counted",
        })

    def test_identical_session_across_accounts_is_not_an_independent_return(self):
        account(self.db, "a1", human="shared")
        account(self.db, "a2", human="shared")
        first(self.db, "a1", AS_OF - DAY, session="global")
        ret(self.db, "a2", AS_OF, session="global")
        self.assertEqual(classifications(self.db), {"human:shared": "pending"})


if __name__ == "__main__":
    unittest.main()
