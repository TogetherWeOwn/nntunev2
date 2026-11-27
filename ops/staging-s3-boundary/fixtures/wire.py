"""Local HTTP/1.1 requests and SigV4 for synthetic qualification fixtures."""

from datetime import datetime, timezone
from hashlib import sha256
import hmac
from http.client import HTTPResponse
from io import BytesIO
import re
import socket
from urllib.parse import parse_qsl, quote


REGION = "us-east-1"
TOKEN = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+")
STATUS = re.compile(r"HTTP/1\.[01] ([2-5][0-9]{2})(?: [\x20-\x7e]*)?")
HEADER_VALUE = re.compile(r"[\t\x20-\x7e\x80-\xff]*")


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


def response(wire, *, decode_body=False):
    fields, separator, body = wire.partition(b"\r\n\r\n")
    if not separator:
        raise RuntimeError("invalid HTTP response framing")
    status_line, *lines = fields.decode("latin1").split("\r\n")
    status = STATUS.fullmatch(status_line)
    if status is None:
        raise RuntimeError("invalid HTTP response status")
    headers = {}
    for line in lines:
        name, colon, value = line.partition(":")
        if not colon or not TOKEN.fullmatch(name) or not HEADER_VALUE.fullmatch(value):
            raise RuntimeError("invalid HTTP response header")
        headers.setdefault(name.lower(), []).append(value.strip(" \t"))
    lengths = headers.get("content-length")
    encodings = headers.get("transfer-encoding")
    if lengths is not None:
        if (encodings is not None or len(lengths) != 1 or not lengths[0].isascii()
                or not lengths[0].isdecimal()
                or (decode_body and (lengths[0].lstrip("0") or "0") != str(len(body)))):
            raise RuntimeError("invalid fixed-length HTTP response")
    if decode_body and encodings is not None:
        if encodings != ["chunked"]:
            raise RuntimeError("unsupported HTTP response transfer encoding")
        # Use the HTTP decoder for chunked XML. HEAD and DELETE keep the
        # actual wire bytes so the caller can detect stray payloads.
        class CapturedSocket:
            def makefile(self, _mode):
                return BytesIO(wire)

        with HTTPResponse(CapturedSocket()) as reply:
            reply.begin()
            body = reply.read()
    return int(status[1]), headers, body


def connect(port):
    return socket.create_connection(("127.0.0.1", port), timeout=8)


def read_to_close(stream):
    chunks = bytearray()
    while chunk := stream.recv(65536):
        chunks.extend(chunk)
        if len(chunks) > 1024 * 1024:
            raise RuntimeError("oversized qualification response")
    return bytes(chunks)


def request_bytes(method, target, headers, body=b"", protocol="HTTP/1.1"):
    if (not TOKEN.fullmatch(method) or not re.fullmatch(r"[\x21-\x7e]+", target)
            or protocol not in ("HTTP/1.0", "HTTP/1.1")):
        raise ValueError("invalid HTTP request line")
    lines = [f"{method} {target} {protocol}", "Connection: close"]
    headers = tuple(headers)
    for name, value in headers:
        if not TOKEN.fullmatch(name) or not value.isascii() or not HEADER_VALUE.fullmatch(value):
            raise ValueError("invalid HTTP request header")
        lines.append(f"{name}: {value}")
    if body and not any(name.lower() in ("content-length", "transfer-encoding") for name, _ in headers):
        lines.append(f"Content-Length: {len(body)}")
    return "\r\n".join([*lines, "", ""]).encode("ascii") + body


def exchange(port, method, target, headers, body=b"", protocol="HTTP/1.1"):
    with connect(port) as stream:
        stream.sendall(request_bytes(method, target, headers, body, protocol))
        return response(read_to_close(stream))
