"""Local HTTP/1.1 requests and SigV4 for synthetic qualification fixtures."""

from datetime import datetime, timezone
from hashlib import sha256
import hmac
import socket
from urllib.parse import parse_qsl, quote


REGION = "us-east-1"


def signed(method, target, host, credential, body=b"", extra=(), service="s3"):
    access, secret = credential
    date = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    scope = f"{date[:8]}/{REGION}/{service}/aws4_request"
    payload = sha256(body).hexdigest()
    headers = {"host": host, "x-amz-content-sha256": payload, "x-amz-date": date}
    headers.update(extra)
    names = ";".join(sorted(headers))
    fields = "".join(f"{name}:{headers[name]}\n" for name in sorted(headers))
    path, _, query = target.partition("?")
    items = sorted((quote(k, safe=""), quote(v, safe="")) for k, v in parse_qsl(query, keep_blank_values=True))
    canonical_query = "&".join(f"{k}={v}" for k, v in items)
    canonical = f"{method}\n{path}\n{canonical_query}\n{fields}\n{names}\n{payload}"
    message = f"AWS4-HMAC-SHA256\n{date}\n{scope}\n{sha256(canonical.encode()).hexdigest()}"
    key = f"AWS4{secret}".encode()
    for part in scope.split("/"):
        key = hmac.digest(key, part.encode(), "sha256")
    signature = hmac.digest(key, message.encode(), "sha256").hex()
    headers["authorization"] = (
        f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={names}, Signature={signature}"
    )
    return list(headers.items())


def response(wire):
    fields, separator, body = wire.partition(b"\r\n\r\n")
    if not separator or not fields.startswith(b"HTTP/1."):
        raise RuntimeError("invalid HTTP response framing")
    status, *lines = fields.decode("latin1").split("\r\n")
    headers = {}
    for line in lines:
        name, value = line.split(":", 1)
        headers.setdefault(name.lower(), []).append(value.strip())
    return int(status.split()[1]), headers, body


def connect(port):
    return socket.create_connection(("127.0.0.1", port), timeout=8)


def read_to_close(stream):
    chunks = bytearray()
    while chunk := stream.recv(65536):
        chunks.extend(chunk)
        if len(chunks) > 1024 * 1024:
            raise RuntimeError("oversized qualification response")
    return bytes(chunks)


def exchange(port, method, target, headers, body=b"", protocol="HTTP/1.1"):
    lines = [f"{method} {target} {protocol}", "Connection: close"]
    lines.extend(f"{name}: {value}" for name, value in headers)
    with connect(port) as stream:
        stream.sendall("\r\n".join([*lines, "", ""]).encode("ascii") + body)
        return response(read_to_close(stream))
