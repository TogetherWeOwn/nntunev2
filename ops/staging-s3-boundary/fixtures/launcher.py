"""Use the reviewed HTTPS launcher with a disposable loopback TLS connection."""

import json
from pathlib import Path
import socketserver
import ssl
import threading
from time import monotonic

from contract import GENERATION, require
from qualify import run
from render import BUCKET
from wire import REGION, connect, read_to_close


LAUNCHER = Path(__file__).resolve().parents[3] / ".github/scripts/private_trial_provider_preflight.cjs"
QUALIFIED = (
    b"Private staging fixture provider preflight: qualified. Matched present HEAD and preserved sibling HEAD. "
    b"Twice observed empty DELETE 204 and HEAD 404 with optional version headers absent or matching.\n"
)
HEADER_LIMIT = 16_384


def qualified_launcher(stack, bundle, boundary, selected, sibling):
    cert = stack.scratch / "loopback.crt"
    key = stack.scratch / "loopback.key"
    run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-noenc", "-days", "1",
         "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1",
         "-keyout", str(key), "-out", str(cert)], stack.env)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    tls.set_alpn_protocols(["http/1.1"])

    class Relay(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(8)
            with tls.wrap_socket(self.request, server_side=True) as client:
                deadline = monotonic() + 8
                headers = bytearray()
                while len(headers) < HEADER_LIMIT:
                    remaining = deadline - monotonic()
                    if remaining <= 0:
                        return
                    client.settimeout(remaining)
                    chunk = client.recv(HEADER_LIMIT - len(headers))
                    if not chunk:
                        return
                    headers.extend(chunk)
                    end = headers.find(b"\r\n\r\n")
                    if end != -1:
                        if end + 4 != len(headers):
                            return
                        with connect(boundary.port) as upstream:
                            upstream.sendall(headers)
                            client.sendall(read_to_close(upstream))
                        return

    with socketserver.TCPServer(("127.0.0.1", 0), Relay) as relay:
        worker = threading.Thread(target=relay.serve_forever, kwargs={"poll_interval": 0.1})
        worker.start()
        try:
            endpoint = f"https://127.0.0.1:{relay.server_address[1]}"
            env = stack.env | {
                "NODE_EXTRA_CA_CERTS": str(cert),
                "PRIVATE_PLAYBACK_STAGING_S3_ENDPOINT": endpoint,
                "PRIVATE_PLAYBACK_STAGING_S3_REGION": REGION,
                "PRIVATE_TRIAL_PREFLIGHT_SCOPE": json.dumps({
                    "bucket": BUCKET, "endpoint": endpoint, "region": REGION, "generation": GENERATION,
                    "version": selected, "sibling_version": sibling,
                }),
                "PRIVATE_TRIAL_PREFLIGHT_S3": json.dumps(dict(zip(("access_key", "secret_key"), boundary.credential))),
            }
            result = run(["node", str(LAUNCHER), "--provider", str(bundle)], env, check=False, timeout=30)
            require(result.returncode == 0 and result.stdout == QUALIFIED, "reviewed launcher did not qualify")
            print(QUALIFIED.decode("ascii").rstrip(), flush=True)
        finally:
            relay.shutdown()
            worker.join()
