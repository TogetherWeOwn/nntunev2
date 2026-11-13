# Programmatic ingest session contract (proposal)

This is inactive preparation, not an endpoint, credential grant, storage or
crypto implementation. Coordinate with upload, authorization and crypto
consumers and obtain independent Security and QA review before activation.
The existing administrator routes do not grant these permissions.

## Operations and authorization

- `POST /v1/ingest/sessions`: create a FLAC session using bounded metadata,
  declared length, a claimed plaintext SHA-256 and an idempotency-key header.
- `GET /v1/ingest/sessions/{session_id}`: return the owned session's existing
  ingest job status vocabulary.
- `POST /v1/ingest/sessions/{session_id}/abort`: request an owned session's
  abort. Define terminal-state behavior with consumers before activation.

Each operation rechecks the authenticated grant, expiry, revocation, environment,
namespace, ownership and the relevant ingest/status/reuse action; never trust a
caller-supplied owner. Auth implementation lives in the separate scoped grant
slice. Require an independently bounded request body at the transport boundary,
including before parsing; the helper bounds metadata to 16 KiB and declared file
size to 8 GiB but is not a body reader. Deployment limits remain profile inputs.

## Idempotency and upload integrity

After authorization, scope each key by environment, library and authenticated
principal. Atomically reserve that tuple with the versioned fingerprint and
intended session identity. A same-key, same-request retry returns that identity;
a changed request conflicts. The fingerprint hashes a domain/version, declared
length, claimed content digest and length-delimited metadata including optional
presence. Retention and post-expiry behavior require consumer agreement.
Fingerprints, keys, metadata and digests are sensitive: do not log them or put
them in error messages. Return only redacted typed errors and approved opaque
audit identifiers. Synthetic fixtures must not contain real media or credentials.

A caller's digest, declared length and metadata do **not** prove received byte
equality or grant no-upload reuse. Verify the actual plaintext digest and FLAC,
complete authenticated upload and identity checks before publication. Reuse needs
explicit authorization for a fully validated object in the same namespace. This
helper compares create requests only: it does not verify content, establish
ownership, reserve a key, accept upload pieces or implement deduplication.

Above the deployed Worker body limit, bounded authenticated requests feed the
existing queue/checkpoint and multipart pipeline; do not buffer whole files or
expose provider credentials or require browser CORS. Bind each piece/checkpoint
to environment, library, session, generation, offset, request fingerprint,
lease revision and expiry. Accepted piece replay must be byte-identical; changed
replay conflicts. If exact ciphertext replay or a trustworthy checkpoint is
unavailable, fence, abort and reconcile before starting a fresh lifecycle; never
reseal changed plaintext under an old lifecycle. Absolute expiry is at most 24 h,
with a 60-minute idle deadline since accepted progress; status and retries do
not extend absolute expiry. Tombstones, lease fencing, cleanup and publication
remain consumer integration obligations, not capabilities of this helper.
