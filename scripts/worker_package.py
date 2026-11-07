#!/usr/bin/env python3
"""Fixed Worker packaging child for cargo_cache.py; not a build entry point.

The parent owns admission, quota sampling, cancellation and the inherited
lease FD. All intermediate packaging stays in that lease's scratch directory.
Only a complete package is copied to a new workspace directory. Never installs
tools or substitutes a Cargo executable.
"""

import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

from cargo_cache import Refusal, real_directory, resolve_cargo


# Match the installed worker SDK and its upstream packaging toolchain. An
# intentional upgrade must update these pins and requalify the resulting Wasm.
TOOLS = (
    ('worker-build', '0.8.7', None, '0.8.7'),
    ('wasm-bindgen', '0.2.129',
     'wasm-bindgen-x86_64-unknown-linux-musl-0.2.129/wasm-bindgen', 'wasm-bindgen 0.2.129'),
    ('wasm-opt', '132', 'wasm-opt-x86_64-linux-132/bin/wasm-opt', 'wasm-opt version 132'),
    ('esbuild', '0.28.2', 'esbuild-linux-x64-0.28.2/bin/esbuild', '0.28.2'),
)


def validate_paths(workspace, crate, output):
    crate = real_directory(workspace / crate)
    manifest = crate / 'Cargo.toml'
    if (not crate.is_relative_to(workspace) or not manifest.is_file()
            or manifest.is_symlink()):
        raise Refusal('Worker crate must have a manifest inside this workspace')
    output = Path(os.path.abspath(workspace / output))
    if (output.resolve() != output or not output.is_relative_to(workspace)
            or output == workspace or output.exists() or output.is_symlink()):
        raise Refusal('Worker output must be a new non-symlink directory inside this workspace')
    # Do not allow Cargo output to become a per-worktree target tree, even
    # when the caller chooses an otherwise new path.
    if 'target' in output.relative_to(workspace).parts:
        raise Refusal('Worker output cannot be a per-worktree target directory')
    real_directory(output.parent)
    return crate, output


def resolve_tools():
    tools = []
    cache = Path(os.environ.get('XDG_CACHE_HOME', Path.home() / '.cache')) / 'worker-build'
    for name, version, cached, prefix in TOOLS:
        found = shutil.which(name)
        if found is None and name == 'worker-build':
            found = resolve_cargo(name)  # same Cargo-home/PATH fallback, no install
        if found is None and cached and platform.system() == 'Linux' and platform.machine() == 'x86_64':
            candidate = cache / cached
            if candidate.is_file() and os.access(candidate, os.X_OK):
                found = str(candidate)
        if found is None:
            raise Refusal(f'preinstalled {name} {version} required; no tool download')
        result = subprocess.run([found, '--version'], capture_output=True, text=True,
                                check=True, timeout=10)
        actual = result.stdout.strip()
        if actual != prefix and not actual.startswith(prefix + ' '):
            raise Refusal(f'{name} must be version {version}')
        tools.append(found)
    return tools


def package(crate, output, tools, cargo='cargo'):
    # This is a child-only check, not permission to start a build. The only
    # supported caller is run_worker_build, which holds the slot lock and
    # supervises this entire process group, including the final copy.
    target = real_directory(os.environ['CARGO_TARGET_DIR'])
    scratch = real_directory(os.environ['TMPDIR'])
    if (target.parent.name != 'target' or scratch.parent.name != 'scratch'
            or target.name != scratch.name or target.parent.parent != scratch.parent.parent
            or not (target.parent.parent / 'lease.json').is_file()):
        raise Refusal('Worker packaging requires the active bounded lease')
    if output.exists() or output.is_symlink():
        raise Refusal('Worker output already exists; preserved unchanged')
    env = os.environ.copy()
    env.update(CARGO_NET_OFFLINE='true', CARGO=str(cargo), RUSTUP_AUTO_INSTALL='0',
               WASM_BINDGEN_BIN=str(tools[1]), WASM_OPT_BIN=str(tools[2]),
               ESBUILD_BIN=str(tools[3]))
    # Upstream installs a missing Wasm target with rustup (Cargo offline does
    # not constrain rustup). Refuse first, without auto-installing a toolchain.
    libdir = subprocess.run(['rustc', '--print', 'target-libdir', '--target',
                             'wasm32-unknown-unknown'], env=env, capture_output=True,
                            text=True, check=True, timeout=15, close_fds=False)
    if not list(Path(libdir.stdout.strip()).glob('libstd-*.rlib')):
        raise Refusal('preinstalled wasm32-unknown-unknown Rust target required')
    # Upstream's initial Cargo metadata lacks --locked. Validate the same
    # manifest/environment first so a stale/missing lock cannot be repaired
    # behind the later locked build. No resolution output is logged.
    metadata = subprocess.run([str(cargo), 'metadata', '--offline', '--locked',
                               '--format-version', '1', '--manifest-path',
                               str(crate / 'Cargo.toml')], env=env,
                              stdout=subprocess.DEVNULL, close_fds=False)
    if metadata.returncode:
        return metadata.returncode
    # Do not allow inherited shim/debug hooks to replace upstream packaging.
    env.pop('CUSTOM_SHIM', None)
    env.pop('COREDUMP', None)
    with tempfile.TemporaryDirectory(prefix='worker-package-', dir=scratch) as temp:
        built = Path(temp) / 'build'
        command = [str(tools[0]), str(crate), '--release', '--out-dir', str(built),
                   '--offline', '--locked']
        # Keep the parent's inheritable lock FD in worker-build and Cargo.
        result = subprocess.run(command, env=env, close_fds=False)
        if result.returncode:
            return result.returncode
        if built.is_symlink() or not built.is_dir():
            raise Refusal('Worker package root is not a real directory')
        for item in built.rglob('*'):
            if item.is_symlink() or not (item.is_dir() or item.is_file()):
                raise Refusal('Worker package contains a non-regular entry')
        if not (built / 'worker' / 'shim.mjs').is_file():
            raise Refusal('Worker package is missing its entry point')
        # copytree refuses an existing destination; never delete or overwrite
        # an earlier artifact. A failed copy remains visibly incomplete.
        shutil.copytree(built, output)
    return 0


def main():
    try:
        if len(sys.argv) != 8:
            raise Refusal('use cargo_cache.py worker-build, not this child directly')
        return package(Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[4:], sys.argv[3])
    except (Refusal, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f'Worker packaging refused: {error}', file=sys.stderr)
        return 75


if __name__ == '__main__':
    sys.exit(main())
