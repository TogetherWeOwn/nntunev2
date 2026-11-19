"""Bounded PostgreSQL wire framing for disposable loopback test controls.

This module does not open sockets or interpret SQL. Startup and TLS negotiation
must be handled separately before using the typed-message decoder.
"""

import struct


class ProtocolError(ValueError):
    pass


class FrameDecoder:
    """Incrementally decode typed PostgreSQL messages without trusting lengths."""

    def __init__(self, max_frame=1024 * 1024):
        if max_frame < 5:
            raise ValueError("max_frame must include a type and length")
        self.max_frame = max_frame
        self.buffer = bytearray()

    def feed(self, data):
        frames = []
        # Process a bounded slice at a time, even if the caller supplies a large
        # coalesced read containing many small messages.
        view = memoryview(data)
        while view:
            needed = 5 - len(self.buffer) if len(self.buffer) < 5 else self._size() - len(self.buffer)
            take = min(needed, len(view))
            self.buffer.extend(view[:take])
            view = view[take:]
            if len(self.buffer) >= 5:
                size = self._size()
                if len(self.buffer) == size:
                    frames.append(bytes(self.buffer))
                    self.buffer.clear()
        return frames

    def _size(self):
        length = struct.unpack_from("!I", self.buffer, 1)[0]
        if length < 4 or length + 1 > self.max_frame:
            raise ProtocolError("invalid PostgreSQL message length")
        return length + 1

    def finish(self):
        if self.buffer:
            raise ProtocolError("truncated PostgreSQL message")
