"""Exercise the generated policy against a recording HTTP upstream."""

from base64 import b64encode
from dataclasses import dataclass
from hashlib import md5, sha256
import json
from pathlib import Path

from render import BUCKET, render
from wire import exchange, response, signed


GENERATION = "10000000000040008000000000000000"
OBJECT = "00000000000040008000000000000000"
KEY = f"media/{OBJECT}/{GENERATION}"
PATH = f"/{BUCKET}/{KEY}"
OTHER = f"/{BUCKET}/media/{GENERATION}/{OBJECT}"
DIGEST = f"media-digests/{OBJECT}/{GENERATION}/{GENERATION}/00000000"


def require(condition, label):
    if not condition:
        raise RuntimeError(label)


@dataclass
class Boundary:
    port: int
    credential: tuple

    def request(self, method, target, body=b"", *, headers=(), identity=None, protocol="HTTP/1.1"):
        fields = signed(method, target, "fixtures.invalid", identity or self.credential, body)
        return exchange(self.port, method, target, fields + list(headers), body, protocol)


def require_denial(reply, label):
    status, headers, body = reply
    require((status, headers.get("x-nntune-boundary"), body) == (403, ["deny"], b""), label)
    require(headers.get("cache-control") == ["no-store"], label + " cache policy")


def bulk_body(version):
    return (f"<Delete><Object><Key>{KEY}</Key><VersionId>{version}</VersionId>"
            "</Object></Delete>").encode()


def bulk_checksum(body):
    return b64encode(md5(body, usedforsecurity=False).digest()).decode()


def denied_matrix(boundary, version):
    target = f"{PATH}?versionId={version}"
    body = bulk_body(version)
    for label, method, path, payload, headers in (
        ("list all", "GET", "/", b"", ()),
        ("list objects", "GET", f"/{BUCKET}?list-type=2", b"", ()),
        ("list versions", "GET", f"/{BUCKET}?versions", b"", ()),
        ("bulk delete", "POST", f"/{BUCKET}?delete", body,
         (("Content-Length", str(len(body))), ("Content-MD5", bulk_checksum(body)))),
        ("put", "PUT", PATH, b"x", (("Content-Length", "1"),)),
        ("multipart", "POST", f"{PATH}?uploads", b"", ()),
        ("unversioned HEAD", "HEAD", PATH, b"", ()),
        ("unversioned GET", "GET", PATH, b"", ()),
        ("unversioned DELETE", "DELETE", PATH, b"", ()),
        ("versioned GET", "GET", target, b"", ()),
        ("other path", "DELETE", f"{OTHER}?versionId={version}", b"", ()),
        ("other bucket", "HEAD", target.replace(BUCKET, BUCKET + "-other"), b"", ()),
        ("empty version", "DELETE", f"{PATH}?versionId=", b"", ()),
        ("null version", "DELETE", f"{PATH}?versionId=null", b"", ()),
        ("encoded null version", "DELETE", f"{PATH}?versionId=%6Eull", b"", ()),
        ("malformed escape", "DELETE", f"{PATH}?versionId={version}%", b"", ()),
        ("encoded control", "DELETE", f"{PATH}?versionId=%00", b"", ()),
        ("oversized version", "DELETE", f"{PATH}?versionId=" + "a" * 1025, b"", ()),
        ("extra query", "DELETE", target + "&delete", b"", ()),
        ("duplicate query", "DELETE", target + "&versionId=other", b"", ()),
        ("case-variant query", "DELETE", target.replace("versionId", "VersionId"), b"", ()),
        ("encoded query name", "DELETE", target.replace("versionId", "version%49d"), b"", ()),
        ("presigned query", "DELETE", target + "&X-Amz-Algorithm=AWS4-HMAC-SHA256", b"", ()),
        ("encoded slash", "DELETE", target.replace("/media/", "/media%2F"), b"", ()),
        ("lowercase encoded slash", "DELETE", target.replace("/media/", "/media%2f"), b"", ()),
        ("doubled slash", "DELETE", target.replace("/media/", "/media//"), b"", ()),
        ("dot segment", "DELETE", target.replace("/media/", "/media/../media/"), b"", ()),
        ("escaped path character", "DELETE", target.replace("/media/", "/%6Dedia/"), b"", ()),
        ("trailing slash", "DELETE", target.replace("?", "/?"), b"", ()),
        ("absolute-form URI", "DELETE", "http://fixtures.invalid" + target, b"", ()),
        ("method override", "HEAD", target, b"", (("X-HTTP-Method-Override", "DELETE"),)),
        ("empty method override", "HEAD", target, b"", (("X-Method-Override", ""),)),
        ("alternate method override", "HEAD", target, b"", (("X-HTTP-Method", "DELETE"),)),
        ("forwarded prefix", "DELETE", target, b"", (("X-Forwarded-Prefix", "/other"),)),
        ("hop-by-hop signed header", "DELETE", target, b"", (("Connection", "Authorization"),)),
        ("origin-form OPTIONS", "OPTIONS", "/", b"", ()),
    ):
        require_denial(boundary.request(method, path, payload, headers=headers), label)
    for method in ("HEAD", "DELETE"):
        require_denial(boundary.request(method, target, b"x", headers=(("Content-Length", "1"),)), method + " body")
        require_denial(boundary.request(method, target, b"0\r\n\r\n",
                       headers=(("Transfer-Encoding", "chunked"),)), method + " chunked")
        require_denial(boundary.request(method, target, protocol="HTTP/1.0"), method + " HTTP/1.0")
    fields = signed("DELETE", target, "fixtures.invalid", boundary.credential)
    require_denial(exchange(boundary.port, "DELETE", target, fields[:-1]), "missing authorization")
    require_denial(exchange(boundary.port, "DELETE", target, fields + fields[-1:]), "duplicate authorization")
    access, secret = boundary.credential
    require_denial(boundary.request("DELETE", target, identity=(access + "X", secret)), "wrong access key")


def renderer_contract(access):
    require(render(access, [KEY, DIGEST]) == render(access, [DIGEST, KEY]), "unstable render")
    for bad_access, keys in (
        ("bad/id", [KEY]), ("", [KEY]), ("x" * 129, [KEY]), ("\n", [KEY]),
        (access, []), (access, [KEY, KEY]), (access, [KEY, DIGEST, KEY]),
        (access, [KEY + "/"]), (access, [KEY.upper()]), (access, [KEY.replace("4000", "3000")]),
        (access, [DIGEST[:-1] + "1"]), (access, [KEY.replace("/media/", "//") + "\n"]),
    ):
        try:
            render(bad_access, keys)
        except ValueError:
            continue
        raise RuntimeError("renderer accepted invalid input")


def fixed_length_response_contract():
    prefix = b"HTTP/1.1 200 OK\r\nContent-Length: "
    payload = b"<Inventory/>"
    require(response(prefix + b"12\r\n\r\n" + payload, decode_body=True)[2] == payload,
            "valid fixed-length response was changed")
    for length in (b"13", b"11", b"bad", b"12\r\nContent-Length: 12", b"12\r\nTransfer-Encoding: chunked"):
        try:
            response(prefix + length + b"\r\n\r\n" + payload, decode_body=True)
        except RuntimeError:
            continue
        raise RuntimeError("ambiguous fixed-length response was accepted")
    require(response(prefix + b"12\r\n\r\n")[2] == b"", "bodyless HEAD framing was changed")


def recording_contract(stack, boundary):
    renderer_contract(boundary.credential[0])
    fixed_length_response_contract()
    expected = []
    for method, path, version, status, body in (
        ("HEAD", PATH, "r%2Bv%26x%3D1", 200, b""),
        ("HEAD", PATH, "v" * 1024, 200, b""),
        ("HEAD", f"/{BUCKET}/{DIGEST}", "backend-gone", 404, b""),
        ("DELETE", PATH, "backend-gone", 404, b""),
        ("DELETE", PATH, "backend-forbidden", 403, b"backend forbidden"),
        ("DELETE", PATH, "backend-unavailable", 503, b"backend unavailable"),
        ("DELETE", PATH, "version-one", 204, b""),
    ):
        target = f"{path}?versionId={version}"
        headers = signed(method, target, "fixtures.invalid:443", boundary.credential)
        fields = headers + ([("Content-Length", "0")] if status == 204 else [])
        reply_status, reply_headers, reply_body = exchange(boundary.port, method, target, fields)
        require((reply_status, reply_body) == (status, body), "backend status or body changed")
        require(reply_headers.get("cache-control") == ["no-store"], "forwarded cache policy")
        require("x-nntune-boundary" not in reply_headers, "forwarded response carried deny marker")
        expected.append((method, target, dict(headers)))

    denied_matrix(boundary, "version-one")
    # Go's general OPTIONS and malformed request handling happen before the
    # Caddy route. The upstream log below must also prove they never proxy.
    status, _, body = exchange(boundary.port, "OPTIONS", "*", [("Host", "fixtures.invalid")])
    require((status, body) == (200, b""), "unexpected server-wide OPTIONS reply")
    fields = signed("DELETE", PATH + "?versionId=one", "fixtures.invalid", boundary.credential)
    rejected = exchange(boundary.port, "DELETE", PATH + "?versionId=one",
                        fields + [("Content-Length", "2"), ("Content-Length", "0")])
    require(rejected[0] == 400, "ambiguous HTTP framing reached the router")
    logs = stack.run("logs", "--no-log-prefix", "backend").splitlines()
    requests = [entry["request"] for line in logs if line.startswith("{")
                if (entry := json.loads(line)).get("logger", "").startswith("http.log.access.")]
    require(len(requests) == len(expected), "unexpected recording-upstream request count")
    for request, (method, target, headers) in zip(requests, expected):
        require((request["method"], request["uri"]) == (method, target), "request target changed")
        require(request["host"] == headers.pop("host"), "signed Host changed")
        actual = {name.lower(): values for name, values in request["headers"].items()}
        authorization = headers.pop("authorization")
        require(actual.get("authorization") == [sha256(authorization.encode()).hexdigest()[:8]],
                "upstream authorization changed or was logged without hashing")
        require(all(actual.get(name) == [value] for name, value in headers.items()), "signed header changed")
    print("Recording upstream preserved signed requests and backend replies, with zero denied traffic.", flush=True)


RECORDING_CONFIG = Path(__file__).with_name("recording.Caddyfile")
