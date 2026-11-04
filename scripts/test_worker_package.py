#!/usr/bin/env python3
"""Offline regressions for the Worker packaging path.

Only fake executables run here: no real Cargo, worker-build, network or host
pool. Synchronisation uses event files, not delays.
"""

import contextlib
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import cargo_cache as cache
import worker_package


SCRIPT = Path(cache.__file__).resolve()

VERSIONS = {'worker-build': '0.8.7', 'wasm-bindgen': 'wasm-bindgen 0.2.129',
            'wasm-opt': 'wasm-opt version 132', 'esbuild': '0.28.2'}

# One fake body installed under every tool name (and a fake `cargo`). The
# worker-build "hold" modes emulate: Cargo runs and exits, packaging goes on.
FAKE_TOOL = '''
import json, os, pathlib, subprocess, sys, time
name = pathlib.Path(sys.argv[0]).name
ctl = pathlib.Path(os.environ["NN_FAKE_CTL"])
VERSIONS = %(versions)r
if sys.argv[1:] == ["--version"]:
    override = ctl / ("version-" + name)
    print(override.read_text() if override.exists() else VERSIONS[name])
    sys.exit(0)
def event(text):
    with open(ctl / "events", "a") as log:
        log.write(text + "\\n")
if name == "rustc":
    (ctl / "rustc-env").write_text(os.environ.get("RUSTUP_AUTO_INSTALL", "unset"))
    print((ctl / "libdir").read_text())
    sys.exit(0)
if name == "cargo":
    if sys.argv[1:2] == ["metadata"]:
        event("metadata")
        (ctl / "metadata.json").write_text(json.dumps(
            {"argv": sys.argv, "target": os.environ.get("CARGO_TARGET_DIR"),
             "tmpdir": os.environ.get("TMPDIR")}))
        if (ctl / "metadata-fail").exists():
            print("error: lock needs updating", file=sys.stderr)
            sys.exit(101)
        print("{}")
        sys.exit(0)
    (ctl / "cargo-ran").write_text(os.environ["CARGO_TARGET_DIR"])
    sys.exit(0)
event("builder")
(pathlib.Path(sys.argv[1]) / "Cargo.lock").write_text("REWRITTEN")
mode = (ctl / "mode").read_text()
tag = pathlib.Path.cwd().name
out = pathlib.Path(sys.argv[sys.argv.index("--out-dir") + 1])
keys = ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "TMPDIR", "CARGO_INCREMENTAL",
        "CARGO_NET_OFFLINE", "RUSTUP_AUTO_INSTALL", "WASM_BINDGEN_BIN", "WASM_OPT_BIN", "ESBUILD_BIN",
        "CUSTOM_SHIM", "COREDUMP"]
(ctl / ("record-" + tag + ".json")).write_text(json.dumps(
    {"argv": sys.argv, "env": {k: os.environ.get(k) for k in keys}, "pid": os.getpid()}))
def good():
    (out / "worker").mkdir(parents=True)
    (out / "worker" / "shim.mjs").write_text("export {}")
    (out / "worker" / "index.wasm").write_bytes(b"\\0asm")
if mode == "ok":
    good()
elif mode == "fail":
    (out / "partial").mkdir(parents=True)
    (out / "partial" / "half").write_text("x")
    sys.exit(3)
elif mode == "no-entry":
    (out / "worker").mkdir(parents=True)
    (out / "worker" / "other.js").write_text("x")
elif mode == "symlink":
    good()
    os.symlink("/etc/hostname", out / "worker" / "link")
elif mode == "root-symlink":
    source = ctl / "shared-package"
    (source / "worker").mkdir(parents=True)
    (source / "worker" / "shim.mjs").write_text("export {}")
    out.symlink_to(source, target_is_directory=True)
elif mode == "race":
    good()
    race = pathlib.Path((ctl / "race-output").read_text())
    race.mkdir()
    (race / "keep").write_text("precious")
else:
    # hold / grow: run Cargo to completion first, then keep packaging.
    subprocess.run([str(pathlib.Path(sys.argv[0]).with_name("cargo")), "build"], check=True)
    grandchild = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
    (ctl / ("grandchild-" + tag)).write_text(str(grandchild.pid))
    if mode == "grow":
        import random
        scratch = pathlib.Path(os.environ["TMPDIR"])
        (scratch / "big").write_bytes(random.randbytes(400 * 1024))
    (ctl / ("cargo-exited-" + tag)).write_text("1")
    while not (ctl / "release").exists():
        time.sleep(.01)
    grandchild.kill()
    grandchild.wait()
    good()
''' % {'versions': VERSIONS}

DRIVER = '''
import json, sys
from pathlib import Path
import cargo_cache as c, worker_package as w
pool, tools, cargo = Path(sys.argv[1]), json.loads(sys.argv[2]), sys.argv[3]
w.resolve_tools = lambda: tools
c.resolve_cargo = lambda name: cargo
try:
    sys.exit(c.run_worker_build(pool, Path("worker"), Path("out"), interval=.02))
except c.Refusal as error:
    print("REFUSED:", error, file=sys.stderr)
    sys.exit(75)
'''


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def lock_free(slot):
    fd = os.open(slot / 'lock', os.O_RDWR)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except BlockingIOError:
        return False
    finally:
        os.close(fd)


class WorkerPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get('PAPERCLIP_RUN_SCRATCH_DIR'))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.pool = self.root / 'pool'
        self.pool.mkdir()
        self.policy = {'version': 1, 'slots': 2, 'slot_budget_bytes': 256 * 1024,
                       'min_available_bytes': 1, 'hard_limit_bytes': 1024 * 1024,
                       'quota_receipt': 'synthetic fixture; not a real quota receipt',
                       'scratch_coverage_receipt': 'synthetic fixture; not host coverage'}
        (self.pool / 'policy.json').write_text(json.dumps(self.policy))
        for number in range(2):
            slot = self.pool / f'slot-{number}'
            for part in ('target', 'scratch'):
                (slot / part).mkdir(parents=True)
            (slot / 'lock').touch()
        self.ctl = self.root / 'ctl'
        self.ctl.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        for name in (*VERSIONS, 'cargo', 'rustc'):
            tool = self.bin / name
            tool.write_text('#!' + sys.executable + '\n' + FAKE_TOOL)
            tool.chmod(0o700)
        self.tools = [str(self.bin / name) for name in VERSIONS]
        self.set_mode('ok')
        self.libdir = self.root / 'libdir'
        self.libdir.mkdir()
        (self.libdir / 'libstd-0123abcd.rlib').write_bytes(b'rlib')
        (self.ctl / 'libdir').write_text(str(self.libdir))
        self.ws = self.make_workspace('ws')
        # Fixed environment so nothing resolves a real tool or hook.
        env = {'NN_FAKE_CTL': str(self.ctl), 'CUSTOM_SHIM': 'hook', 'COREDUMP': '1',
               'PYTHONPATH': str(SCRIPT.parent),
               'PATH': os.pathsep.join([str(self.bin), os.environ.get('PATH', '')])}
        patcher = patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.pids = []
        self.addCleanup(self.reap)

    def reap(self):
        (self.ctl / 'release').touch()
        for pid in self.pids:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)
        for path in self.ctl.glob('grandchild-*'):
            with contextlib.suppress(ProcessLookupError, ValueError):
                os.kill(int(path.read_text()), signal.SIGKILL)

    def set_mode(self, mode):
        (self.ctl / 'mode').write_text(mode)

    def make_workspace(self, name):
        workspace = self.root / name
        (workspace / 'worker').mkdir(parents=True)
        (workspace / 'worker' / 'Cargo.toml').write_text('[package]\n')
        return workspace

    @contextlib.contextmanager
    def cwd(self, path):
        old = os.getcwd()
        os.chdir(path)
        try:
            yield
        finally:
            os.chdir(old)

    def build(self, crate='worker', output='out', workspace=None):
        """In-process run with fake tools and Cargo path."""
        with self.cwd(workspace or self.ws), \
                patch.object(worker_package, 'resolve_tools', return_value=self.tools), \
                patch.object(cache, 'resolve_cargo', return_value=str(self.bin / 'cargo')):
            return cache.run_worker_build(self.pool, Path(crate), Path(output), interval=.02)

    def assert_nothing_leased(self):
        self.assertEqual(list(self.pool.glob('slot-*/lease.json')), [])
        self.assertEqual(list(self.pool.glob('slot-*/target/*')), [])
        self.assertEqual(list(self.pool.glob('slot-*/scratch/*')), [])
        self.assertFalse((self.ws / 'target').exists())

    def record(self, tag='ws'):
        return json.loads((self.ctl / f'record-{tag}.json').read_text())

    def wait_for(self, path, what, timeout=10):
        deadline = time.monotonic() + timeout
        while not Path(path).exists():
            if time.monotonic() > deadline:
                self.fail(f'timed out waiting for {what}')
            time.sleep(.01)

    def start(self, workspace):
        env = os.environ.copy()
        errors = open(self.root / f'driver-{workspace.name}.err', 'w')
        child = subprocess.Popen(
            [sys.executable, '-c', DRIVER, str(self.pool), json.dumps(self.tools),
             str(self.bin / 'cargo')],
            env=env, cwd=workspace, stdout=subprocess.DEVNULL, stderr=errors)
        errors.close()
        child.errors = self.root / f'driver-{workspace.name}.err'
        self.addCleanup(lambda: child.poll() is None and child.kill())
        return child

    def finish(self, child, timeout=20):
        # Files, not pipes: surviving descendants must not block the reader.
        child.wait(timeout=timeout)
        return child.errors.read_text()

    def lease(self, slot):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                lease = json.loads((slot / 'lease.json').read_text())
            except (OSError, ValueError):
                lease = {}
            if 'cargo_pgid' in lease:
                return lease
            time.sleep(.01)
        self.fail('lease never recorded cargo_pgid')

    # -- path refusal: before any lease or spawn -------------------------

    def refused(self, pattern, **kwargs):
        with patch.object(cache, 'acquire', side_effect=AssertionError('lease taken')), \
                patch.object(subprocess, 'Popen', side_effect=AssertionError('spawned')):
            with self.assertRaisesRegex(cache.Refusal, pattern):
                self.build(**kwargs)
        self.assert_nothing_leased()

    def test_crate_outside_workspace_or_without_manifest_refused(self):
        outside = self.root / 'outside'
        outside.mkdir()
        (outside / 'Cargo.toml').write_text('[package]\n')
        self.refused('inside this workspace', crate='../outside')
        (self.ws / 'empty').mkdir()
        self.refused('inside this workspace', crate='empty')
        (self.ws / 'linked').symlink_to(self.ws / 'worker')
        self.refused('not a real directory', crate='linked')
        (self.ws / 'linked-manifest').mkdir()
        (self.ws / 'linked-manifest' / 'Cargo.toml').symlink_to(outside / 'Cargo.toml')
        self.refused('inside this workspace', crate='linked-manifest')

    def test_output_path_refusals_leave_existing_material_alone(self):
        (self.ws / 'dir-out').mkdir()
        (self.ws / 'dir-out' / 'keep').write_text('precious')
        (self.ws / 'file-out').write_text('precious')
        (self.ws / 'link-out').symlink_to(self.root / 'nowhere')  # broken symlink
        (self.ws / 'real').mkdir()
        (self.ws / 'link-dir').symlink_to(self.ws / 'real')
        (self.ws / 'nested' / 'target').mkdir(parents=True)
        for output in ('dir-out', 'file-out', 'link-out', 'link-dir', 'link-dir/new',
                       '../escaped', '.', 'target/pkg', 'nested/target/pkg', 'target'):
            with self.subTest(output=output):
                self.refused('Worker output', output=output)
        self.assertEqual((self.ws / 'dir-out' / 'keep').read_text(), 'precious')
        self.assertEqual((self.ws / 'file-out').read_text(), 'precious')
        self.assertFalse((self.root / 'escaped').exists())
        self.assertEqual(list((self.ws / 'real').iterdir()), [])

    # -- tool pins and presence: before any lease ------------------------

    @contextlib.contextmanager
    def tool_path(self, path):
        empty = self.root / 'empty-home'
        empty.mkdir(exist_ok=True)
        with patch.dict(os.environ, {'PATH': str(path), 'XDG_CACHE_HOME': str(empty),
                                     'CARGO_HOME': str(empty), 'HOME': str(empty)}):
            yield

    def test_missing_tool_refused_before_lease(self):
        for missing in VERSIONS:
            with self.subTest(missing=missing):
                partial = self.root / f'bin-without-{missing}'
                partial.mkdir()
                for name in VERSIONS:
                    if name != missing:
                        (partial / name).symlink_to(self.bin / name)
                with self.tool_path(partial), \
                        patch.object(cache, 'acquire', side_effect=AssertionError('lease')), \
                        self.cwd(self.ws):
                    with self.assertRaisesRegex(cache.Refusal, f'preinstalled {missing}|{missing} not found'):
                        cache.run_worker_build(self.pool, Path('worker'), Path('out'))
                self.assert_nothing_leased()

    def test_wrong_tool_version_refused_before_lease(self):
        for name in VERSIONS:
            with self.subTest(tool=name):
                (self.ctl / f'version-{name}').write_text(VERSIONS[name] + '0')  # prefix trap
                try:
                    with self.tool_path(self.bin), \
                            patch.object(cache, 'acquire', side_effect=AssertionError('lease')), \
                            self.cwd(self.ws):
                        with self.assertRaisesRegex(cache.Refusal, f'{name} must be version'):
                            cache.run_worker_build(self.pool, Path('worker'), Path('out'))
                finally:
                    (self.ctl / f'version-{name}').unlink()
                self.assert_nothing_leased()

    def test_pinned_tools_resolve_from_path(self):
        with self.tool_path(self.bin):
            self.assertEqual(worker_package.resolve_tools(), self.tools)

    # -- child environment and publication -------------------------------

    def test_child_gets_fixed_flags_target_env_and_explicit_tools(self):
        self.assertEqual(self.build(), 0)
        record = self.record()
        self.assertEqual(record['argv'][0], self.tools[0])
        out_dir = Path(record['argv'][record['argv'].index('--out-dir') + 1])
        self.assertEqual(record['argv'][1:3], [str(self.ws / 'worker'), '--release'])
        self.assertEqual(record['argv'][-2:], ['--offline', '--locked'])
        self.assertEqual(record['argv'][3], '--out-dir')
        env = record['env']
        target = Path(env['CARGO_TARGET_DIR'])
        scratch = Path(env['TMPDIR'])
        self.assertEqual(env['CARGO_BUILD_TARGET_DIR'], str(target))
        self.assertEqual(target.parent.parent.parent, self.pool)
        self.assertEqual(target.parent.name, 'target')
        self.assertEqual(target.name, cache.workspace_subdir_name(self.ws))
        self.assertEqual(scratch, target.parent.parent / 'scratch' / target.name)
        self.assertEqual(out_dir.parent.parent, scratch)  # staged in lease scratch
        self.assertEqual(env['CARGO_INCREMENTAL'], '0')
        self.assertEqual(env['CARGO_NET_OFFLINE'], 'true')
        self.assertEqual((env['WASM_BINDGEN_BIN'], env['WASM_OPT_BIN'], env['ESBUILD_BIN']),
                         tuple(self.tools[1:]))
        self.assertIsNone(env['CUSTOM_SHIM'])
        self.assertIsNone(env['COREDUMP'])

    def test_success_publishes_complete_output_and_releases_lease(self):
        self.assertEqual(self.build(), 0)
        out = self.ws / 'out'
        self.assertEqual(sorted(p.relative_to(out).as_posix() for p in out.rglob('*')),
                         ['worker', 'worker/index.wasm', 'worker/shim.mjs'])
        self.assertEqual(list(self.pool.glob('slot-*/lease.json')), [])
        self.assertEqual(list(self.pool.glob('slot-*/scratch/*/*')), [])  # staging removed
        self.assertFalse((self.ws / 'target').exists())
        self.assertTrue(all(lock_free(p) for p in self.pool.glob('slot-*')))

    def test_failed_or_incomplete_packages_publish_nothing(self):
        expected = {'fail': 3, 'no-entry': 75, 'symlink': 75, 'root-symlink': 75}
        for mode, code in expected.items():
            with self.subTest(mode=mode):
                self.set_mode(mode)
                self.assertEqual(self.build(), code)
                self.assertFalse((self.ws / 'out').exists())
                self.assertEqual(list(self.pool.glob('slot-*/scratch/*/*')), [])

    def test_output_appearing_during_build_is_preserved(self):
        self.set_mode('race')
        (self.ctl / 'race-output').write_text(str(self.ws / 'out'))
        self.assertEqual(self.build(), 75)
        self.assertEqual(sorted(p.name for p in (self.ws / 'out').iterdir()), ['keep'])
        self.assertEqual((self.ws / 'out' / 'keep').read_text(), 'precious')

    def test_child_refuses_to_run_without_active_lease(self):
        # The packaging child is not a build entry point.
        env = {**os.environ, 'CARGO_TARGET_DIR': str(self.root / 'tgt'),
               'TMPDIR': str(self.root / 'tmp')}
        (self.root / 'tgt').mkdir()
        (self.root / 'tmp').mkdir()
        result = subprocess.run(
            [sys.executable, str(SCRIPT.with_name('worker_package.py')), str(self.ws / 'worker'),
             str(self.ws / 'out'), str(self.bin / 'cargo'), *self.tools],
            env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 75)
        self.assertIn('requires the active bounded lease', result.stderr)
        self.assertFalse((self.ws / 'out').exists())
        self.assertFalse((self.ctl / 'record-ws.json').exists())

    # -- preflight: target presence and locked metadata, under the lease --

    def preflight_outcome(self):
        """Exit code, or None when refused before spawning; never a success."""
        try:
            return self.build()
        except cache.Refusal:
            return None

    def test_preflight_runs_metadata_before_builder_under_lease(self):
        lock = self.ws / 'worker' / 'Cargo.lock'
        lock.write_bytes(b'sentinel-lock')
        self.assertEqual(self.build(), 0)
        events = (self.ctl / 'events').read_text().split()
        self.assertEqual(events[:2], ['metadata', 'builder'])
        metadata = json.loads((self.ctl / 'metadata.json').read_text())
        argv = metadata['argv']
        for flag in ('--offline', '--locked'):
            self.assertIn(flag, argv)
        self.assertEqual(argv[argv.index('--manifest-path') + 1],
                         str(self.ws / 'worker' / 'Cargo.toml'))
        self.assertEqual(metadata['target'], self.record()['env']['CARGO_TARGET_DIR'])
        self.assertEqual(metadata['tmpdir'], self.record()['env']['TMPDIR'])
        self.assertEqual((self.ctl / 'rustc-env').read_text(), '0')
        self.assertEqual(self.record()['env']['RUSTUP_AUTO_INSTALL'], '0')

    def test_failed_metadata_leaves_lock_and_never_invokes_builder(self):
        lock = self.ws / 'worker' / 'Cargo.lock'
        lock.write_bytes(b'sentinel-lock')
        (self.ctl / 'metadata-fail').touch()
        self.assertNotEqual(self.preflight_outcome(), 0)
        self.assertEqual(lock.read_bytes(), b'sentinel-lock')
        self.assertFalse((self.ctl / 'record-ws.json').exists())
        self.assertNotIn('builder', (self.ctl / 'events').read_text().split())
        self.assertFalse((self.ws / 'out').exists())
        self.assertEqual(list(self.pool.glob('slot-*/scratch/*/*')), [])

    def test_missing_wasm_target_never_invokes_builder(self):
        (self.libdir / 'libstd-0123abcd.rlib').unlink()
        (self.libdir / 'libcore-0123abcd.rmeta').write_bytes(b'not std')  # not enough
        self.assertNotEqual(self.preflight_outcome(), 0)
        self.assertFalse((self.ctl / 'record-ws.json').exists())
        self.assertFalse((self.ctl / 'events').exists()
                         and 'builder' in (self.ctl / 'events').read_text().split())
        self.assertFalse((self.ws / 'out').exists())

    def test_nonexistent_wasm_libdir_never_invokes_builder(self):
        (self.ctl / 'libdir').write_text(str(self.root / 'no-such-libdir'))
        self.assertNotEqual(self.preflight_outcome(), 0)
        self.assertFalse((self.ctl / 'record-ws.json').exists())

    # -- packaging phase keeps lock + lease ------------------------------

    def test_lock_and_lease_held_after_cargo_exits_then_released(self):
        self.set_mode('hold')
        child = self.start(self.ws)
        self.wait_for(self.ctl / 'cargo-exited-ws', 'cargo exit')
        slot = self.pool / 'slot-0'
        lease = self.lease(slot)
        self.assertEqual(lease['workspace'], str(self.ws))
        self.assertTrue(pid_alive(self.record()['pid']))
        self.assertFalse(lock_free(slot))
        self.assertFalse((self.ws / 'out').exists())  # nothing published yet
        # Another worktree is routed to the other slot; the held one is skipped.
        other, _, fd = cache.acquire(self.pool, self.policy, self.root)
        try:
            self.assertEqual(other.name, 'slot-1')
        finally:
            os.close(fd)
        (self.ctl / 'release').touch()
        error = self.finish(child)
        self.assertEqual(child.returncode, 0, error)
        self.assertFalse((slot / 'lease.json').exists())
        self.assertTrue(lock_free(slot))
        self.assertTrue((self.ws / 'out' / 'worker' / 'shim.mjs').is_file())

    def test_lock_survives_wrapper_and_package_parent_sigkill(self):
        # The lease FD must reach worker-build itself, not just the wrappers.
        self.set_mode('hold')
        child = self.start(self.ws)
        self.wait_for(self.ctl / 'cargo-exited-ws', 'cargo exit')
        slot = self.pool / 'slot-0'
        lease = self.lease(slot)
        worker_pid = self.record()['pid']
        self.pids.append(worker_pid)
        os.kill(child.pid, signal.SIGKILL)
        child.wait(timeout=10)
        os.kill(lease['cargo_pgid'], signal.SIGKILL)  # worker_package.py leader only
        deadline = time.monotonic() + 10
        while pid_alive(lease['cargo_pgid']) and time.monotonic() < deadline:
            try:
                os.waitpid(lease['cargo_pgid'], os.WNOHANG)  # not our child; ignore
            except ChildProcessError:
                pass
            time.sleep(.01)
        self.assertTrue(pid_alive(worker_pid))
        self.assertFalse(lock_free(slot))
        self.assertTrue((slot / 'lease.json').exists())
        with self.assertRaisesRegex(cache.Refusal, 'no idle'):
            slot1, _, fd1 = cache.acquire(self.pool, self.policy, self.root)
            try:
                cache.acquire(self.pool, self.policy, self.root)
            finally:
                os.close(fd1)

    def assert_group_killed_lease_retained(self, child, slot, pattern):
        error = self.finish(child)
        self.assertEqual(child.returncode, 75, error)
        self.assertRegex(error, pattern)
        lease = json.loads((slot / 'lease.json').read_text())
        self.assertIn('cargo_pgid', lease)
        grandchild = int((self.ctl / 'grandchild-ws').read_text())
        deadline = time.monotonic() + 10
        while (pid_alive(self.record()['pid']) or pid_alive(grandchild)) \
                and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertFalse(cache.group_alive(lease['cargo_pgid']))
        self.assertFalse(pid_alive(grandchild))
        self.assertTrue(lock_free(slot))  # wrapper closed its FD after the kill
        self.assertFalse((self.ws / 'out').exists())

    def test_cancellation_kills_group_and_retains_sentinel(self):
        self.set_mode('hold')
        child = self.start(self.ws)
        self.wait_for(self.ctl / 'cargo-exited-ws', 'cargo exit')
        slot = self.pool / 'slot-0'
        self.lease(slot)
        child.send_signal(signal.SIGTERM)
        self.assert_group_killed_lease_retained(child, slot, 'interrupted by signal')
        self.assertTrue((self.ws / 'worker').is_dir())

    def test_budget_overrun_kills_group_and_retains_sentinel(self):
        self.set_mode('grow')
        child = self.start(self.ws)
        slot = self.pool / 'slot-0'
        self.assert_group_killed_lease_retained(child, slot, 'sampled byte budget')
        # Oversized retained slot is not handed out again.
        got, _, fd = cache.acquire(self.pool, self.policy, self.root)
        os.close(fd)
        self.assertEqual(got.name, 'slot-1')

    def test_third_concurrent_build_refused_without_fallback(self):
        self.set_mode('hold')
        second_ws = self.make_workspace('ws2')
        third_ws = self.make_workspace('ws3')
        first = self.start(self.ws)
        second = self.start(second_ws)
        self.wait_for(self.ctl / 'cargo-exited-ws', 'first cargo exit')
        self.wait_for(self.ctl / 'cargo-exited-ws2', 'second cargo exit')
        with patch.object(subprocess, 'Popen', side_effect=AssertionError('spawned')):
            with self.assertRaisesRegex(cache.Refusal, 'no idle'):
                self.build(workspace=third_ws)
        for path in (self.ws, second_ws, third_ws):
            self.assertFalse((path / 'target').exists())
        self.assertFalse((third_ws / 'out').exists())
        self.assertEqual(len(list(self.pool.glob('slot-*/lease.json'))), 2)
        (self.ctl / 'release').touch()
        for child in (first, second):
            error = self.finish(child)
            self.assertEqual(child.returncode, 0, error)


if __name__ == '__main__':
    unittest.main()
