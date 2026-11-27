"""Focused, offline tests for private-trial setup recovery decisions."""

from dataclasses import replace
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from private_trial_setup_recovery import (
    ApplyIntent,
    ApplyObservation,
    CleanupDecision,
    CleanupScope,
    ConnectionIntent,
    ConnectionReadback,
    CreateDecision,
    RecoveryInputError,
    ResourceReadback,
    decide_apply_recovery,
    decide_lost_create,
    plan_cleanup,
)


DIGEST = "a" * 64
POST_DIGEST = "b" * 64
CONFIG = "c" * 32


def apply_intent():
    return ApplyIntent(
        attempt_id="trial-attempt",
        database_id="database-1",
        migration_sha256=DIGEST,
        pre_apply_digest=DIGEST,
        post_apply_digest=POST_DIGEST,
        expected_ledger=(("0063_private_trial.sql", DIGEST),),
    )

def connection_intent():
    return ConnectionIntent(
        account_id="account-1",
        request_ref="create-request-1",
        binding="UPLOAD",
        database=("nntunev2_staging", 40200),
        role=("fixture_upload", 40201),
        origin="trial.example.invalid",
        access_client_ref="access-client-1",
        origin_ca_ref="origin-ca-1",
    )

def connection_readback(intent, **changes):
    return ConnectionReadback(**(vars(intent) | {"config_id": CONFIG} | changes))

class ApplyRecoveryTests(unittest.TestCase):
    def test_unsent_apply_requires_complete_pre_apply_state(self):
        intent = apply_intent()
        self.assertEqual(
            decide_apply_recovery(intent, ApplyObservation(DIGEST, ()), "unsent").action,
            "apply",
        )
        partial = ApplyObservation(POST_DIGEST, ())
        self.assertEqual(decide_apply_recovery(intent, partial, "unsent").action, "hold")

    def test_lost_apply_ack_skips_only_a_complete_post_apply_state(self):
        intent = apply_intent()
        post = ApplyObservation(POST_DIGEST, intent.expected_ledger)
        self.assertEqual(decide_apply_recovery(intent, post, "sent_unknown").action, "skip_apply")
        self.assertEqual(decide_apply_recovery(intent, post, "unsent").action, "hold")
        partial = ApplyObservation(POST_DIGEST, ())
        self.assertEqual(decide_apply_recovery(intent, partial, "sent_unknown").action, "hold")
        self.assertEqual(
            decide_apply_recovery(intent, ApplyObservation(DIGEST, ()), "sent_unknown").action,
            "apply",
        )

    def test_retry_keeps_an_earlier_ledger_prefix(self):
        intent = apply_intent()
        earlier = (("0062_previous.sql", "d" * 64),)
        intent = ApplyIntent(**{**intent.__dict__, "expected_ledger": earlier + intent.expected_ledger})
        self.assertEqual(decide_apply_recovery(intent, ApplyObservation(DIGEST, earlier), "sent_unknown").action,
                         "apply")

    def test_apply_recovery_rejects_duplicate_ledger_rows(self):
        intent = apply_intent()
        observation = ApplyObservation(POST_DIGEST, intent.expected_ledger * 2)
        with self.assertRaisesRegex(RecoveryInputError, "duplicate migration"):
            decide_apply_recovery(intent, observation, "sent_unknown")


class LostCreateTests(unittest.TestCase):
    def test_lost_acknowledgement_binds_one_result_and_skips_create(self):
        intent = connection_intent()
        unrelated = connection_readback(intent, config_id="d" * 32, request_ref="other-request")
        decision = decide_lost_create(intent, [unrelated, connection_readback(intent)])
        self.assertEqual((decision.action, decision.config_id), ("skip_create", CONFIG))

    def test_lost_acknowledgement_holds_ambiguous_or_mismatched_results(self):
        intent = connection_intent()
        match = connection_readback(intent)
        for readbacks in (
            [],
            [match, match],
            [connection_readback(intent, request_ref="other-request")],
            [connection_readback(intent, role=("fixture_upload", 40202))],
            [connection_readback(intent, caching_disabled=1)],
            [match, replace(match, account_id="other-account", config_id="d" * 32)],
            [replace(match, request_ref="other-request"), match],
        ):
            with self.subTest(readbacks=readbacks):
                self.assertEqual(decide_lost_create(intent, readbacks),
                                 CreateDecision("hold", "create result is missing, ambiguous or out of scope"))

    def test_lost_acknowledgement_rejects_unreviewed_connection_settings(self):
        intent = connection_intent()
        pending = CreateDecision("hold", "connection settings are outside the reviewed contract")
        for unsafe in (replace(intent, sslmode="require"), replace(intent, caching_disabled=False)):
            self.assertEqual(decide_lost_create(unsafe, [connection_readback(unsafe)]), pending)

    def test_lost_acknowledgement_refuses_malformed_identities(self):
        intent = connection_intent()
        for setting in ("sslmode", "caching_disabled"):
            fields = vars(intent) | {"config_id": CONFIG}
            del fields[setting]
            with self.assertRaisesRegex(TypeError, setting):
                ConnectionReadback(**fields)
        for invalid in (replace(intent, database=("", 0)), replace(intent, role=("fixture_upload", True))):
            with self.assertRaises(RecoveryInputError):
                decide_lost_create(invalid, [connection_readback(invalid)])
        for config_id in (None, "not-an-id"):
            with self.assertRaisesRegex(RecoveryInputError, "config ID"):
                decide_lost_create(intent, [connection_readback(intent, config_id=config_id)])
        with self.assertRaisesRegex(RecoveryInputError, "database"):
            decide_lost_create(intent, [connection_readback(intent, database=("nntunev2_staging", 40200.0))])


class CleanupScopeTests(unittest.TestCase):
    def test_cleanup_selects_only_exact_attempt_owned_resources(self):
        scope = CleanupScope("attempt-1", "account-1", ("config-1", "ca-1"))
        rows = [
            ResourceReadback("config-1", "attempt-1", "account-1", "hyperdrive"),
            ResourceReadback("ca-1", "attempt-1", "account-1", "origin-ca"),
            ResourceReadback("config-sibling", "attempt-2", "account-1", "hyperdrive"),
            ResourceReadback("config-other", "attempt-1", "account-2", "hyperdrive"),
        ]
        decision = plan_cleanup(scope, rows)
        self.assertEqual(decision.action, "cleanup")
        self.assertEqual(decision.resource_ids, scope.resource_ids)

    def test_cleanup_holds_on_missing_or_repeated_scoped_readback(self):
        scope = CleanupScope("attempt-1", "account-1", ("config-1", "ca-1"))
        config = ResourceReadback("config-1", "attempt-1", "account-1", "hyperdrive")
        self.assertEqual(plan_cleanup(scope, [config]).action, "hold")
        conflict = ResourceReadback("config-1", "attempt-2", "account-1", "hyperdrive")
        foreign = ResourceReadback("config-1", "attempt-1", "account-2", "hyperdrive")
        ca = ResourceReadback("ca-1", "attempt-1", "account-1", "origin-ca")
        wrong_owner = CleanupDecision("hold", "cleanup readback conflicts with the selected scope")
        repeated = CleanupDecision("hold", "cleanup readback repeats a selected resource")
        for other in (conflict, foreign):
            self.assertEqual(plan_cleanup(scope, [other, ca]), wrong_owner)
            self.assertEqual(plan_cleanup(scope, [other, config, ca]), wrong_owner)
            self.assertEqual(plan_cleanup(scope, [config, other, ca]), repeated)
        self.assertEqual(plan_cleanup(scope, [config, config, ca]), repeated)

    def test_cleanup_rejects_invalid_scope_ids(self):
        for resource_ids in (("config-1", "config-1"), ("",), "ab"):
            with self.assertRaisesRegex(RecoveryInputError, "resource IDs"):
                plan_cleanup(CleanupScope("attempt-1", "account-1", resource_ids), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
