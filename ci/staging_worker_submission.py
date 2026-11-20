"""Offline comparison for the existing staging Worker lanes; no deploy entrypoint."""

import hashlib
import re

_TARGETS = {
    (".github/workflows/deploy.yml", "staging-web"): "nntune-web-staging",
    (".github/workflows/deploy.yml", "staging-api"): "nntune-api-staging",
    (".github/workflows/deploy.yml", "staging-mb"): "nntune-mb-staging",
    (".github/workflows/deploy.yml", "staging-ingest"): "nntune-ingest-staging",
    (".github/workflows/deploy-alerts-staging.yml", "deploy"): "nntune-alerts-staging",
}


def require_reviewed_submission(
    *,
    workflow: str,
    job: str,
    approved_account: str,
    approved_body_sha256: str,
    account: str,
    worker: str,
    body: bytes,
) -> None:
    """Compare the fixed target and exact final body, without sending anything.

    The credential holder must authenticate workflow/job and load the independent
    approval (account and digest) itself. It must extract the target from the actual
    request and submit the same immutable bytes without overrides. This comparison
    does not authenticate the request operation, transport, run/attempt, provider
    bindings, side effects or rollback eligibility. It is not an admission verdict.
    """
    target = _TARGETS.get((workflow, job))
    if target is None or worker != target:
        raise ValueError("unapproved staging Worker target")
    if not re.fullmatch(r"[0-9a-f]{32}", approved_account) or account != approved_account:
        raise ValueError("unapproved Cloudflare account")
    if type(body) is not bytes or not body or hashlib.sha256(body).hexdigest() != approved_body_sha256:
        raise ValueError("submission differs from reviewed bytes")
