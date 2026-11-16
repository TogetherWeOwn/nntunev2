# Worker allocation provenance

The ordinary Worker release builds use the Rust tuple in
`.github/worker-toolchain.json`, not a floating toolchain. The manifest records
the official [Rust 1.98.1 channel manifest](https://static.rust-lang.org/dist/channel-rust-1.98.1.toml)
and the integrity identities of the target standard library and source archives.
The source revision is
[`48a229ceaefd4985c50990b14116b6d856af0985`](https://github.com/rust-lang/rust/tree/48a229ceaefd4985c50990b14116b6d856af0985).
Its [WASM system allocator implementation](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/std/src/sys/alloc/wasm.rs)
uses dlmalloc; the [standard-library lockfile](https://github.com/rust-lang/rust/blob/48a229ceaefd4985c50990b14116b6d856af0985/library/Cargo.lock)
identifies version 0.2.13 and its registry checksum. This is a source identity,
not a proof of an allocation charge.

The release build records the lockfile and resulting WASM digests along with the
compiler and tool versions. The pinned source and target archives are checked
against the channel manifest; the installed target standard library is compared
to the verified digests before each Worker build and provenance (missing
evidence fails); Cargo configs and alternate rustup sources are refused. The compiler and Cargo identities are checked using
their reported versions and rustup's installation checks. A build log is not an
independent attestation that later-uploaded bytes match the recorded digest.

**No allocation certificate is established by this pin.** In particular, it
proves no allocator rounding or metadata bound, collection capacity bound,
raw-row receipt, or complete application memory cap. It must not admit snapshot
construction: those require a separate source-derived certificate and a release
WASM witness bound to the actual build, including failure and mismatch tests.
The existing unactivated construction and mount behaviour remains unchanged.
