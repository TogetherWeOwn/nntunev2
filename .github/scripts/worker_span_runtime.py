#!/usr/bin/env python3
"""Evaluate Worker span probe observations with offline self-tests."""

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
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest import mock

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
    check(isinstance(body, dict) and body.keys() == {"mode", "root_after", "child_expected", "overlapping", "observations"}
          and body["mode"] == mode and type(body["overlapping"]) is bool, "bad response envelope")
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
        ids.append(value["root"])
        ids.extend(item["before"] for item in [*value["tasks"], value["child"]])
    check(len(set(ids)) == len(ids), "span ids collided across concurrent requests")


def evaluate_burst(bodies):
    evaluate_scoped(bodies)
    check(any(body["overlapping"] for body in bodies), "requests did not overlap")


def evaluate_negative(body):
    value = observations(body, "unscoped")
    check(body["child_expected"] is None, "unscoped control reported a captured child span")
    for item in [*value["tasks"], value["child"]]:
        check(isinstance(item, dict) and item.keys() == {"before", "mid", "end", "after"},
              "malformed control observation")
        evaluate_root_only({"root": value["root"], **item})
    try:
        evaluate_positive(value)
    except ValueError:
        return
    raise ValueError("positive evaluator accepted the unscoped control")


class BuildShutdownError(RuntimeError):
    """The build process group could not be verified stopped."""


def build(env, output):
    subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1"],
                   cwd=API, env=env, stdout=subprocess.DEVNULL, check=True, timeout=300)
    log_path = Path(env["RUNNER_TEMP"]) / "worker-span-runtime.log"
    with log_path.open("w") as log:
        proc = subprocess.Popen(
            ["worker-build", "--release", "--out-dir", str(output), "--", "--locked",
             "--features", "span-runtime-test"],
            cwd=API, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            returncode = proc.wait(timeout=2400)
        finally:
            try:
                shutdown_process_group(proc)
            except BaseException as exc:
                raise BuildShutdownError(f"build shutdown unverified; output retained at {output}") from exc
    check(returncode == 0, f"span-runtime-test worker-build failed; see {log_path}")
    subprocess.run([sys.executable, "-I", str(ROOT / ".github/scripts/worker_tools.py"),
                    "check-build", str(log_path)], cwd=ROOT, env=env, check=True, timeout=120)
    subprocess.run(["git", "diff", "--exit-code", "--", "Cargo.lock"],
                   cwd=API, env=env, check=True, timeout=120)
    check((output / "worker/shim.mjs").is_file(), "worker-build produced no shim.mjs")


def free_ports():
    with socket.socket() as first, socket.socket() as second:
        first.bind(("127.0.0.1", 0))
        second.bind(("127.0.0.1", 0))
        ports = first.getsockname()[1], second.getsockname()[1]
        check(ports[0] != ports[1], "ephemeral ports collided")
        return ports


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
    preserve = False
    try:
        try:
            build(env, state / "bundle")
        except BuildShutdownError:
            preserve = True
            raise
        config = state / "wrangler.toml"
        config.write_text(CONFIG)
        home = state / "home"
        home.mkdir()
        port, inspector_port = free_ports()
        runtime_env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home),
                       "XDG_CONFIG_HOME": str(home), "CI": "1", "NO_COLOR": "1",
                       "WRANGLER_SEND_METRICS": "false"}
        with (state / "runtime.log").open("w+") as log:
            proc = subprocess.Popen(
                ["node", str(wrangler), "dev", "--config", str(config), "--local",
                 "--persist-to", str(state / "data"), "--ip", "127.0.0.1",
                 "--port", str(port), "--inspector-port", str(inspector_port)],
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
                evaluate_burst(bodies)
                evaluate_negative(get(port, "unscoped"))
                evaluate_scoped([get(port, "scoped")])
                print(f"worker span runtime: ok (real workerd; requests={REQUESTS}; tasks={TASKS}; scoped/control/post-control passed)")
            except Exception:
                with (state / "runtime.log").open() as diagnostics:
                    sys.stdout.write(diagnostics.read()[-4000:])
                raise
            finally:
                try:
                    shutdown_process_group(proc)
                except Exception:
                    print(f"runtime shutdown unverified; state and log retained at {state}", file=sys.stderr)
                    raise
                proc = None
    finally:
        if proc is None and not preserve:
            shutil.rmtree(state)


class SelfTest(unittest.TestCase):
    def body(self, scoped):
        def sample(identity):
            return {"before": identity, "mid": identity, "end": identity, "after": "root"}
        return {"mode": "scoped" if scoped else "unscoped", "root_after": "root",
                "child_expected": "child" if scoped else None, "overlapping": False, "observations": {"root": "root",
                                 "tasks": [sample(f"task-{i}" if scoped else "root")
                                           for i in range(TASKS)],
                                 "child": sample("child" if scoped else "root")}}

    def test_negative_control(self):
        evaluate_negative(self.body(False))
        with self.assertRaisesRegex(ValueError, "captured child span"):
            evaluate_negative({**self.body(False), "child_expected": "unexpected"})
        with self.assertRaises(ValueError):
            evaluate_scoped([{**self.body(False), "mode": "scoped"}])
        with self.assertRaises(ValueError):
            evaluate_negative(self.body(True))
        body = self.body(False)
        body["observations"]["tasks"][0] = dict.fromkeys(
            ("root", "before", "mid", "end", "after"), "other")
        with self.assertRaisesRegex(ValueError, "malformed control observation"):
            evaluate_negative(body)
        body = self.body(False)
        body["observations"]["tasks"][0]["before"] = "other"
        with self.assertRaisesRegex(ValueError, "negative control did not observe root throughout"):
            evaluate_negative(body)

    def test_concurrent_collision(self):
        body = self.body(True)
        evaluate_scoped([body])
        with self.assertRaisesRegex(ValueError, "requests did not overlap"):
            evaluate_burst([body])
        body["overlapping"] = True
        evaluate_burst([body])
        other = self.body(True)
        for item in [*other["observations"]["tasks"], other["observations"]["child"]]:
            for key in ("before", "mid", "end"):
                item[key] += "-other"
        other["child_expected"] += "-other"
        with self.assertRaisesRegex(ValueError, "collided"):
            evaluate_scoped([body, other])
        body["child_expected"] = "different-captured-span"
        with self.assertRaisesRegex(ValueError, "captured child span mismatch"):
            evaluate_scoped([body])

    def test_failure_lifecycle(self):
        for stage in ("build", "build_shutdown", "start", "request", "shutdown"):
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as tmp:
                api = Path(tmp)
                (api / "build").mkdir()
                original = api / "build/original"
                original.write_text("original")
                state = api / "runtime"
                state.mkdir()
                proc = mock.Mock()
                proc.poll.return_value = None
                error = BuildShutdownError(stage) if stage == "build_shutdown" else ValueError(stage)
                def start(*args, **kwargs):
                    if stage == "start":
                        raise error
                    kwargs["stdout"].write("runtime diagnostic\n")
                    kwargs["stdout"].flush()
                    return proc

                with mock.patch(__name__ + ".tempfile.mkdtemp", return_value=str(state)), \
                     mock.patch(__name__ + ".build", side_effect=error if stage.startswith("build") else None), \
                     mock.patch(__name__ + ".free_ports", return_value=(10001, 10002)), \
                     mock.patch(__name__ + ".subprocess.Popen", side_effect=start) as popen, \
                     mock.patch(__name__ + ".get", side_effect=ValueError(stage) if stage == "request" else None), \
                     mock.patch(__name__ + ".evaluate_burst"), \
                     mock.patch(__name__ + ".evaluate_negative"), \
                     mock.patch(__name__ + ".evaluate_scoped"), \
                     mock.patch(__name__ + ".shutdown_process_group", side_effect=error if stage == "shutdown" else None) as shutdown:
                    with self.assertRaises((ValueError, BuildShutdownError)):
                        run(Path("wrangler"))
                    self.assertEqual(shutdown.call_count, int(stage in ("request", "shutdown")))
                    if stage in ("start", "request", "shutdown"):
                        self.assertTrue(popen.call_args.kwargs["stdout"].closed)
                self.assertEqual(original.read_text(), "original")
                self.assertEqual(state.exists(), stage in ("build_shutdown", "shutdown"))
                if stage == "shutdown":
                    self.assertTrue((state / "wrangler.toml").is_file())
                    self.assertTrue((state / "home").is_dir())
                    self.assertEqual((state / "runtime.log").read_text(), "runtime diagnostic\n")

    def test_build_supervision(self):
        for result in (0, 1, subprocess.TimeoutExpired("worker-build", 2400)):
            for shutdown_fails in (False, True):
                with self.subTest(result=result, shutdown_fails=shutdown_fails), tempfile.TemporaryDirectory() as tmp:
                    output = Path(tmp) / "bundle"
                    proc = mock.Mock()
                    proc.wait.side_effect = result if isinstance(result, Exception) else None
                    proc.wait.return_value = result
                    with mock.patch(__name__ + ".subprocess.run") as command, \
                         mock.patch(__name__ + ".subprocess.Popen", return_value=proc) as popen, \
                         mock.patch(__name__ + ".shutdown_process_group", side_effect=RuntimeError("alive") if shutdown_fails else None) as shutdown:
                        if result == 0 and not shutdown_fails:
                            (output / "worker").mkdir(parents=True)
                            (output / "worker/shim.mjs").touch()
                            build({"RUNNER_TEMP": tmp}, output)
                        else:
                            expected = BuildShutdownError if shutdown_fails else (subprocess.TimeoutExpired if isinstance(result, Exception) else ValueError)
                            with self.assertRaises(expected):
                                build({"RUNNER_TEMP": tmp}, output)
                        self.assertTrue(popen.call_args.kwargs["start_new_session"])
                        proc.wait.assert_called_once_with(timeout=2400)
                        shutdown.assert_called_once_with(proc)
                        self.assertEqual(command.call_count, 3 if result == 0 and not shutdown_fails else 1)

    def test_interrupted_build_shutdown_retains_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp) / "runtime"
            state.mkdir()
            proc = mock.Mock()
            proc.wait.side_effect = subprocess.TimeoutExpired("worker-build", 2400)
            with mock.patch.dict(os.environ, {"RUNNER_TEMP": tmp}), \
                 mock.patch(__name__ + ".tempfile.mkdtemp", return_value=str(state)), \
                 mock.patch(__name__ + ".subprocess.run"), \
                 mock.patch(__name__ + ".subprocess.Popen", return_value=proc), \
                 mock.patch(__name__ + ".shutdown_process_group", side_effect=KeyboardInterrupt):
                with self.assertRaises(BuildShutdownError) as caught:
                    run(Path("wrangler"))
                self.assertIsInstance(caught.exception.__cause__, KeyboardInterrupt)
                self.assertTrue(state.is_dir())

    def test_diagnostic_read_does_not_move_writer(self):
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp) / "runtime"
            state.mkdir()
            proc = mock.Mock()
            proc.poll.return_value = None
            writer = None
            open_path = Path.open

            def open_log(path, *args, **kwargs):
                nonlocal writer
                stream = open_path(path, *args, **kwargs)
                if path == state / "runtime.log":
                    if args and args[0] == "w+":
                        writer = stream
                    else:
                        read = stream.read
                        def interleaved_read(*read_args):
                            writer.write("child append\n")
                            writer.flush()
                            return read(*read_args)
                        stream.read = interleaved_read
                return stream

            def start(*args, **kwargs):
                kwargs["stdout"].write("original prefix\n")
                kwargs["stdout"].flush()
                return proc

            with mock.patch(__name__ + ".tempfile.mkdtemp", return_value=str(state)), \
                 mock.patch(__name__ + ".build"), \
                 mock.patch(__name__ + ".free_ports", return_value=(10001, 10002)), \
                 mock.patch(__name__ + ".subprocess.Popen", side_effect=start), \
                 mock.patch(__name__ + ".get", side_effect=ValueError("request")), \
                 mock.patch(__name__ + ".shutdown_process_group", side_effect=RuntimeError("alive")), \
                 mock.patch.object(Path, "open", open_log):
                with self.assertRaisesRegex(RuntimeError, "alive"):
                    run(Path("wrangler"))
            self.assertEqual((state / "runtime.log").read_text(), "original prefix\nchild append\n")

    def test_free_ports_simultaneous(self):
        active = set()

        class FakeSocket:
            def __enter__(self):
                return self

            def __exit__(self, *_):
                active.remove(self.port)

            def bind(self, address):
                self.port = next(port for port in (1234, 5678) if port not in active)
                active.add(self.port)

            def getsockname(self):
                return ("127.0.0.1", self.port)

        with mock.patch(__name__ + ".socket.socket", side_effect=FakeSocket):
            self.assertEqual(free_ports(), (1234, 5678))
        self.assertFalse(active)
        duplicate = mock.MagicMock()
        duplicate.__enter__.return_value.getsockname.return_value = ("127.0.0.1", 1234)
        with mock.patch(__name__ + ".socket.socket", return_value=duplicate):
            with self.assertRaisesRegex(ValueError, "collided"):
                free_ports()


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        unittest.main(argv=[sys.argv[0]])
    else:
        if len(sys.argv) > 2:
            raise SystemExit("usage: worker_span_runtime.py [WRANGLER] | --self-test")
        run(find_wrangler(sys.argv[1] if len(sys.argv) == 2 else None))
