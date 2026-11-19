"""Single-connection COMMIT fault for disposable hosted PostgreSQL fixtures."""

import importlib.util
import os
import re
import select
import socket
import struct
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from pathlib import Path


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


frames = load("pg_protocol_frames")
barrier = load("pg_commit_barrier")
ProtocolError = frames.ProtocolError
BILLING_DB = "nntune_billing_ack"
COMMIT = re.compile(rb"\s*COMMIT\s*;?\s*\x00", re.IGNORECASE)
COMMIT_WORD = re.compile(rb"\bCOMMIT\b", re.IGNORECASE)


class CommitExchange:
    database = BILLING_DB
    native = False

    def __init__(self):
        self.startup = bytearray()
        self.front = frames.FrameDecoder()
        self.back = frames.FrameDecoder()
        self.outstanding = 1  # startup or one per simple query / Sync
        self.extended = False
        self.status = b""
        self.gate = None
        self.rejected = False

    def frontend(self, data):
        if self.gate:
            raise ProtocolError("frontend data after COMMIT")
        forward = []
        if self.startup is not None:
            self.startup.extend(data)
            if len(self.startup) < 4:
                return forward
            size = struct.unpack_from("!I", self.startup)[0]
            if not 8 <= size <= 16384:
                raise ProtocolError("invalid startup length")
            if len(self.startup) < size:
                return forward
            if self.startup[4:8] != b"\0\x03\0\0":
                raise ProtocolError("expected plaintext protocol 3.0 startup")
            payload = self.startup[8:size]
            parts = bytes(payload[:-2]).split(b"\0")
            params = dict(zip(parts[::2], parts[1::2]))
            if (not payload.endswith(b"\0\0") or 2 * len(params) != len(parts)
                    or params.get(b"database") != self.database.encode()
                    or params.get(b"user") != b"postgres"):
                raise ProtocolError("unexpected disposable database startup")
            forward.append(bytes(self.startup[:size]))
            data = self.startup[size:]
            self.startup = None
        messages = self.front.feed(data)
        for i, frame in enumerate(messages):
            kind, body = frame[:1], frame[5:]
            commit = kind == b"Q" and COMMIT.fullmatch(body)
            if not commit and kind in (b"Q", b"P") and COMMIT_WORD.search(body):
                raise ProtocolError("unsupported COMMIT form")
            if kind == b"Q":
                if self.extended or not body.endswith(b"\0") or b"\0" in body[:-1]:
                    raise ProtocolError("invalid simple query")
                if commit:
                    if (self.outstanding or self.back.buffer or self.status not in (b"T", b"E")
                            or i != len(messages) - 1 or self.front.buffer):
                        raise ProtocolError("COMMIT must be in a drained transaction with no pipeline")
                    self.gate = barrier.CommitBarrier()
                self.outstanding += 1
            elif kind == b"S":
                self.outstanding += 1
                self.extended = False
            elif kind in (b"P", b"B", b"D", b"E", b"C"):
                self.extended = True
            elif kind not in (b"H", b"X", b"p"):
                raise ProtocolError("unsupported frontend message")
            forward.append(frame)
        return forward

    def backend(self, data):
        forward = []
        for frame in self.back.feed(data):
            if self.rejection_drained:
                raise ProtocolError("message received after COMMIT rejection")
            kind, body = frame[:1], frame[5:]
            if kind == b"Z":
                if not self.outstanding or body not in (b"I", b"T", b"E"):
                    raise ProtocolError("unexpected ReadyForQuery")
                self.outstanding -= 1
                self.status = body
            if self.gate is None:
                forward.append(frame)
            elif self.rejected:
                if kind not in (b"Z", b"N", b"S", b"A") or (kind == b"Z" and body != b"I"):
                    raise ProtocolError("unexpected response after COMMIT rejection")
                forward.append(frame)
            elif (self.native and self.gate.state == "waiting"
                  and (kind == b"E" or (kind == b"C" and body == b"ROLLBACK\0"))):
                # Preserve PostgreSQL's definitive failure instead of replacing
                # it with EOF. Only successful COMMIT may arm the fault.
                self.rejected = True
                forward.append(frame)
            else:
                if kind not in (b"C", b"Z", b"E", b"N", b"S", b"A"):
                    raise ProtocolError("unexpected COMMIT response")
                if self.gate.accept(frame) and self.native:
                    forward.append(frame)
        if (self.withheld or self.rejection_drained) and self.back.buffer:
            raise ProtocolError("trailing partial frame after COMMIT")
        return forward

    @property
    def withheld(self):
        return self.gate is not None and self.gate.state == "ready"

    @property
    def rejection_drained(self):
        return self.rejected and not self.outstanding and self.status == b"I"


class NativeExchange(CommitExchange):
    database = "nntune_ci"
    native = True


def run(listener, stop, exchange=None):
    deadline = time.monotonic() + 25

    def readable(*sockets):
        while not stop.is_set() and time.monotonic() < deadline:
            ready, _, _ = select.select(sockets, [], [], 0.1)
            if ready:
                return ready
        raise ProtocolError("relay cancelled or timed out")

    readable(listener)
    client, _ = listener.accept()
    listener.close()  # no second connection
    with client, socket.create_connection(("127.0.0.1", 5432), timeout=1) as server:
        client.settimeout(1)
        exchange = exchange if exchange is not None else CommitExchange()
        while not (exchange.withheld or exchange.rejection_drained):
            ready = readable(client, server)
            # Check client first when both sides are ready, so pipelined input
            # cannot be mistaken for a successful isolated COMMIT.
            for source, dest, decode in ((client, server, exchange.frontend),
                                         (server, client, exchange.backend)):
                if source in ready:
                    data = source.recv(65536)
                    if not data:
                        if exchange.native:
                            return b"U"  # EOF without proof, even if COMMIT was sent.
                        raise ProtocolError("peer closed before a successful COMMIT response")
                    for frame in decode(data):
                        if stop.is_set() or time.monotonic() >= deadline:
                            raise ProtocolError("relay cancelled or timed out")
                        dest.sendall(frame)
        if exchange.rejection_drained:
            return b"R"
        # The server's readiness snapshot may predate queued frontend input.
        if select.select((client,), [], [], 0)[0]:
            raise ProtocolError("frontend data after COMMIT")
    # Both connections have closed without releasing the barrier.
    return b"W"


@contextmanager
def commit_relay():
    stop = threading.Event()
    with socket.socket() as listener, ThreadPoolExecutor(max_workers=1) as pool:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 15432))
        listener.listen(1)
        listener.settimeout(1)
        result = pool.submit(run, listener, stop)
        try:
            yield result
        finally:
            stop.set()


NATIVE_CI_URL = "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci"


def native_main():
    if (sys.argv[1:] != ["--native-ci"]
            or os.environ.get("GITHUB_ACTIONS") != "true"
            or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted"
            or os.environ.get("NNTUNE_TEST_DATABASE_URL") != NATIVE_CI_URL):
        raise SystemExit("native COMMIT relay requires the disposable hosted Actions fixture")
    # Private control pipe: two port bytes, then one outcome byte only after
    # both sockets close. The supplied URL is an admission check, not a target.
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(1)
        sys.stdout.buffer.write(struct.pack("!H", listener.getsockname()[1]))
        sys.stdout.buffer.flush()
        outcome = run(listener, threading.Event(), NativeExchange())
    sys.stdout.buffer.write(outcome)


if __name__ == "__main__":
    native_main()
