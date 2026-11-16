# Worker Rust provenance prerequisite

The standalone `.github/scripts/worker_rust_provenance.py` checks the pinned
Rust tuple in `.github/worker-toolchain.json`. It is not wired into the canonical
release workflow. Its `rust` command is intended for GitHub Actions on Linux
x86_64; `verify-rust` checks the active tuple and previously verified standard
library digests. Integration must invoke these checks at the appropriate build
boundary and protect the digest file. `check_lock_identity` verifies the API
lock pin when called; it is not automatically enforced by either command.

The pins identify the [Rust 1.98.1 channel manifest](https://static.rust-lang.org/dist/channel-rust-1.98.1.toml),
[source revision](https://github.com/rust-lang/rust/tree/48a229ceaefd4985c50990b14116b6d856af0985),
[WASM allocator implementation](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/std/src/sys/alloc/wasm.rs),
and [standard-library lockfile](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/Cargo.lock).
The source names dlmalloc 0.2.13. These identities are not an allocation
certificate and do not bind a build to uploaded bytes. This prerequisite does
not enable snapshot construction or activate a mount.
