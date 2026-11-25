#!/usr/bin/env python3
"""Qualify an isolated fixture-only stack on the local Docker engine."""

import argparse
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
import traceback

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))

from contract import Boundary, DIGEST, KEY, RECORDING_CONFIG, recording_contract, require
from render import render
from wire import connect, exchange


DOCKER = ["docker", "--host", "unix:///var/run/docker.sock"]


def run(command, env, *, data=None, check=True, timeout=90):
    try:
        result = subprocess.run(command, input=data, capture_output=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"{Path(command[0]).name} command exceeded its deadline") from None
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(command[:2])} command failed with exit {result.returncode}")
    return result


def credential(prefix):
    return prefix + secrets.token_hex(8), secrets.token_hex(20)


def verify_timeout_redaction(env):
    marker = "synthetic-private-argument"
    try:
        run([sys.executable, "-c", "import time; time.sleep(1)", marker], env, timeout=0.05)
    except RuntimeError:
        trace = traceback.format_exc()
        require("command exceeded its deadline" in trace and marker not in trace,
                "subprocess timeout exposes arguments")
        return
    raise RuntimeError("timeout redaction control did not time out")


def wait_for(operation, label):
    for _ in range(80):
        try:
            result = operation()
            if result:
                return result
        except OSError:
            pass
        time.sleep(0.25)
    raise RuntimeError(label)


class Stack:
    def __init__(self, scratch, recording):
        self.project = "nntune-fixture-" + secrets.token_hex(8)
        self.scratch = scratch
        self.admin = credential("Admin")
        self.scoped = credential("Fixture")
        admin_env = scratch / "admin.env"
        admin_env.touch(mode=0o600)
        admin_env.write_text(f"AWS_ACCESS_KEY_ID={self.admin[0]}\nAWS_SECRET_ACCESS_KEY={self.admin[1]}\n")
        self.config = scratch / "Caddyfile"
        self.config.write_text(render(self.scoped[0], [KEY, DIGEST] if recording else [KEY]))
        self.config.chmod(0o644)
        self.env = {"PATH": os.environ["PATH"], "HOME": str(scratch)}
        self.compose_env = self.env | {
            # Compose interpolates the base bind path before either override replaces it.
            "FIXTURE_ADMIN_ENV": str(admin_env), "FIXTURE_DATA_DIR": str(scratch / "unused-data"),
            "FIXTURE_CADDYFILE": str(self.config), "FIXTURE_BOUNDARY_PORT": "0",
        }
        boundary_image = next(line.split()[1] for line in (HERE / "compose.yml").read_text().splitlines()
                              if line.lstrip().startswith("image: caddy@"))
        override = scratch / "qualification.yml"
        if recording:
            override.write_text(f"""
services:
  backend:
    image: {boundary_image}
    env_file: !reset []
    command: [caddy, run, --config, /etc/caddy/Caddyfile]
    healthcheck:
      test: [CMD, nc, -z, -w, "1", 127.0.0.1, "8333"]
      interval: 250ms
      timeout: 2s
      retries: 20
    volumes: !override
      - "{RECORDING_CONFIG}:/etc/caddy/Caddyfile:ro"
""")
        else:
            audit = scratch / "audit.json"
            audit.write_text(json.dumps({"fluent_host": "127.0.0.1", "fluent_port": 24224, "marshal_as_json": True}))
            audit.chmod(0o644)
            override.write_text(f"""
services:
  backend:
    entrypoint:
      - /bin/sh
      - -ec
      - |
        nc -l -s 127.0.0.1 -p 24224 > /tmp/nntune-audit &
        exec /entrypoint.sh "$$@" -s3.auditLogConfig=/etc/seaweedfs/audit.json
      - fixture-audit
    volumes: !override
      - fixture-data:/data
      - "{audit}:/etc/seaweedfs/audit.json:ro"
volumes:
  fixture-data:
""")
        self.cmd = [*DOCKER, "compose", "--env-file", "/dev/null", "--project-directory", str(scratch),
                    "-p", self.project, "-f", str(HERE / "compose.yml"), "-f", str(override)]
        self.port = None

    def run(self, *args, data=None, check=True, timeout=90):
        result = run([*self.cmd, *args], self.compose_env, data=data, check=check, timeout=timeout)
        return result.stdout.decode()

    def backend_wire(self, request):
        # Save the exec streams before nc gives its connected socket to sh.
        # Keep the socket writable until SeaweedFS closes the response.
        relay = (
            "exec 8<&0 9>&1; "
            "exec timeout 8 nc 127.0.0.1 8333 -e sh -ec 'cat <&8; exec cat >&9'"
        )
        args = [*self.cmd, "exec", "-T", "backend", "sh", "-ec", relay]
        return run(args, self.compose_env, data=request, timeout=12).stdout

    def start(self):
        self.run("up", "-d", "--wait", "--wait-timeout", "25", timeout=180)
        address = self.run("port", "boundary", "8080").strip()
        require(address.startswith("127.0.0.1:"), "boundary is not bound to loopback")
        self.port = int(address.rsplit(":", 1)[1])
        wait_for(lambda: exchange(self.port, "GET", "/", [("Host", "fixtures.invalid")])[0] == 403,
                 "boundary did not start")
        self.verify_isolation()
        return Boundary(self.port, self.scoped)

    def verify_isolation(self):
        names = {service: self.run("ps", "-q", service).strip() for service in ("backend", "boundary")}
        backend, boundary = json.loads(run([*DOCKER, "inspect", *names.values()], self.env).stdout)
        network = self.project + "_storage"
        require(set(backend["NetworkSettings"]["Networks"]) == {network}, "backend joined another network")
        require(not backend["HostConfig"]["PortBindings"], "backend published a port")
        require(set(boundary["NetworkSettings"]["Networks"]) == {network, self.project + "_ingress"},
                "boundary joined another network")
        bindings = boundary["HostConfig"]["PortBindings"]
        require(set(bindings) == {"8080/tcp"} and len(bindings["8080/tcp"]) == 1
                and bindings["8080/tcp"][0]["HostIp"] == "127.0.0.1",
                "boundary published a non-loopback port")
        storage, = json.loads(run([*DOCKER, "network", "inspect", network], self.env).stdout)
        require(storage["Internal"] and not storage["EnableIPv6"] and storage["Options"].get(
            "com.docker.network.bridge.gateway_mode_ipv4") == "isolated", "backend network is not isolated")
        require(set(storage["Containers"]) == {item["Id"] for item in (backend, boundary)},
                "unexpected container on backend network")
        backend_ip = backend["NetworkSettings"]["Networks"][network]["IPAddress"]
        require(bool(backend_ip), "backend network had no address")
        for port in (8333, 8888, 9333, 9340):
            try:
                with socket.create_connection((backend_ip, port), timeout=0.3):
                    raise RuntimeError(f"backend port {port} is reachable from the host")
            except OSError:
                pass

    def close(self):
        self.run("down", "--volumes", "--remove-orphans")
        for kind in ("container", "network", "volume"):
            states = ["--all"] if kind == "container" else []
            args = ["ls", "-q", *states, "--filter", f"label=com.docker.compose.project={self.project}"]
            require(not run([*DOCKER, kind, *args], self.env).stdout.strip(), f"disposable {kind} remains")
        if self.port is not None:
            try:
                with connect(self.port):
                    raise RuntimeError("disposable loopback port is still open")
            except ConnectionRefusedError:
                pass


def qualify(recording, bundle):
    with tempfile.TemporaryDirectory(prefix="nntune-fixture-") as directory:
        stack = Stack(Path(directory), recording)
        try:
            if recording:
                verify_timeout_redaction(stack.env)
                recording_contract(stack, stack.start())
            else:
                from storage import Storage
                stack.run("up", "-d", "backend", timeout=180)
                storage = Storage(stack)
                stack.scoped = storage.provision()
                stack.config.write_text(render(stack.scoped[0], [KEY]))
                storage.qualify(stack.start(), bundle)
        finally:
            stack.close()
    require(not Path(directory).exists(), "disposable scratch data remains")
    print("Verified disposable containers, networks, volumes, ports and scratch are gone.", flush=True)


def terminate(signum, _frame):
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--contract", action="store_true", help="run the recording-upstream contract only")
    mode.add_argument("--storage", action="store_true", help="run the focused disposable storage check")
    mode.add_argument("--bundle", type=Path, help="run both lanes and the reviewed launcher with this WASM JS")
    settings = parser.parse_args()
    if settings.bundle and not settings.bundle.is_file():
        parser.error("a built provider preflight bundle is required")
    if settings.contract or settings.bundle:
        qualify(True, None)
    if settings.storage or settings.bundle:
        qualify(False, settings.bundle.resolve() if settings.bundle else None)
