"""Bounded shutdown for a subprocess started in its own POSIX session."""

import math
import os
import signal
import time


def shutdown_process_group(process, *, term_timeout=5.0, kill_timeout=5.0):
    """Stop and verify the owned group, even if its leader has exited.

    The caller must have started process with start_new_session=True and must
    call this while it still owns the group. Do not pass an arbitrary PID.
    Permission and other OS errors propagate; a group that survives SIGKILL
    raises TimeoutError. The leader is reaped through poll while waiting.
    """
    if any(not math.isfinite(t) or t < 0 for t in (term_timeout, kill_timeout)):
        raise ValueError("shutdown timeouts must be finite and non-negative")
    if process.pid <= 1 or process.pid == os.getpgrp():
        raise ValueError("process must identify an owned, separate process group")

    def send(sig):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            return False
        return True

    def disappeared(timeout):
        deadline = time.monotonic() + timeout
        while True:
            process.poll()
            if not send(0):
                return True
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            time.sleep(min(0.05, remaining))

    if not send(signal.SIGTERM) or disappeared(term_timeout):
        process.poll()
        return
    if not send(signal.SIGKILL) or disappeared(kill_timeout):
        process.poll()
        return
    raise TimeoutError(f"process group {process.pid} survived SIGKILL")
