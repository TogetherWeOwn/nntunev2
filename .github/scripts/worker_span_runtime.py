#!/usr/bin/env python3
"""Build and evaluate the feature-gated span probe in real local workerd."""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "workers/api/tests"))
from span_result import evaluate_positive, evaluate_negative as evaluate_root_only
from process_group import shutdown_process_group
from worker_routing_postgres_smoke import OPENER, find_wrangler

ROOT = Path(__file__).resolve().parents[2]
API = ROOT / "workers/api"
TASKS, REQUESTS = 8, 6


def check(cond, message):
    if not cond:
        raise ValueError(message)


def observations(body, mode):
    check(isinstance(body, dict) and body.keys() == {"mode", "root_after", "child_expected", "observations"}
          and body["mode"] == mode, "bad response envelope")
    value = body["observations"]
    check(isinstance(value, dict) and value.keys() == {"root", "tasks", "child"},
          "bad observations")
    check(isinstance(value["tasks"], list) and len(value["tasks"]) == TASKS,
          "wrong task count")
    check(body["root_after"] == value["root"], "root span not restored after join")
    return value


def evaluate_scoped(bodies):
    ids = []
    for body in bodies:
        value = observations(body, "scoped")
        evaluate_positive(value)
        check(value["child"]["before"] == body["child_expected"], "captured child span mismatch")
        ids.extend(item["before"] for item in [*value["tasks"], value["child"]])
    check(len(set(ids)) == len(ids), "span ids collided across concurrent requests")


def evaluate_negative(body):
    value = observations(body, "unscoped")
    for item in [*value["tasks"], value["child"]]:
        check(isinstance(item, dict) and item.keys() == {"before", "mid", "end", "after"},
              "malformed control observation")
        evaluate_root_only({"root": value["root"], **item})
    try:
        evaluate_positive(value)
    except ValueError:
        return
    raise ValueError("positive evaluator accepted the unscoped control")


def build(env, output):
    subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1"],
                   cwd=API, env=env, stdout=subprocess.DEVNULL, check=True, timeout=300)
    log_path = Path(env["RUNNER_TEMP"]) / "worker-span-runtime.log"
    with log_path.open("w") as log:
        result = subprocess.run(
            ["worker-build", "--release", "--out-dir", str(output), "--", "--locked",
             "--features", "span-runtime-test"],
            cwd=API, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=2400)
    check(result.returncode == 0, f"span-runtime-test worker-build failed; see {log_path}")
    subprocess.run([sys.executable, "-I", str(ROOT / ".github/scripts/worker_tools.py"),
                    "check-build", str(log_path)], cwd=ROOT, env=env, check=True, timeout=120)
    subprocess.run(["git", "diff", "--exit-code", "--", "Cargo.lock"],
                   cwd=API, env=env, check=True, timeout=120)
    check((output / "worker/shim.mjs").is_file(), "worker-build produced no shim.mjs")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def get(port, mode):
    with OPENER.open(f"http://127.0.0.1:{port}/probe?mode={mode}", timeout=60) as response:
        check(response.status == 200, f"{mode}: HTTP {response.status}")
        return json.load(response)


CONFIG = """name = "nntune-span-runtime-test"
main = "bundle/worker/shim.mjs"
compatibility_date = "2026-10-01"
workers_dev = false
preview_urls = false
send_metrics = false

[observability]
enabled = true

[observability.traces]
enabled = true
"""


def run(wrangler):
    env = dict(os.environ, CI="1", WRANGLER_SEND_METRICS="false", NO_COLOR="1")
    state = Path(tempfile.mkdtemp(prefix="worker-span-runtime-"))
    proc = None
    try:
        build(env, state / "bundle")
        config = state / "wrangler.toml"
        config.write_text(CONFIG)
        home = state / "home"
        home.mkdir()
        port = free_port()
        runtime_env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home),
                       "XDG_CONFIG_HOME": str(home), "CI": "1", "NO_COLOR": "1",
                       "WRANGLER_SEND_METRICS": "false"}
        with tempfile.TemporaryFile("w+") as log:
            proc = subprocess.Popen(
                ["node", str(wrangler), "dev", "--config", str(config), "--local",
                 "--persist-to", str(state / "data"), "--ip", "127.0.0.1",
                 "--port", str(port), "--inspector-port", str(free_port())],
                cwd=state, env=runtime_env, stdout=log, stderr=subprocess.STDOUT,
                start_new_session=True)
            try:
                deadline = time.monotonic() + 180
                while True:
                    check(proc.poll() is None, "wrangler/workerd exited early")
                    try:
                        get(port, "scoped")
                        break
                    except ValueError:
                        raise
                    except Exception:
                        check(time.monotonic() < deadline, "workerd did not become ready")
                        time.sleep(1)
                barrier = threading.Barrier(REQUESTS)

                def one(_):
                    barrier.wait(timeout=30)
                    return get(port, "scoped")

                with ThreadPoolExecutor(REQUESTS) as pool:
                    bodies = list(pool.map(one, range(REQUESTS)))
                evaluate_scoped(bodies)
                evaluate_negative(get(port, "unscoped"))
                evaluate_scoped([get(port, "scoped")])
                print(f"worker span runtime: ok (real workerd; requests={REQUESTS}; tasks={TASKS}; scoped/control/post-control passed)")
            except Exception:
                log.seek(0)
                sys.stdout.write(log.read()[-4000:])
                raise
            finally:
                shutdown_process_group(proc)
                proc = None
    finally:
        if proc is None:
            shutil.rmtree(state)


class SelfTest(unittest.TestCase):
    def body(self, scoped):
        def sample(identity):
            return {"before": identity, "mid": identity, "end": identity, "after": "root"}
        return {"mode": "scoped" if scoped else "unscoped", "root_after": "root",
                "child_expected": "child" if scoped else None, "observations": {"root": "root",
                                 "tasks": [sample(f"task-{i}" if scoped else "root")
                                           for i in range(TASKS)],
                                 "child": sample("child" if scoped else "root")}}

    def test_negative_control(self):
        evaluate_negative(self.body(False))
        with self.assertRaises(ValueError):
            evaluate_scoped([{**self.body(False), "mode": "scoped"}])
        with self.assertRaises(ValueError):
            evaluate_negative(self.body(True))
        body = self.body(False)
        body["observations"]["tasks"][0] = dict.fromkeys(
            ("root", "before", "mid", "end", "after"), "other")
        with self.assertRaises(ValueError):
            evaluate_negative(body)

    def test_concurrent_collision(self):
        body = self.body(True)
        evaluate_scoped([body])
        with self.assertRaises(ValueError):
            evaluate_scoped([body, body])
        body["child_expected"] = "different-captured-span"
        with self.assertRaisesRegex(ValueError, "captured child span mismatch"):
            evaluate_scoped([body])

    def test_failure_lifecycle(self):
        for stage in ("build", "start", "request", "shutdown"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as tmp:
                api = Path(tmp)
                (api / "build").mkdir()
                original = api / "build/original"
                original.write_text("original")
                state = api / "runtime"
                state.mkdir()
                proc = mock.Mock()
                proc.poll.return_value = None
                with tempfile.TemporaryFile("w+") as log, \
                     mock.patch.dict(globals(), API=api), \
                     mock.patch(__name__ + ".tempfile.mkdtemp", return_value=str(state)), \
                     mock.patch(__name__ + ".tempfile.TemporaryFile", return_value=log), \
                     mock.patch(__name__ + ".build", side_effect=ValueError(stage) if stage == "build" else None), \
                     mock.patch(__name__ + ".subprocess.Popen", side_effect=ValueError(stage) if stage == "start" else None, return_value=proc), \
                     mock.patch(__name__ + ".get", side_effect=ValueError(stage)), \
                     mock.patch(__name__ + ".shutdown_process_group", side_effect=ValueError(stage) if stage == "shutdown" else None) as shutdown:
                    with self.assertRaises(ValueError):
                        run(Path("wrangler"))
                    self.assertEqual(shutdown.call_count, int(stage in ("request", "shutdown")))
                    self.assertEqual(log.closed, stage != "build")
                self.assertEqual(original.read_text(), "original")
                self.assertEqual(state.exists(), stage == "shutdown")
                if stage == "shutdown":
                    self.assertTrue((state / "wrangler.toml").is_file())
                    self.assertTrue((state / "home").is_dir())


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        unittest.main(argv=[sys.argv[0]])
    else:
        if len(sys.argv) > 2:
            raise SystemExit("usage: worker_span_runtime.py [WRANGLER] | --self-test")
        run(find_wrangler(sys.argv[1] if len(sys.argv) == 2 else None))
