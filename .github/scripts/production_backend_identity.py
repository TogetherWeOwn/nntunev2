"""Read the serving API and Stream identities for the private release watch."""

import re
from collections.abc import Callable
from typing import NamedTuple

from staging_api_identity import SHA, UUID, current_deployment, require, valid, verify_build

ACCOUNT = re.compile(r"[0-9a-f]{32}")


class WorkerPin(NamedTuple):
    worker: str
    build_sha: str
    version_id: str
    deployment_id: str


def valid_pin(pin: WorkerPin, worker: str) -> bool:
    return (
        type(pin) is WorkerPin
        and type(pin.worker) is str
        and pin.worker == worker
        and all(type(value) is str and valid(pattern, value) for pattern, value in (
            (SHA, pin.build_sha), (UUID, pin.version_id), (UUID, pin.deployment_id),
        ))
    )


def readback(
    *, account: str, api: WorkerPin, stream: WorkerPin, read: Callable[[str], object]
) -> dict[str, str]:
    """Compare reviewed pins for one cutover SHA and return only outcome fields.

    The caller supplies the reviewed account and a silent, uncached provider GET
    reader that returns the decoded result object. The caller must enforce one
    elapsed-time deadline across the entire readback, including in-flight reads,
    and treat expiry as unavailable. The staging provider_result socket timeout
    only limits inactivity.
    Both serving allocations bracket the immutable version reads. This is
    point-in-time allocation evidence. Route health and access are separate.
    """
    if not (
        type(account) is str and valid(ACCOUNT, account)
        and valid_pin(api, "nntune-api")
        and valid_pin(stream, "nntune-stream")
        and api.build_sha == stream.build_sha
        and api.version_id != stream.version_id
        and api.deployment_id != stream.deployment_id
    ):
        return {"identity": "unavailable", "reason": "invalid-pins"}

    base = f"/client/v4/accounts/{account}/workers/scripts"
    targets = (("api", api, f"{base}/{api.worker}"), ("stream", stream, f"{base}/{stream.worker}"))

    def require_allocation(pin, path):
        observed = current_deployment(read(path + "/deployments"), pin.version_id)
        require(observed == pin.deployment_id, "deployment-id-mismatch")

    try:
        reason = "deployment-unverified"
        for backend, pin, path in targets:
            require_allocation(pin, path)
        reason = "build-unverified"
        for backend, pin, path in targets:
            verify_build(read(f"{path}/versions/{pin.version_id}"), pin.version_id, pin.build_sha)
        reason = "deployment-unverified"
        for backend, pin, path in reversed(targets):
            require_allocation(pin, path)
    except Exception:  # Redact all reader and parser diagnostics at the evidence boundary.
        return {"identity": "unavailable", "backend": backend, "reason": reason}
    return {"identity": "matched"}
