#!/usr/bin/env python3
"""Experimental immutable consumer input; not a build provenance certificate.

The caller must authenticate the build, verifier, consumer and environment. This
helper is deliberately not connected to candidate admission or the CI workflow.
"""
import fcntl
import hashlib
import os
import subprocess

MAX_CANDIDATE_BYTES = 64 * 1024 * 1024
SEALS = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL


def consume_sealed(candidate: bytes, verify, command: list[str], *, cwd, env, run=subprocess.run) -> str:
    """Verify a sealed snapshot, then pass that exact snapshot to a child.

    Linux memfd seals prevent writes and truncation even through another open
    descriptor. The verifier must raise on rejection; its return value is ignored.
    The child must open its final argument as the input file. This makes no claim
    about where candidate came from or what the consumer does.
    """
    if type(candidate) is not bytes or not candidate or len(candidate) > MAX_CANDIDATE_BYTES:
        raise ValueError("invalid candidate bytes")
    command = list(command)
    if not command or not os.path.isabs(command[0]):
        raise ValueError("consumer must be an absolute executable path")
    fd = os.memfd_create("worker-candidate", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
    try:
        view = memoryview(candidate)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise OSError("failed to write candidate")
            view = view[written:]
        fcntl.fcntl(fd, fcntl.F_ADD_SEALS, SEALS)
        if fcntl.fcntl(fd, fcntl.F_GET_SEALS) & SEALS != SEALS:
            raise ValueError("candidate seals are incomplete")
        path = f"/proc/self/fd/{fd}"
        with open(path, "rb") as sealed:
            snapshot = sealed.read(MAX_CANDIDATE_BYTES + 1)
        if snapshot != candidate:
            raise ValueError("sealed candidate differs from input")
        verify(snapshot)
        run([*command, path], cwd=cwd, env=env, pass_fds=(fd,), check=True)
        return hashlib.sha256(snapshot).hexdigest()
    finally:
        os.close(fd)
