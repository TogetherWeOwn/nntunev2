"""Pure recovery admission checks; no provider calls or mutation authority.

The environment, reviewed receipts and serving observations must come from
trusted sources at the mutation boundary, not from the recovery request.
"""

from dataclasses import dataclass
import re


@dataclass(frozen=True)
class VersionPin:
    environment: str
    worker: str
    version: str
    source_sha: str


@dataclass(frozen=True)
class ApiQualification:
    api: VersionPin
    deployment: str
    binding: str
    schema: str
    retained_state_contract: str


@dataclass(frozen=True)
class WebQualification:
    web: VersionPin
    api: VersionPin


@dataclass(frozen=True)
class Policy:
    environment: str
    api_worker: str
    web_worker: str
    binding: str
    schema: str
    retained_state_contract: str
    api_qualifications: tuple[ApiQualification, ...]
    web_qualifications: tuple[WebQualification, ...]


@dataclass(frozen=True)
class ServingApi:
    version: VersionPin
    deployment: str


@dataclass(frozen=True)
class Admission:
    api: ApiQualification
    web: VersionPin | None = None
    preserve: ServingApi | None = None


class Refused(ValueError):
    """The requested transition is not covered by the reviewed evidence."""


def _uuid(value: str) -> bool:
    return re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", value) is not None


def _check_pin(policy: Policy, pin: VersionPin | None, worker: str) -> VersionPin:
    if (
        pin is None
        or policy.environment not in {"staging", "production"}
        or pin.environment != policy.environment
        or not worker
        or pin.worker != worker
        or not _uuid(pin.version)
        or not re.fullmatch(r"[0-9a-f]{40}", pin.source_sha)
    ):
        raise Refused("missing or mismatched version pin")
    return pin


def _qualified_api(policy: Policy, api: VersionPin | None) -> ApiQualification:
    pin = _check_pin(policy, api, policy.api_worker)
    matches = [receipt for receipt in policy.api_qualifications if receipt.api == pin]
    if len(matches) != 1:
        raise Refused("API version has no unique qualification")
    receipt = matches[0]
    compatibility = (policy.binding, policy.schema, policy.retained_state_contract)
    if (
        not all(re.fullmatch(r"[0-9a-f]{64}", value) for value in compatibility)
        or compatibility != (receipt.binding, receipt.schema, receipt.retained_state_contract)
        or not _uuid(receipt.deployment)
    ):
        raise Refused("API qualification does not cover the retained environment")
    return receipt


def admit_api(policy: Policy, target: VersionPin | None) -> Admission:
    return Admission(_qualified_api(policy, target))


def admit_web_only(policy: Policy, web: VersionPin | None, serving: ServingApi | None) -> Admission:
    if serving is None or not _uuid(serving.deployment):
        raise Refused("serving API deployment is not pinned")
    api = _qualified_api(policy, serving.version)
    return _admit_web(policy, web, api, preserve=serving)


def admit_coupled(policy: Policy, web: VersionPin | None, api_target: VersionPin | None) -> Admission:
    return _admit_web(policy, web, _qualified_api(policy, api_target))


def _admit_web(
    policy: Policy, web: VersionPin | None, api: ApiQualification, *, preserve: ServingApi | None = None
) -> Admission:
    pin = _check_pin(policy, web, policy.web_worker)
    if policy.web_qualifications.count(WebQualification(pin, api.api)) != 1:
        raise Refused("Web version is not qualified with this API version")
    return Admission(api, pin, preserve)


def require_first_activation_fallback(
    policy: Policy, candidate: VersionPin | None, fallback: VersionPin | None
) -> ApiQualification:
    pin = _check_pin(policy, candidate, policy.api_worker)
    recovery = _qualified_api(policy, fallback)
    if recovery.api.version == pin.version:
        raise Refused("activation candidate cannot be its own fallback")
    return recovery
