"""Offline comparison for the existing staging Worker lanes; no deploy entrypoint."""

import hashlib
import re

_TARGETS = {
    (".github/workflows/deploy.yml", "staging-web"): "nntune-web-staging",
    (".github/workflows/deploy.yml", "staging-api"): "nntune-api-staging",
    (".github/workflows/deploy.yml", "staging-mb"): "nntune-mb-staging",
    (".github/workflows/deploy.yml", "staging-ingest"): "nntune-ingest-staging",
    (".github/workflows/deploy.yml", "staging-stream"): "nntune-stream-staging",
    (".github/workflows/deploy-alerts-staging.yml", "deploy"): "nntune-alerts-staging",
}
_VERSION_ID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")


def require_reviewed_submission(
    *,
    workflow: str,
    job: str,
    approved_account: str,
    approved_worker: str,
    approved_source_sha: str,
    approved_operation: str,
    approved_body_sha256: str,
    approved_version: str | None,
    account: str,
    worker: str,
    source_sha: str,
    operation: str,
    body: bytes,
    version: str | None,
) -> None:
    """Compare the fixed target, source revision, operation and final body offline.

    The credential holder must authenticate workflow/job and load the independent
    approval itself. The source SHA comes from the candidate build provenance or
    the recorded build provenance of the version being restored. The holder must
    derive the target and operation from the final request and, for rollback,
    extract the explicit version from its body. It must send those same immutable
    bytes without overrides. This comparison cannot establish approval provenance,
    HTTP request semantics, provider state or rollback eligibility.
    """
    target = _TARGETS.get((workflow, job))
    if target is None or worker != target or worker != approved_worker:
        raise ValueError("unapproved staging Worker target")
    if not re.fullmatch(r"[0-9a-f]{32}", approved_account) or account != approved_account:
        raise ValueError("unapproved Cloudflare account")
    if not re.fullmatch(r"[0-9a-f]{40}", approved_source_sha) or source_sha != approved_source_sha:
        raise ValueError("unapproved source revision")
    if operation not in ("deploy", "rollback") or operation != approved_operation:
        raise ValueError("unapproved deployment operation")
    if operation == "rollback":
        if (
            approved_version is None
            or not _VERSION_ID.fullmatch(approved_version)
            or version != approved_version
        ):
            raise ValueError("unapproved rollback version")
    elif approved_version is not None or version is not None:
        raise ValueError("unexpected version for deployment")
    if type(body) is not bytes or not body or hashlib.sha256(body).hexdigest() != approved_body_sha256:
        raise ValueError("submission differs from reviewed bytes")
