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
3. **Streaming:** the stream Worker proxies ranged GETs using the read-only
   key, preserving the signed grant check, range handling and metering.
4. **Provider owns:** NNTP/Usenet access, posting and verification, provider
   accounts, at-rest encryption, caching, PAR2, repair and retention.
5. **End state:** NNTune holds no NNTP code, no provider accounts and no
   storage-side media keys. Endpoint changes are configuration only. Cold
   reads may be slower than cached ones.
6. **Locator:** a track's media is addressed by `{bucket, key, etag, size}`.
   The ETag is an opaque value from the store, never a content hash. A
   successful PUT (HTTP 200) must precede the atomic pointer switch to the
   new object.
7. **Schema:** forward-expand `s3_bucket`, `s3_key`, `s3_etag` and `s3_size`
   alongside the existing `nzb_ref_*` columns; contract (drop the old
   columns) only in a later step once nothing reads them.
8. **Code seams (target, not yet done):** `stream/play.rs` keeps grants,
   ranges and metering but replaces the NNTP miss path and the demo XOR path
   with a read-only S3 GET; `api/admin_ingest.rs` sources the FLAC object
   from S3; the core NNTP shim is removed only after its last caller is gone.
9. **Rollout:** disposable buckets only until the provider is declared
   production-ready.

## Unchanged and unresolved

- Everything merged stays as is until separately changed: this record moves
  no code, keys, credentials, production or runtime state.
- Open owner questions: how curated NZB titles enter storage, whether to keep
  application-level sha256-to-key dedupe, and whether NNTune adds its own
  end-to-end encryption on top. No dedupe or encryption choice is approved
  by this ADR. Until answered, the existing worker ingest path that fetches,
  verifies and repairs NZBs, and related in-flight work, are left untouched.
- Security, rights, YouTube and MetaBrainz restrictions in `AGENTS.md` and
  ADR 0012 are unchanged.

## REA angle

Reuse standard S3 protocol and client interoperability, from public protocol
documentation only; no competitor artefacts.
