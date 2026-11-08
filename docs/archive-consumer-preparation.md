# Archive consumer preparation (experimental, default off)

The core's `archive-provider` compile-time feature exposes the shared
`velmora::archive::v1` contract and NNTune's private `ArchiveSource` metadata
wrapper. It does not install an ingest writer, source service, live transport,
Worker cold-miss path or cache fill. Normal product builds leave it off.
`archive.write_backend` remains reserved: only `nntp` is currently admissible,
and the key does not reroute consumers. Neither backend is newly qualified by
this preparation.

## Immutable metadata boundary

`ArchiveSource` retains the exact shared `ObjectRecord`, including tenant,
backend, immutable routing-account identity, locator, generation, remote version,
ciphertext length/digest, encryption format/mode/epoch/envelope reference,
operation and part manifest. The account is a versioned routing configuration,
not a login name. Rotation must not redirect an old account to a different
endpoint, bucket or region.

The API remains the source-envelope/key owner. After bounded authenticated open,
the parser checks track/job and the full object identity selected from trusted
catalogue/journal state. That expected identity must not come from a browser or
from today's write-target configuration. Errors contain no stored values. The
payload is capped at 2 MiB before JSON decoding; the shared bounded manifest
validator and serde recursion limit remain in force. Only `velmora.media.v1`
with NNTune's Random key mode is admissible.

`cache_key()` delegates to the shared domain-separated immutable identity. It is
an internal coordinate, not an ETag, entitlement, public existence check or
cache-hit oracle. This metadata does not authenticate media: restore/publication
and serving must still use `VerifiedHeader` and chunk authentication under
current authority, including cache hits. It grants no durability or eviction
permission. NNTP still needs its exact-generation independent-backbone proof;
S3 needs independently accepted backend-specific durable/readability evidence.
R2 remains common temporary/hot staging, cache and delivery, never an alternate
permanent archive when S3 is unavailable.

## Compatibility and retained integration boundaries

`OriginalSource` describes imported FLAC article metadata, not a media.v1
ciphertext manifest. Its existing format and parser remain unchanged. Assigning
invented ciphertext hashes, key epochs or remote versions to that import would
be a false conversion. `ArchiveSource` has an internal version field but does
not reserve a `track_sources` payload-format number, alter its schema or install
an API read/write path. A reviewed envelope/catalogue bridge must precede that
cutover. Old-source reads must keep their original resolver regardless of the
new-write policy; do not reinterpret old locators.

The existing NNTP writer, pool/broker, source resolver, original-media response
driver and cache implementation retain their ownership and gates. This module
is not a replacement for any of them. In the current Worker code a known-track
cold miss still returns `502 upstream_unavailable`. Both-backend ingest,
commit/abort/restart, real hit/miss/range restore and publication receipts remain
undelivered. No migration or provider activation is authorized.

## Invocation control boundary (not installed)

The corrected shared checkpoint requires both `Control::check` and the
`Control::cancelled(target, action)` future. A future invocation adapter must
bind trusted current authority, cancellation and deadline wake sources for the
exact target/action, registering before rechecking policy. A pending future that
never wakes, or checks only when socket I/O progresses, is not a live control.
This metadata module does not implement `Control` or invoke a provider.

The shared NNTP client adapter interrupts pending POST/BODY/STAT and invalidates
and attempts shutdown without QUIT. That attempt is not physical-close proof:
the existing product broker must retain the lease until closure/settlement is
proved. A post-ack readback failure remains uncertain, not absence or rollback.
Final commit/reconcile proof must still authorize Write even if old-object Read
remains allowed. These are integration requirements, not consumer cancellation,
restart or pool-settlement receipts delivered by the metadata tests.

## Dependency and verification limits

This draft uses immutable shared checkpoint
`40dba9a8cedb27748fa22e8a1d63cb2132067efd` from
[Velmora PR #19](https://github.com/TogetherWeOwn/velmora/pull/19), not an accepted
production pin. The source bump also includes the previously merged bounded
repair-cleanup fix. The shared API, full dependency delta, NNTune consumer and
applicable Security/Crypto/QA gates must be accepted before this PR is ready.

The upstream manifest delta adds only an empty feature; registry dependencies
are unchanged. Git source coordinates in both shim manifests and all five
committed lockfiles move together. No dependency installation or source-tree
materialization was performed locally; Actions validates locked resolution and
builds. Cargo has no blanket native release-age filter or dependency
build-script/proc-macro refusal. Exact pins and `--locked` do not supply those
controls; isolated Actions checks and dependency review remain necessary.

Generated-byte tests cover metadata round trips for both backend kinds, every
cache-identity coordinate, track/job binding, malformed manifests, invalid media
geometry, foreign modes/formats, immutable versions, input bounds and legacy
format refusal. These are metadata tests, not fake-provider, media-authentication,
live supplier or real Worker integration receipts. Feature-on native tests,
clippy and wasm checks are wired into the existing Actions jobs. No local full
suite or CI mirror is permitted; unexecuted checks are not passing evidence.

REA angle: reuse the existing source envelope, shared immutable identity,
media reader and common cache paths rather than invent another provider engine.
Changes follow `AGENTS.md`'s Clean-room, YouTube and MetaBrainz guardrails.
