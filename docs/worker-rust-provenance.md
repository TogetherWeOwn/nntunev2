# Worker Rust provenance prerequisite

The `.github/scripts/worker_rust_provenance.py` machinery uses the canonical
`.github/worker-toolchain.json` pins. The `rust` command installs and checks
Rust for Linux x86_64 Actions; `verify-rust` checks host executables, the active
tuple, the API lock and installed wasm standard library against pinned inputs.
Worker tooling also checks the API lock and active tuple during install and
build verification. These checks are prerequisites, not candidate certification.

`verify-candidate-boundary` **always exits nonzero**: no ordinary candidate
is eligible for an allocation-bound certificate, whether provenance is missing,
changed, or apparently present. This refusal does not stop ordinary Worker
builds or deployments and is not wired as a release gate. It must not be used as
a substitute for the existing runtime refusal of unproven snapshot rows.

The ordinary Worker build does not couple the verified compiler paths and wasm
std to its target, effective configuration, linked allocator and final bytes.
Unlike the synthetic ledger feature, it does not select `System` to reject a
conflicting active dependency global allocator; direct allocator calls and custom
arenas also remain outside that diagnostic. A lockfile alone does not attest
resolved dependency sources or build-script inputs. Host tools and dynamic loader
inputs are not authenticated, and files may change between check and use. No
artifact digest is bound through the Worker build and upload. Linker identity,
options and the resulting heap-end alignment also remain unproved. The synthetic
ledger build and its digest establish none of these facts for a candidate. See
[subsonic snapshot construction](subsonic-snapshot-construction.md) for the
remaining allocator argument and runtime refusal.

The pins identify the [Rust 1.98.1 channel manifest](https://static.rust-lang.org/dist/channel-rust-1.98.1.toml),
[source revision](https://github.com/rust-lang/rust/tree/48a229ceaefd4985c50990b14116b6d856af0985),
[WASM allocator implementation](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/std/src/sys/alloc/wasm.rs),
and [standard-library lockfile](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/Cargo.lock).
The source names dlmalloc 0.2.13. These identities are not an allocation
certificate and do not bind a build to uploaded bytes. This prerequisite does
not enable snapshot construction or activate a mount.
