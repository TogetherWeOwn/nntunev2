"""Generate a dedicated per-instance CA and one PostgreSQL server certificate."""

import argparse
import os
from pathlib import Path
import secrets
import subprocess
import sys
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / ".github/scripts"))
from staging_trial_isolated import FIXTURE_HOSTS, HOSTS


def create_tls(output, *, fixture=False):
    # Reserve a fresh directory. Failed generation leaves no key material in
    # it, and the CA is kept apart from the server mount on success.
    output.mkdir(mode=0o700)
    with TemporaryDirectory(dir=output) as staging:
        scratch = Path(staging)
        generate_tls(scratch, fixture=fixture)
        for name in ("ca", "server"):
            (scratch / name).rename(output / name)


def generate_tls(output, *, fixture):
    previous = os.umask(0o077)
    try:
        hosts = FIXTURE_HOSTS if fixture else HOSTS
        names = ",".join(f"DNS:{host}" for host in hosts.values())
        ca = output / "ca"
        server = output / "server"
        ca.mkdir()
        server.mkdir()
        extensions = output / "extensions.cnf"
        extensions.write_text(f"basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\n"
                              f"extendedKeyUsage=serverAuth\nsubjectAltName={names}\n", encoding="ascii")
        env = {"PATH": os.defpath, "OPENSSL_CONF": "/dev/null"}

        def openssl(*args):
            subprocess.run(["openssl", *args], env=env, cwd=output, check=True,
                           capture_output=True, timeout=20)

        openssl("req", "-new", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
                "-nodes", "-days", "30", "-subj", "/CN=NNTune dedicated trial CA",
                "-addext", "basicConstraints=critical,CA:TRUE,pathlen:0",
                "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-keyout", "ca/ca.key", "-out", "ca/ca.crt")
        openssl("req", "-new", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
                "-subj", f"/CN={hosts['direct']}", "-keyout", "server/server.key", "-out", "server/request.csr")
        openssl("x509", "-req", "-in", "server/request.csr", "-CA", "ca/ca.crt", "-CAkey", "ca/ca.key",
                "-set_serial", f"0x{secrets.token_hex(16)}", "-days", "8", "-extfile", "extensions.cnf",
                "-out", "server/server.crt")
        (server / "request.csr").unlink()
        extensions.unlink()
    finally:
        os.umask(previous)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new directory owned by the invoking user")
    parser.add_argument("--fixture", action="store_true", help="use the disposable .invalid names")
    args = parser.parse_args()
    create_tls(args.output, fixture=args.fixture)
