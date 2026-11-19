#!/usr/bin/env python3
"""Experimental immutable consumer input; not a build provenance certificate.

The caller must authenticate the build, verifier, consumer and environment. This
helper is used by CI compatibility experiments, not candidate admission.
"""
import fcntl
import hashlib
import os
import stat
import subprocess
import tempfile
from pathlib import Path

MAX_CANDIDATE_BYTES = 64 * 1024 * 1024
SEALS = fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL


def snapshot_build_output(path) -> bytes:
    """Read a bounded regular output without following its final symlink.

    This does not authenticate the build or protect parent directories or a
    concurrently modified file. The caller must control those separately; the
    returned bytes can be passed to consume_sealed for inspection and consumption.
    """
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= MAX_CANDIDATE_BYTES:
            raise ValueError("invalid candidate output file")
        # Reject short reads and probe for growth without buffered read-ahead.
        with os.fdopen(fd, "rb", buffering=0, closefd=False) as output:
            candidate = output.read(info.st_size)
            if len(candidate) != info.st_size or output.read(1):
                raise ValueError("candidate output size changed while reading")
        return candidate
    finally:
        os.close(fd)


def experiment_build_and_consume(build, verify, command, *, cwd, env, run=subprocess.run) -> str:
    """Exercise a fresh output path through inspection and consumption.

    The callback must finish writing and return None on success, or raise on
    failure. This experiment does not authenticate the callback, its tools or
    environment, nor exclude concurrent mutation by another process. It must not
    be used as an admission certificate.
    """
    with tempfile.TemporaryDirectory(prefix="worker-candidate-") as directory:
        output = Path(directory) / "candidate.wasm"
        if build(output) is not None:
            raise ValueError("build callback must return None on success and raise on failure")
        candidate = snapshot_build_output(output)
        return consume_sealed(candidate, verify, command, cwd=cwd, env=env, run=run)


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
