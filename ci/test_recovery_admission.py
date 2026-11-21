"""Inert policy fixtures, not evidence about installed mutation routes."""

import unittest
from dataclasses import replace
from uuid import UUID

from recovery_admission import (
    Admission,
    ApiDeployment,
    ApiQualification,
    Policy,
    Refused,
    VersionPin,
    WebQualification,
    admit_api,
    admit_coupled,
    admit_web_only,
    require_first_activation_fallback,
    verify_serving,
)


def identifier(number: int) -> str:
    return str(UUID(int=number))


API = VersionPin("staging", "example-api", identifier(1), "a" * 40, "1" * 64)
WEB = VersionPin("staging", "example-web", identifier(2), "b" * 40, "2" * 64)
CANDIDATE = replace(API, version_id=identifier(3), source_sha="c" * 40, build_digest="3" * 64)
TARGET = ApiDeployment(API, identifier(4), "4" * 64, "d" * 64, "e" * 64)
RECEIPT = ApiQualification(TARGET, "f" * 64)
POLICY = Policy(
    "staging", API.worker, WEB.worker, TARGET.binding, TARGET.schema,
    RECEIPT.retained_state_contract, (RECEIPT,), (WebQualification(WEB, API),),
)


class RecoveryAdmissionTest(unittest.TestCase):
    def test_qualified_api(self):
        self.assertEqual(admit_api(POLICY, TARGET), Admission(RECEIPT))

    def test_api_requires_the_exact_environment_worker_version_sha_and_build(self):
        for pin in (
            replace(API, environment="production"),
            replace(API, worker=WEB.worker),
            replace(API, version_id=identifier(6)),
            replace(API, source_sha=CANDIDATE.source_sha),
            replace(API, version_id=""),
            replace(API, source_sha=API.source_sha[:7]),
            replace(API, build_digest=CANDIDATE.build_digest),
            replace(API, build_digest=""),
        ):
            with self.subTest(pin=pin), self.assertRaises(Refused):
                admit_api(POLICY, replace(TARGET, version=pin))

    def test_api_requires_the_qualified_deployment_and_routing(self):
        for target in (
            None,
            replace(TARGET, deployment=""),
            replace(TARGET, deployment=identifier(5)),
            replace(TARGET, routing=""),
            replace(TARGET, routing="5" * 64),
        ):
            with self.subTest(target=target), self.assertRaises(Refused):
                admit_api(POLICY, target)

    def test_mismatched_qualification_does_not_count(self):
        for receipts in (
            (),
            (RECEIPT, RECEIPT),
            (replace(RECEIPT, retained_state_contract="c" * 64),),
            (replace(RECEIPT, retained_state_contract=""),),
        ):
            with self.subTest(receipts=receipts), self.assertRaises(Refused):
                admit_api(replace(POLICY, api_qualifications=receipts), TARGET)
        for target in (replace(TARGET, binding="c" * 64), replace(TARGET, schema="c" * 64)):
            with self.subTest(target=target), self.assertRaises(Refused):
                admit_api(replace(POLICY, api_qualifications=(replace(RECEIPT, api=target),)), target)

    def test_web_only_requires_the_exact_qualified_serving_api(self):
        self.assertEqual(admit_web_only(POLICY, WEB, TARGET), Admission(RECEIPT, WEB, preserve_api=True))
        for serving in (None, replace(TARGET, deployment=identifier(5)), replace(TARGET, routing="5" * 64)):
            with self.subTest(serving=serving), self.assertRaises(Refused):
                admit_web_only(POLICY, WEB, serving)
        for web in (None, replace(WEB, environment="production"), replace(WEB, source_sha="c" * 40)):
            with self.subTest(web=web), self.assertRaises(Refused):
                admit_web_only(POLICY, web, TARGET)

    def test_coupled_web_has_no_admission_without_the_actual_compatible_api_target(self):
        self.assertEqual(admit_coupled(POLICY, WEB, TARGET), Admission(RECEIPT, WEB))
        other_api = replace(TARGET, version=CANDIDATE)
        for target in (None, other_api, replace(TARGET, deployment=identifier(5))):
            with self.subTest(target=target), self.assertRaises(Refused):
                admit_coupled(POLICY, WEB, target)
        policy = replace(POLICY, api_qualifications=(RECEIPT, replace(RECEIPT, api=other_api)))
        with self.assertRaisesRegex(Refused, "Web version is not qualified"):
            admit_coupled(policy, WEB, other_api)

    def test_first_activation_requires_a_distinct_qualified_fallback(self):
        self.assertEqual(require_first_activation_fallback(POLICY, CANDIDATE, TARGET), RECEIPT)
        with self.assertRaises(Refused):
            require_first_activation_fallback(POLICY, CANDIDATE, None)
        pre_contract = replace(RECEIPT, retained_state_contract="0" * 64)
        with self.assertRaises(Refused):
            require_first_activation_fallback(replace(POLICY, api_qualifications=(pre_contract,)), CANDIDATE, TARGET)
        with self.assertRaisesRegex(Refused, "cannot be its own fallback"):
            require_first_activation_fallback(POLICY, API, TARGET)

    def test_web_only_readback_requires_same_api_and_expected_web(self):
        admitted = admit_web_only(POLICY, WEB, TARGET)
        verify_serving(admitted, api=TARGET, web=WEB)
        with self.assertRaises(Refused):
            verify_serving(admitted, api=TARGET, web=WEB, executed_api_deployment=TARGET.deployment)
        with self.assertRaises(Refused):
            verify_serving(admitted, api=TARGET, web=replace(WEB, build_digest="9" * 64))
        for api in (replace(TARGET, deployment=identifier(5)), replace(TARGET, routing="5" * 64)):
            with self.subTest(api=api), self.assertRaises(Refused):
                verify_serving(admitted, api=api, web=WEB)

    def test_api_restore_readback_matches_executed_deployment_and_qualified_destination(self):
        admitted = admit_coupled(POLICY, WEB, TARGET)
        restored = replace(TARGET, deployment=identifier(5))
        verify_serving(admitted, api=restored, web=WEB, executed_api_deployment=restored.deployment)
        with self.assertRaises(Refused):
            verify_serving(admitted, api=restored, web=WEB)
        with self.assertRaises(Refused):
            verify_serving(admitted, api=restored, executed_api_deployment=restored.deployment)
        for api in (
            TARGET,
            replace(restored, version=CANDIDATE),
            replace(restored, routing="5" * 64),
            replace(restored, binding="5" * 64),
            replace(restored, schema="5" * 64),
        ):
            with self.subTest(api=api), self.assertRaises(Refused):
                verify_serving(admitted, api=api, web=WEB, executed_api_deployment=restored.deployment)


if __name__ == "__main__":
    unittest.main()
