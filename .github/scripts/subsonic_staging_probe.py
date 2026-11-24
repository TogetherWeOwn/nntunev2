"""One protected staging off-state POST bracketed by independent readbacks."""

import os
import runpy
import signal
import sys
import time
from pathlib import Path
from threading import Event

sys.path.insert(0, str(Path(__file__).resolve().parent))

from subsonic_staging_off_drain import CleanupResult, readback

RUNNER = Path(__file__).resolve().parents[2] / "scripts/subsonic-staging-runner.py"
ACCESS_ID = "SUBSONIC_PROBE_ACCESS_CLIENT_ID"
ACCESS_SECRET = "SUBSONIC_PROBE_ACCESS_CLIENT_SECRET"


def probe(env, transport, cancel):
    client_id, secret = env.get(ACCESS_ID), env.get(ACCESS_SECRET)
    if not client_id or not secret:
        return "access-config-unavailable"
    if cancel.is_set():
        return "cancelled"
    if readback(env, require_headroom=True) != CleanupResult.VERIFIED:
        return "preflight-unverified"

    result = "request-failed"
    try:
        deadline = time.monotonic() + 10
        sample = transport.probe_off(
            deadline=deadline, cancel=cancel,
            access_client_id=client_id, access_client_secret=secret,
        )
        if sample == {"oracle": "fixed_staging_off", "http_status": 404}:
            result = "verified"
    except Exception:
        pass  # Do not publish transport exceptions or response content.
    finally:
        # A failed or interrupted client does not establish backend drain.
        drained = readback(env) == CleanupResult.VERIFIED
    if not drained:
        return "postflight-unverified"
    return "cancelled" if cancel.is_set() else result


def main():
    cancel = Event()
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda *_: cancel.set())
    transport = runpy.run_path(str(RUNNER))["StagingPostTransport"]()
    result = probe(os.environ, transport, cancel)
    print(f"subsonic-staging-off-probe: {result}")
    return int(result != "verified")


if __name__ == "__main__":
    raise SystemExit(main())
