#!/usr/bin/env python3
"""Run the separately built owned-transport WASM Worker on hosted Actions only."""
import hashlib
import importlib.util
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location("routing", Path(__file__).with_name("worker_routing_postgres_smoke.py"))
routing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(routing)


def main():
    routing.check(os.environ.get("GITHUB_ACTIONS") == "true" and
                  os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "hosted Actions only")
    bundle = Path(os.environ["OWNED_TEST_BUNDLE"]).resolve()
    wasm = bundle / "index_bg.wasm"
    shim = bundle / "worker/shim.mjs"
    routing.check(wasm.is_file() and shim.is_file(), "test Worker bundle missing")
    wrangler = routing.find_wrangler(None)
    wasm_bytes = wasm.read_bytes()
    routing.check(b"owned-drive-ok:4" in wasm_bytes, "test entry marker missing from WASM")
    print(f"test wasm sha256: {hashlib.sha256(wasm_bytes).hexdigest()}", flush=True)
    subprocess.run(["node", str(wrangler), "--version"], check=True, timeout=30)
    workerd = list((routing.ROOT / "apps/web/node_modules/.pnpm").glob("workerd@*/node_modules/workerd/bin/workerd"))
    routing.check(len(workerd) == 1, f"expected one workerd executable, got {len(workerd)}")
    subprocess.run([str(workerd[0]), "--version"], check=True, timeout=30)
    with tempfile.TemporaryDirectory(prefix="owned-worker-smoke-") as directory:
        tmp = Path(directory)
        (tmp / "wrangler.toml").write_text(f'name = "owned-socket-test"\nmain = "{shim}"\ncompatibility_date = "2026-10-01"\n')
        (tmp / "home").mkdir()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp / "home"),
               "WRANGLER_SEND_METRICS": "false", "CI": "1", "NO_COLOR": "1"}
        with (tmp / "wrangler.log").open("w+") as log:
            proc = subprocess.Popen([shutil.which("node"), str(wrangler), "dev", "--config", str(tmp / "wrangler.toml"),
                                     "--local", "--ip", "127.0.0.1", "--port", str(port),
                                     "--persist-to", str(tmp / "state"), "--show-interactive-dev-session=false"],
                                    cwd=tmp, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                start = time.monotonic()
                while True:
                    routing.check(proc.poll() is None, "wrangler exited before test response")
                    routing.check(time.monotonic() - start < 180, "test Worker timed out")
                    try:
                        status, _, body = routing.http("GET", f"http://127.0.0.1:{port}/")
                    except OSError:
                        time.sleep(1)
                        continue
                    routing.check(status == 200 and body == "owned-drive-ok:4", f"unexpected test response: {status} {body!r}")
                    print("PASS: total precedence, phase precedence, co-poll/drop, Worker timer (4 cases)", flush=True)
                    print(f"startup and execution elapsed: {time.monotonic() - start:.3f}s", flush=True)
                    break
            finally:
                routing.process_group.shutdown_process_group(proc)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (routing.Failure, OSError, subprocess.SubprocessError, KeyError) as err:
        print(f"FAIL: {err}", file=sys.stderr)
        sys.exit(1)
