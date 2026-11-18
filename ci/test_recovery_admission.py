"""Inert policy fixtures, not evidence about installed mutation routes."""

from dataclasses import replace
import unittest
from uuid import UUID

from recovery_admission import (
    Admission,
    ApiQualification,
    Policy,
    Refused,
    ServingApi,
    VersionPin,
    WebQualification,
    admit_api,
    admit_coupled,
    admit_web_only,
    require_first_activation_fallback,
)


def identifier(number: int) -> str:
    return str(UUID(int=number))


API = VersionPin("staging", "example-api", identifier(1), "a" * 40)
WEB = VersionPin("staging", "example-web", identifier(2), "b" * 40)
CANDIDATE = replace(API, version=identifier(3), source_sha="c" * 40)
RECEIPT = ApiQualification(API, identifier(4), "d" * 64, "e" * 64, "f" * 64)
SERVING = ServingApi(API, identifier(5))
POLICY = Policy(
    "staging", API.worker, WEB.worker, RECEIPT.binding, RECEIPT.schema,
    RECEIPT.retained_state_contract, (RECEIPT,), (WebQualification(WEB, API),),
)


class RecoveryAdmissionTest(unittest.TestCase):
    def test_qualified_api(self):
        self.assertEqual(admit_api(POLICY, API), Admission(RECEIPT))

    def test_api_requires_the_exact_environment_worker_version_and_sha(self):
        for pin in (
            None,
            replace(API, environment="production"),
            replace(API, worker=WEB.worker),
            replace(API, version=identifier(6)),
            replace(API, source_sha=CANDIDATE.source_sha),
            replace(API, version=""),
            replace(API, source_sha=API.source_sha[:7]),
        ):
            with self.subTest(pin=pin), self.assertRaises(Refused):
                admit_api(POLICY, pin)

    def test_mismatched_qualification_does_not_count(self):
        for receipts in (
            (),
            (RECEIPT, RECEIPT),
            (replace(RECEIPT, binding="c" * 64),),
            (replace(RECEIPT, schema="c" * 64),),
            (replace(RECEIPT, retained_state_contract="c" * 64),),
            (replace(RECEIPT, retained_state_contract=""),),
            (replace(RECEIPT, deployment=""),),
        ):
            with self.subTest(receipts=receipts), self.assertRaises(Refused):
                admit_api(replace(POLICY, api_qualifications=receipts), API)

    def test_web_only_pins_the_observed_serving_deployment(self):
        self.assertEqual(admit_web_only(POLICY, WEB, SERVING), Admission(RECEIPT, WEB, SERVING))
        for serving in (None, replace(SERVING, deployment=""), replace(SERVING, version=CANDIDATE)):
            with self.subTest(serving=serving), self.assertRaises(Refused):
                admit_web_only(POLICY, WEB, serving)
        for web in (None, replace(WEB, environment="production"), replace(WEB, source_sha="c" * 40)):
            with self.subTest(web=web), self.assertRaises(Refused):
                admit_web_only(POLICY, web, SERVING)

    def test_coupled_web_has_no_admission_without_the_actual_compatible_api_target(self):
        self.assertEqual(admit_coupled(POLICY, WEB, API), Admission(RECEIPT, WEB))
        for target in (None, CANDIDATE, replace(API, environment="production")):
            with self.subTest(target=target), self.assertRaises(Refused):
                admit_coupled(POLICY, WEB, target)
        other_qualified_api = replace(RECEIPT, api=CANDIDATE)
        policy = replace(POLICY, api_qualifications=(RECEIPT, other_qualified_api))
        with self.assertRaisesRegex(Refused, "Web version is not qualified"):
            admit_coupled(policy, WEB, CANDIDATE)

    def test_first_activation_requires_a_distinct_qualified_fallback(self):
        self.assertEqual(require_first_activation_fallback(POLICY, CANDIDATE, API), RECEIPT)
        with self.assertRaises(Refused):
            require_first_activation_fallback(POLICY, CANDIDATE, None)
        pre_contract = replace(RECEIPT, retained_state_contract="0" * 64)
        with self.assertRaises(Refused):
            require_first_activation_fallback(replace(POLICY, api_qualifications=(pre_contract,)), CANDIDATE, API)
        with self.assertRaisesRegex(Refused, "cannot be its own fallback"):
            require_first_activation_fallback(POLICY, API, API)


if __name__ == "__main__":
    unittest.main()
