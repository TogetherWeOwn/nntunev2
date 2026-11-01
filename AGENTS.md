# NNTune v2 agent notes

## Local Rust builds: the bounded Cargo pool

Compiling Cargo commands never use a per-worktree `target/`. They go
through the bounded slot pool in `scripts/cargo_cache.py`, either
directly or via the host `cargo` shim (which routes automatically when
that file exists):

```sh
python3 scripts/cargo_cache.py run -- check
python3 scripts/cargo_cache.py run -- test
python3 scripts/cargo_cache.py run -- clippy --all-targets -- -D warnings
python3 scripts/cargo_cache.py run -- build
```

Per-manifest crates work the same way, e.g. `run -- test
--manifest-path workers/api/Cargo.toml`. `run -- fmt` is allowed;
bare `cargo fmt` does not compile and may run directly, but prefer the
wrapper for consistency.

Pool facts:

- Two slots, each with a sampled byte budget (see
  `scripts/cargo_pool_policy.json`, provisioned on the host under
  `/paperclip/.cache/nntunev2-bounded`). The hard aggregate bound is a
  host-enforced filesystem quota, attested by the Operator receipt in
  the pool policy — not by the script.
- The wrapper adds `--offline --locked`, redirects `CARGO_TARGET_DIR`
  and `TMPDIR` into per-worktree subdirs (`ws-<hash>`) under the leased
  slot, disables incremental compilation and dev/test debuginfo in the
  child environment only, and rejects output/config overrides and
  manifests outside the workspace. Acquire evicts other worktrees'
  build-output-only subdirs to preserve the slot budget (protected
  material refuses the slot, leaving it unchanged); legacy
  shared-target slots migrate automatically after the same check.
- Exit 75 means refused: both slots busy, a slot over budget, a stale
  lease needing Operator inspection, a missing/bad pool, or low
  filesystem headroom. Retry later. Never fall back to a per-worktree
  `target/`, never set `CARGO_TARGET_DIR` manually, and never run a
  direct compiling `cargo` command to dodge the pool.
- A third concurrent invocation exits 75 immediately; it does not
  queue. Only `build`, `check`, `test`, `clippy` and `fmt` are
  accepted. Dependency metadata commands (`update`, `add`, `tree`)
  do not compile and run directly.
- One interrupted run keeps its `lease.json` sentinel; a stale
  under-budget lease with a dead Cargo process group is recovered on
  the next acquire, anything else stays for Operator inspection.

## Contributing

PRs follow the org contract in `TogetherWeOwn/.github` CONTRIBUTING + this repo's overrides; use this repo's PR template, which replaces the org template.
