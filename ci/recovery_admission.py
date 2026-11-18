"""Pure recovery admission checks; no provider calls or mutation authority.

The environment, resolved provider destination, reviewed receipts and serving
observations must come from trusted sources at the mutation boundary, not from
the recovery request. These checks do not acquire or serialize write authority.
"""

import re
from dataclasses import dataclass, replace


@dataclass(frozen=True)
class VersionPin:
    environment: str
    worker: str
    version_id: str
    source_sha: str
    build_digest: str


@dataclass(frozen=True)
class ApiDeployment:
    version: VersionPin
    deployment: str
    # Canonical identity of effective routes, service targets and traffic weights.
    routing: str
    binding: str
    schema: str


@dataclass(frozen=True)
class ApiQualification:
    api: ApiDeployment
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
class Admission:
    api: ApiQualification
    web: VersionPin | None = None
    preserve_api: bool = False


class Refused(ValueError):
    """The requested transition is not covered by the reviewed evidence."""


def _uuid(value: str) -> bool:
    return re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", value) is not None


def _digest(value: str) -> bool:
    return re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _check_pin(policy: Policy, pin: VersionPin | None, worker: str) -> VersionPin:
    if (
        pin is None
        or policy.environment not in {"staging", "production"}
        or pin.environment != policy.environment
        or not worker
        or pin.worker != worker
        or not _uuid(pin.version_id)
        or not re.fullmatch(r"[0-9a-f]{40}", pin.source_sha)
        or not _digest(pin.build_digest)
    ):
        raise Refused("missing or mismatched version pin")
    return pin


def _qualified_api(policy: Policy, api: ApiDeployment | None) -> ApiQualification:
    if api is None or not _uuid(api.deployment) or not _digest(api.routing):
        raise Refused("API deployment or routing is not pinned")
    _check_pin(policy, api.version, policy.api_worker)
    matches = [receipt for receipt in policy.api_qualifications if receipt.api == api]
    if len(matches) != 1:
        raise Refused("API deployment has no unique qualification")
    receipt = matches[0]
    compatibility = (policy.binding, policy.schema, policy.retained_state_contract)
    if not all(map(_digest, compatibility)) or compatibility != (
        api.binding, api.schema, receipt.retained_state_contract,
    ):
        raise Refused("API qualification does not cover the retained environment")
    return receipt


def admit_api(policy: Policy, target: ApiDeployment | None) -> Admission:
    return Admission(_qualified_api(policy, target))


def admit_web_only(policy: Policy, web: VersionPin | None, serving: ApiDeployment | None) -> Admission:
    return _admit_web(policy, web, _qualified_api(policy, serving), preserve_api=True)


def admit_coupled(policy: Policy, web: VersionPin | None, api_target: ApiDeployment | None) -> Admission:
    return _admit_web(policy, web, _qualified_api(policy, api_target))


def _admit_web(
    policy: Policy, web: VersionPin | None, api: ApiQualification, *, preserve_api: bool = False
) -> Admission:
    pin = _check_pin(policy, web, policy.web_worker)
    if policy.web_qualifications.count(WebQualification(pin, api.api.version)) != 1:
        raise Refused("Web version is not qualified with this API version")
    return Admission(api, pin, preserve_api)


def require_first_activation_fallback(
    policy: Policy, candidate: VersionPin | None, fallback: ApiDeployment | None
) -> ApiQualification:
    pin = _check_pin(policy, candidate, policy.api_worker)
    recovery = _qualified_api(policy, fallback)
    if recovery.api.version.version_id == pin.version_id:
        raise Refused("activation candidate cannot be its own fallback")
    return recovery


def verify_serving(
    admitted: Admission, *, api: ApiDeployment, web: VersionPin | None = None,
    executed_api_deployment: str | None = None,
) -> None:
    """Check independent serving readback, not just the provider write response.

    An API restore can issue a new deployment ID for an already qualified
    version. Use the executed mutation receipt for that ID, never the request.
    Health and retained-state behavior evidence are separate requirements.
    """
    expected = admitted.api.api
    if admitted.preserve_api:
        if executed_api_deployment is not None:
            raise Refused("Web-only recovery recorded an API mutation")
        deployment = expected.deployment
    else:
        if executed_api_deployment is None or not _uuid(executed_api_deployment):
            raise Refused("executed API deployment is not pinned")
        deployment = executed_api_deployment
    if api != replace(expected, deployment=deployment):
        raise Refused("serving API does not match the admitted operation")
    if admitted.web is not None and web != admitted.web:
        raise Refused("serving Web version does not match the admitted operation")
