#!/usr/bin/env python3
"""Real-workerd regression: concurrent Worker 0.9 Rust span identity.

Builds workers/api with the off-by-default `span-runtime-test` feature (test
entry in src/span_runtime_probe.rs, exercising worker::observability
enter_span_async / active_span / in_current_span; locked, existing
workers/api/Cargo.lock), serves it with the pinned Wrangler `dev --local`
(real workerd, loopback, no Cloudflare API, no secrets) and asserts, over
concurrent overlapping requests:

* direct per-task identity: before/mid/end span ids of a task are all present
  and equal;
* in_current_span: a future created inside a live span reports its id when
  awaited outside that scope;
* cross-task inequality: every task id differs, across all requests;
* root restoration: outside the task span the identity is the root's again,
  and the root is unchanged after the join;
* negative control: the same evaluator MUST reject the `unscoped` probe,
  which omits enter_span_async (no withActiveSpan propagation).

Any build/start/timeout/shape failure exits non-zero. The existing
workers/api/build is moved aside and restored. Usage:

    python3 -I .github/scripts/worker_span_runtime.py --wrangler PATH
    python3 -I .github/scripts/worker_span_runtime.py --self-test
"""

import argparse
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
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
API = ROOT / "workers/api"
TASKS, REQUESTS = 8, 6
BUILD = ["worker-build", "--release", "--", "--locked", "--features", "span-runtime-test"]


class Failure(Exception):
    pass


def check(cond, message):
    if not cond:
        raise Failure(message)


def evaluate(body, mode):
    """Pure check of one probe response; raises Failure."""
    check(isinstance(body, dict) and body.get("mode") == mode, "bad mode")
    tasks = body.get("tasks")
    check(isinstance(tasks, list) and len(tasks) == TASKS, "wrong task count")
    root = body.get("root_before")
    check(body.get("root_after") == root, "root span not restored after join")
    ids = []
    for t in tasks:
        own = t.get("mid")
        check(isinstance(own, str) and own and own != root, "task has no distinct active span")
        for key in ("before", "end"):
            check(t.get(key) == own, f"task identity lost at {key}")
        expected = t.get("child_expected")
        check(isinstance(expected, str) and expected and expected != root, "child has no distinct span")
        check(t.get("child") == expected, "child identity lost outside its scope")
        check(t.get("after_scope") == root and t.get("after_child") == root,
              "root not restored after task scope")
        ids.append(own)
    check(len(set(ids)) == len(ids), "task span ids not distinct")
    return ids


def evaluate_scoped(bodies):
    ids = []
    for body in bodies:
        ids += evaluate(body, "scoped")
    check(len(set(ids)) == len(ids), "span ids collided across concurrent requests")


def evaluate_negative(body):
    check(isinstance(body, dict) and body.get("mode") == "unscoped", "bad control mode")
    root = body.get("root_before")
    check(isinstance(root, str) and root, "control has no root span")
    check(body.get("root_after") == root, "control root not restored")
    tasks = body.get("tasks")
    check(isinstance(tasks, list) and len(tasks) == TASKS, "wrong control task count")
    for task in tasks:
        check(isinstance(task, dict), "malformed control task")
        for key in ("before", "mid", "end", "child", "after_scope", "after_child"):
            check(task.get(key) == root, f"control did not retain root at {key}")


def build(env):
    subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1"],
                   cwd=API, env=env, stdout=subprocess.DEVNULL, check=True, timeout=300)
    log_path = Path(env["RUNNER_TEMP"]) / "worker-span-runtime.log"
    with log_path.open("w") as log:
        r = subprocess.run(BUILD, cwd=API, env=env, stdout=log,
                           stderr=subprocess.STDOUT, timeout=2400)
    check(r.returncode == 0, f"span-runtime-test worker-build failed; see {log_path}")
    subprocess.run([sys.executable, "-I", str(ROOT / ".github/scripts/worker_tools.py"),
                    "check-build", str(log_path)], cwd=ROOT, env=env, check=True, timeout=120)
    subprocess.run(["git", "diff", "--exit-code", "--", "Cargo.lock"],
                   cwd=API, env=env, check=True, timeout=120)
    check((API / "build/worker/shim.mjs").is_file(), "worker-build produced no shim.mjs")


def restore(existing, aside):
    shutil.rmtree(existing, ignore_errors=True)
    if aside.exists():
        aside.rename(existing)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(port, mode):
    url = f"http://127.0.0.1:{port}/probe?mode={mode}"
    with urllib.request.urlopen(url, timeout=60) as resp:
        check(resp.status == 200, f"{mode}: HTTP {resp.status}")
        return json.load(resp)


CONFIG = """name = "nntune-span-runtime-test"
main = "build/worker/shim.mjs"
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
    existing = API / "build"
    aside = API / "build.span-runtime-aside"
    check(not aside.exists(), "stale build aside directory")
    config = API / "wrangler.span-runtime.toml"
    check(not config.exists(), "span runtime config already exists")
    if existing.exists():
        existing.rename(aside)
    proc = None
    try:
        build(env)
        config.write_text(CONFIG)
        port = free_port()
        log = tempfile.TemporaryFile("w+")
        proc = subprocess.Popen(
            ["node", str(wrangler), "dev", "--config", str(config), "--local",
             "--ip", "127.0.0.1", "--port", str(port), "--inspector-port", str(free_port())],
            cwd=API, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.time() + 180
            while True:
                check(proc.poll() is None, "wrangler/workerd exited early")
                try:
                    get(port, "scoped")
                    break
                except Failure:
                    raise
                except Exception:
                    check(time.time() < deadline, "workerd did not become ready")
                    time.sleep(1)
            barrier = threading.Barrier(REQUESTS)

            def one(_):
                barrier.wait(timeout=30)
                return get(port, "scoped")

            with ThreadPoolExecutor(REQUESTS) as pool:
                bodies = list(pool.map(one, range(REQUESTS)))
            evaluate_scoped(bodies)
            evaluate_negative(get(port, "unscoped"))
            evaluate_scoped([get(port, "scoped")])  # still clean after the control
            print("worker span runtime: ok (real workerd)")
        except Failure:
            log.seek(0)
            sys.stdout.write(log.read()[-4000:])
            raise
        finally:
            proc.terminate()
            try:
                proc.wait(20)
            except subprocess.TimeoutExpired:
                proc.kill()
    finally:
        config.unlink(missing_ok=True)
        restore(existing, aside)


class SelfTest(unittest.TestCase):
    def body(self, mode="scoped", ids=None, **over):
        ids = ids or [f"s{i}" for i in range(TASKS)]
        tasks = [{"before": i, "mid": i, "end": i, "child": i,
                  "child_expected": i, "after_scope": "root", "after_child": "root"} for i in ids]
        body = {"mode": mode, "root_before": "root", "root_after": "root", "tasks": tasks}
        body.update(over)
        return body

    def fails(self, body, mode="scoped"):
        with self.assertRaises(Failure):
            evaluate(body, mode)

    def test_good_passes(self):
        evaluate_scoped([self.body()])

    def test_missing_propagation_fails(self):
        self.fails(self.body(ids=["root"] * TASKS))  # no distinct span: unscoped shape

    def test_cross_task_equality_fails(self):
        self.fails(self.body(ids=["s"] * TASKS))

    def test_lost_child_identity_fails(self):
        b = self.body()
        b["tasks"][3]["child"] = "other"
        self.fails(b)

    def test_midflight_drift_fails(self):
        b = self.body()
        b["tasks"][0]["end"] = "s1"
        self.fails(b)

    def test_unrestored_root_fails(self):
        self.fails(self.body(root_after="leaked"))
        b = self.body()
        b["tasks"][2]["after_scope"] = "s2"
        self.fails(b)

    def test_cross_request_collision_fails(self):
        with self.assertRaises(Failure):
            evaluate_scoped([self.body(), self.body()])

    def test_negative_control_must_be_rejected(self):
        evaluate_negative(self.body(mode="unscoped", ids=["root"] * TASKS))
        with self.assertRaises(Failure):
            evaluate_negative(self.body(mode="unscoped"))
        with self.assertRaises(Failure):
            evaluate_negative({})
        with self.assertRaises(Failure):
            evaluate_negative(self.body(mode="unscoped", tasks=[]))


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument("--wrangler")
    p.add_argument("--self-test", action="store_true")
    a = p.parse_args(argv)
    if a.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
        return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1
    if not a.wrangler:
        p.error("--wrangler is required")
    try:
        run(Path(a.wrangler).resolve(strict=True))
    except Failure as e:
        print(f"worker span runtime: FAIL: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
