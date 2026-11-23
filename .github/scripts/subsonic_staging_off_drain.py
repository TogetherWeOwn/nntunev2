"""Protected staging read adapter for the off-and-drained verifier (Linux CLI)."""

import os
import re
import signal
import sys
import time
from pathlib import Path

# Isolated mode excludes the script directory. Only import our checked-out siblings.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from staging_api_identity import SCRIPT, provider_result, valid
from subsonic_staging_cleanup import CleanupResult, DeploymentPin, verify_cleanup
from subsonic_staging_observer import sample_pressure

BUDGET_SECONDS = 30


class ReadDeadline(BaseException):
    """Do not let the verifier's callback error classifier swallow the alarm."""


def expire(_signal, _frame):
    raise ReadDeadline


def readback(env):
    account = env.get("CLOUDFLARE_ACCOUNT_ID")
    token = env.get("SUBSONIC_STAGING_WORKERS_READ_TOKEN")
    if not (
        valid(re.compile(r"[0-9a-f]{32}"), account)
        and valid(re.compile(r"[\x21-\x7e]{1,512}"), token)
        and env.get("PLANETSCALE_OBSERVER_URL_STAGING")
    ):
        return "readback-config-unavailable"

    pin = DeploymentPin(
        *(
            env.get(f"EXPECTED_STAGING_{name}", "")
            for name in ("DEPLOYMENT_ID", "VERSION_ID", "BUILD_SHA")
        )
    )
    path = f"/client/v4/accounts/{account}/workers/scripts/{SCRIPT}"
    deadline_ms = int((time.monotonic() + BUDGET_SECONDS) * 1000)

    def read(suffix, cutoff_ms):
        if time.monotonic() * 1000 >= cutoff_ms:
            raise ReadDeadline
        return provider_result(path + suffix, token)

    previous = signal.signal(signal.SIGALRM, expire)
    try:
        remaining = deadline_ms / 1000 - time.monotonic()
        if remaining <= 0:
            return CleanupResult.DEADLINE
        signal.setitimer(signal.ITIMER_REAL, remaining)
        return verify_cleanup(
            pin,
            deadline_ms=deadline_ms,
            read_deployments=lambda *, deadline_ms: read("/deployments", deadline_ms),
            read_version=lambda version, *, deadline_ms: read(
                f"/versions/{version}", deadline_ms
            ),
            sample_pressure=lambda *, deadline_ms: sample_pressure(
                env, deadline_ms=deadline_ms
            ),
        )
    except ReadDeadline:
        return CleanupResult.DEADLINE
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def main():
    result = readback(os.environ)
    print(f"subsonic-staging-off-drain: {result}")
    return int(result != CleanupResult.VERIFIED)


if __name__ == "__main__":
    raise SystemExit(main())
