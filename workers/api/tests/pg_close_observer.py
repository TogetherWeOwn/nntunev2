"""Disposable loopback relay for observing client-side TCP closure.

This is test support only. An EOF here does not by itself prove PostgreSQL
has aborted a blocked statement or rolled back its transaction.
"""

import select
import socket
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager


def _relay(listener, stop, client_closed):
    listener.settimeout(0.2)
    deadline = time.monotonic() + 20
    while not stop.is_set():
        if time.monotonic() >= deadline:
            raise TimeoutError("close observer did not accept a client")
        try:
            client, _ = listener.accept()
            break
        except TimeoutError:
            continue
    else:
        return

    with client, socket.create_connection(("127.0.0.1", 5432), timeout=2) as server:
        client.settimeout(2)
        server.settimeout(2)
        while not stop.is_set():
            if time.monotonic() >= deadline:
                raise TimeoutError("close observer exchange expired")
            readable, _, _ = select.select([client, server], [], [], 0.2)
            for source in readable:
                data = source.recv(65536)
                if not data:
                    if source is client:
                        client_closed.set()
                    return
                (server if source is client else client).sendall(data)


@contextmanager
def close_observer():
    """Run one fixed-route exchange; the event records only client EOF."""
    stop = threading.Event()
    client_closed = threading.Event()
    with socket.socket() as listener, ThreadPoolExecutor(max_workers=1) as pool:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 15433))
        listener.listen(1)
        result = pool.submit(_relay, listener, stop, client_closed)
        try:
            yield client_closed, result
        finally:
            stop.set()
            result.result(timeout=4)
