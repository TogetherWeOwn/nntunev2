# ADR 0015: Standard S3 storage boundary

- **Status:** Approved target direction (owner decision 2026-11-13). Design
  only: no implementation, migration or removal is claimed by this record.
- **Date:** 2026-11-13
- **Amends:** the NNTP-in-NNTune parts of ADR 0001 (core transport), ADR 0013
  (shared popularity cache) and ADR 0014 (backend provider pool). Those
  records stay as written, as history of what is merged today.

## Context

Today NNTune holds NNTP code, backend Usenet accounts and NZB/par2 handling
(ADRs 0001, 0013, 0014). The owner has decided that a storage provider will
own that whole concern, and NNTune will talk to it as an ordinary S3 client.

## Decision (target)

1. **NNTune is a plain, standard S3 client.** Configuration is an endpoint or
   hostname, a bucket and access keys. One bucket per environment; ingest
   holds a read-write key, streaming a read-only key.
2. **Operations used:** PUT, multipart upload, HEAD, ranged GET, LIST and
   DELETE, with ordinary read-after-write consistency only. No
   provider-specific features, no conditional PUT, and no dependence on
   browser CORS (the browser never reads the bucket directly).
3. **Streaming:** the stream Worker maps plaintext ranges to ciphertext
   chunks, fetches with the read-only key and authenticates each chunk before
   releasing its plaintext. It preserves signed grant checks, plaintext range
   semantics and plaintext metering; browsers never read the bucket directly.
4. **Provider owns:** NNTP/Usenet access, posting and verification, provider
   accounts, at-rest encryption, caching, PAR2, repair and retention.
5. **End state:** NNTune holds no NNTP code, NZB/Usenet awareness, backend
   provider accounts or storage-side media keys. S3 access keys and NNTune's
   client-side envelope keys remain application responsibilities, separate
   from provider at-rest encryption. Endpoint changes are configuration only.
   Cold reads may be slower than cached ones.
6. **Locator/publication:** `{bucket, key, etag, size}` identifies the stored
   ciphertext, not its plaintext length. Playback also needs trusted crypto
   metadata and plaintext size, with the exact record gated by crypto review.
   ETag is opaque, never a content hash. Publish the pointer atomically only
   after the whole encrypted object, S3 ciphertext checksum and plaintext
   digest/media validation succeed. Multipart completion must be validated:
   a part PUT 200 or completion HTTP 200 containing an error is not success.
7. **Schema:** forward-expand `s3_bucket`, `s3_key`, `s3_etag` and `s3_size`
   alongside the existing `nzb_ref_*` columns; contract (drop the old
   columns) only in a later step once nothing reads them. Crypto metadata
   and dedupe lifecycle need separately reviewed forward schemas.
8. **Code seams (target, not yet done):** `stream/play.rs` keeps grants,
   ranges and metering but replaces the NNTP miss path and demo XOR with
   authenticated S3 range decryption. Admin ingest consumes validated,
   encrypted S3 objects. The replacing ingest delivery retires the obsolete
   `workers/ingest` NZB fetch/verify/repair path; the core NNTP shim is removed
   after its final caller. No enabled legacy or plaintext fallback remains.
9. **Rollout:** disposable buckets only until the provider is declared
   production-ready. Replacement and removal form independently green,
   size-bounded PRs; no deletion before replacement or intermediate plaintext
   activation. Required review, qualification, rollback and release gates stay.
10. **Intake (resolved 2026-11-13):** owner-supplied audio files through an
    authenticated programmatic ingest API usable by scripts, services and
    apps. Require scoped credentials, idempotency keys and multipart/presigned
    upload support. Any transfer directly to S3 carries ciphertext only;
    neither plaintext nor envelope keys go to the provider.
11. **Dedupe (resolved 2026-11-13):** keep an application-side SHA256 of the
    plaintext to object-key map. Only validated completed objects enter it;
    a caller-supplied digest alone is not proof. Scope lookup and references
    to the authorized namespace and coordinate publication/deletion atomically.
12. **Encryption (resolved 2026-11-13):** every upload is client-side encrypted
    before S3, regardless of provider. Use envelope keys: a per-object data
    key wrapped by a versioned key-encryption key held in Worker secrets, plus
    seekable chunked AEAD for ranged playback. This is not end-user E2E:
    authorized NNTune Workers can decrypt. S3 upload checksums cover ciphertext,
    never substitute for AEAD, and are not ETags. Keep plaintext and keys out
    of logs; ingest uses rw access, streaming ro. Exact format, chunk size,
    vetted library, limits, rotation and range mapping require Security and
    Applied Cryptography approval before implementation.

## Unchanged gates and implementation status

- The owner has settled intake, plaintext dedupe and mandatory client-side
  encryption. They are requirements, not optional app E2E or pending questions.
  The earlier assurance-target and planning-input deferral basis is resolved;
  the revised design remains subject to exact-revision specialist acceptance.
  This ADR grants neither design acceptance nor implementation.
- Owner-set assurance inputs are integrity advantage <= 2^-57 and
  confidentiality advantage <= 2^-60, with failure-only accounting summed
  across the fleet (v = genuine authentication failures + 1). Plan for at most
  1,000,000 stored objects (conservatively bounded by 2^20) and a five-year
  key-safety horizon; successful reads and unwraps consume no failure budget.
  Permanently quarantine an object at its lifetime authentication-failure
  limit, recording that state durably; repair re-encrypts under a fresh key,
  not a reset of the old key's allowance. The revised design must quantify
  fleet exposure, concurrent overshoot and DB-restore/repeated-attack overshoot
  at these inputs, without requiring a nonrollback service. These are design
  requirements, not a claim of proven bounds or an accepted construction.
- The independent scoped session/idempotency contract and truthful
  documentation may continue. Crypto implementation and downstream reactivation
  require independent Security, Applied Cryptography and QA acceptance of the
  exact repaired design, plus verified producer outputs and restored dependencies.
- No plaintext fallback, hidden valid-playback/library cap or forced
  re-encryption merely to reset budgets is authorized. Quarantined-object
  repair still requires a fresh key. Preserve legacy readers and inventory,
  coexistence/disposition, restore and rollback obligations until the accepted
  replacement satisfies its activation and release gates.
- This documentation changes no code, credentials, provider resources or
  runtime state and authorizes no spend or deployment. Merged implementation
  stays in place until the reviewed replacement delivery retires it.
- Security, rights, YouTube and MetaBrainz restrictions in
  `AGENTS.md` (Clean-room, YouTube and MetaBrainz guardrails) and ADR 0012
  are unchanged.

## REA angle

Reuse standard S3 protocol and client interoperability, from public protocol
documentation only; no competitor artefacts.
