"""Explicit release-side rehearsal for paired backend identity readback."""

import json
from time import monotonic

from production_backend_identity import WorkerPin, readback
from staging_api_identity import SHA, valid

READBACK_SECONDS = 15


def report(*, candidate_sha: str, account: str, api: WorkerPin, stream: WorkerPin, read) -> int:
    """Print a redacted verdict for independently reviewed publication pins.

    The injected reader must be silent, use uncached provider GETs, return a
    decoded result object, and enforce the shared absolute monotonic deadline
    through connection and body read. Use the existing staging_api_identity.decode
    semantics to preserve decimal allocations such as 100.0 as Decimal values.
    There is no default reader. In particular,
    the staging reader's socket inactivity timeout cannot enforce this contract.
    """
    if not (
        valid(SHA, candidate_sha)
        and type(api) is WorkerPin
        and type(stream) is WorkerPin
        and api.build_sha == stream.build_sha == candidate_sha
    ):
        result = {"identity": "unavailable", "reason": "invalid-pins"}
    else:
        deadline = monotonic() + READBACK_SECONDS

        def read_before_deadline(path):
            if monotonic() >= deadline:
                raise TimeoutError
            return read(path, deadline=deadline)

        result = readback(account=account, api=api, stream=stream, read=read_before_deadline)
        if monotonic() >= deadline:
            result = {"identity": "unavailable", "reason": "deadline-expired"}

    print(json.dumps(result))
    return 0 if result["identity"] == "matched" else 1
