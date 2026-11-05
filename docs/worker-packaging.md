# Bounded local Worker packaging

Use the repository's pool entry point from the worktree root:

```sh
python3 scripts/cargo_cache.py worker-build \
  --crate workers/stream --output workers/stream/build
```

`--output` must be a **new** directory inside the current worktree, with an
existing real parent. Existing output is preserved and refused, not replaced.
For a repeat build, choose another new ignored artifact directory or inspect
and explicitly remove the previous generated package first. A `target` path,
a symlink, a crate outside the workspace, and a symlinked manifest are refused.
Do not run `scripts/worker_package.py` directly: it is the supervised child,
not an alternative entry point.

## Tool prerequisites

Packaging never installs or downloads tools. Put these preinstalled versions on
`PATH` (the usual Cargo-home fallback and upstream Linux x86-64 tool cache are
also recognized):

- `worker-build` **0.8.7**, matching the Worker SDK;
- `wasm-bindgen` **0.2.129**, matching the Cargo lockfile;
- `wasm-opt` **132**;
- `esbuild` **0.28.2**.

Missing or mismatched tools refuse before taking a lease. The selected Rust
toolchain must already have `wasm32-unknown-unknown` installed. Inside the lease,
a no-auto-install `rustc` check refuses a missing target before upstream can
invoke `rustup target add`. A separate `cargo metadata --offline --locked`
preflight refuses a missing/stale lockfile before upstream's unlocked metadata
can rewrite it.

If the harness uses a sandbox home, put the installed binaries on `PATH`
explicitly; do not copy or masquerade an executable as Cargo. The wrapper calls
the actual upstream `worker-build`, which performs a real
`cargo build --release --offline --locked`. Cargo metadata and Cargo's
compilation inherit the **same** pool target path.
The pinned optimizer, binding generator and bundler are selected explicitly;
inherited `CUSTOM_SHIM` and `COREDUMP` hooks are not used.

The peer-pinned TLS feature's `ring` backend also needs Clang's wasm32 C target.
The CI workflow explicitly selects `clang-18` and `llvm-ar-18` for wasm32 only;
local callers can use the same `CC_wasm32_unknown_unknown` and
`AR_wasm32_unknown_unknown` environment variables. These select a host-installed
compiler, not a hermetic or patch-version-pinned toolchain. A missing compiler
fails the build; do not install host packages from an agent run.

## Lease and artifact contract

The ordinary pool admission and supervision implementation is shared with
`run -- build/check/test/clippy/fmt`:

- One non-queued slot lease spans compilation **and** packaging. There is no
  nested lease, target-directory override, skip-build workaround or Cargo shim.
- Compilation, temporary output and all intermediate package files stay in the
  leased target/scratch trees. Budget/headroom sampling and process-group
  cancellation continue after Cargo exits, through the binding, optimization,
  bundling and final-copy phases.
- The lease FD is inherited by the packaging child, upstream builder and Cargo.
  Cancellation stops the whole process group and leaves the crash sentinel for
  the normal recovery checks. A third invocation still refuses immediately.
- Only after upstream packaging succeeds and a regular `worker/shim.mjs` entry
  exists is the finished package copied to the requested new directory. This
  final, deliberate artifact copy lives outside the pool quota; it is not a
  second compiler cache. A copy interrupted or failed partway is not a usable
  artifact: success requires the wrapper's zero exit status. The wrapper never
  overwrites such a directory on retry.
- The standard exit-75 refusal and preservation rules remain unchanged. Never
  fall back to a per-worktree target or alter a pool policy/lease by hand.

`<output>/worker/shim.mjs` is the **generated package entry**, with the upstream
JavaScript, Wasm and package metadata beside it. It is not necessarily the
configured deploy entry. In particular, the stream Worker must use its checked-in
`entry.mjs`, retaining the `./build/worker/shim.mjs` relative package layout.
Do not deploy or qualify stream by substituting the generated shim: that would
bypass application-owned entry behavior when such behavior is installed.

The current stream entry is deliberately a pass-through. Its account runtime,
class/binding/migration and ingress decorator are uninstalled source, not a
qualified runtime hidden behind a flag. Source tests require the account exports
to be absent and existing object dispatch unchanged. Reinstalling these adapters
for actual Worker qualification needs separate authorization and review of the
final configured entry/bundle; direct-shim execution cannot supply that proof.

Record a checksum of the final `index_bg.wasm`, source commit, configured entry
and build result when using an artifact for qualification. Packaging is **not**
a deploy, an NNTP fixture handshake, proof that an unused adapter survived dead-
code elimination, or evidence of Worker CSPRNG, certificate time or physical
socket closure. Those require a reachable qualification call site in an
authorized non-production Worker and an owned synthetic peer. Production peer
validation and public-CA verification must not be relaxed to make a fixture work.

## Offline regressions

```sh
python3 scripts/test_cargo_cache.py
python3 scripts/test_worker_package.py
python3 scripts/test_pool_capacity_acceptance.py
```

These tests use small fake executables and synthetic pools; they never compile,
contact providers, download tools or provision a cloud environment. CI runs them
in the existing `cargo-pool` gate.
