"""A bounded, in-memory COMMIT acknowledgement barrier for test proxies.

Callers must arm only after forwarding the intended COMMIT on an exclusive
connection. This primitive neither recognizes frontend queries nor opens sockets.
"""

import struct


class BarrierError(ValueError):
    pass


class CommitBarrier:
    """Withhold a successful COMMIT response until the controller releases it."""

    def __init__(self, max_held=65536):
        if max_held < 18:
            raise ValueError("max_held is too small for a COMMIT response")
        self.max_held = max_held
        self.state = "waiting"
        self.held = bytearray()

    def accept(self, frame):
        """Return a frame to forward, or b'' when it must be withheld."""
        if self.state in ("released", "failed"):
            raise BarrierError("barrier is no longer active")
        if len(frame) < 5 or struct.unpack_from("!I", frame, 1)[0] != len(frame) - 1:
            self.state = "failed"
            raise BarrierError("invalid typed message")
        if self.state == "waiting":
            if frame[0:1] == b"E" or frame[0:1] == b"Z":
                self.state = "failed"
                raise BarrierError("COMMIT was not acknowledged")
            if frame[0:1] != b"C":
                return frame
            if frame[5:] != b"COMMIT\0":
                self.state = "failed"
                raise BarrierError("unexpected command completion")
            self.state = "holding"
        elif self.state == "ready":
            self.state = "failed"
            raise BarrierError("message received after transaction completion")
        elif frame[0:1] == b"Z":
            if frame[5:] != b"I":
                self.state = "failed"
                raise BarrierError("transaction did not return to idle")
            self.state = "ready"
        elif frame[0:1] not in (b"N", b"S", b"A"):
            self.state = "failed"
            raise BarrierError("unexpected response after COMMIT")
        if len(self.held) + len(frame) > self.max_held:
            self.state = "failed"
            raise BarrierError("held response exceeds limit")
        self.held.extend(frame)
        return b""

    def release(self):
        if self.state != "ready":
            raise BarrierError("COMMIT response is not complete")
        self.state = "released"
        response = bytes(self.held)
        self.held.clear()
        return response
