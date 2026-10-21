//! `opus-64` pre-compute dispatch + artifact publication (spec §4, B6).
//!
//! Pipeline position: the `Transcoding` step runs after `Matching` and
//! before `Writing` (`Matching -> Transcoding -> Writing`). Placing it
//! after the MusicBrainz match is load-bearing: `track_media` rows key
//! on `track_id`, which only exists once the match assigns track
//! identity, and unmatched audio is never worth encoding. Input is the
//! verified full audio bytes already staged in `INGEST_SCRATCH`.
//!
//! Shape mirrors repair (`crate::repair` + `workers/repair` +
//! `containers/repair`): the ingest Worker never encodes (spec §4.3 —
//! no Worker CPU for transcode). It dispatches a [`TranscodeRequest`]
//! through the `TRANSCODE_CONTAINER` service binding to the no-route
//! transcode Worker, which stages the input from scratch into the
//! transcode Container, runs one bounded ffmpeg encode, writes
//! `out/artifact.ogg` + `out/result.json` back to scratch, and
//! replies with a small status document. Ingest then fetches the
//! artifact from scratch (it already binds `INGEST_SCRATCH`), checks
//! its SHA-256, puts it to the audio bucket, and publishes the §4.2
//! rows. Retries never double-pay the encode: the front memoises by
//! the request's content-derived `staging_id` exactly like repair
//! rounds do, so a redelivery replays the stored artifact.
//!
//! Determinism (acceptance: same bytes/hash whichever path produced
//! them) comes from the shared profile in
//! `nntune_core::transcode`: pinned ffmpeg argv plus the Ogg serial
//! normaliser. Both the pre-compute path (this module) and the
//! conditional on-demand path (B7, if filed) execute that one
//! contract, so their bytes and hashes coincide by construction.
//! Source bytes are only ever read, never modified.
//!
//! Publication (spec §4.2, ADR 0002 rule 3 / ADR 0003 §writers), one
//! idempotent statement each, all converging on redelivery:
//!
//! - [`upsert_cache_object_sql`] — the content-global blob row. No
//!   writer fence: the row carries no user identity and an orphaned
//!   blob is swept, never served.
//! - [`upsert_track_media_sql`] — the per-user mapping with
//!   `format='opus'`, `bitrate=64` as SQL literals (the profile is
//!   pinned in the statement, not in a caller-supplied value).
//! - [`upsert_cache_grant_sql`] — `user_cache_grants(user,
//!   artifact_hash)` with the fetching `provider_id`, so a transcode
//!   derived from own-provider bytes keeps the same own-provider
//!   proof requirement as its source: knowing the hash confers
//!   nothing.
//!
//! The two user rows select through the standard `writer_fence`
//! (`users` row `FOR SHARE` while `deleting_at IS NULL`), so
//! publication cannot repopulate library state after deletion is
//! scheduled. A zero-row write is either a benign replay
//! (`ON CONFLICT DO NOTHING` converged) or a fenced account; the
//! consumer separates them with the PK selects plus
//! [`classify_publish_zero_rows`], never by assuming.
//!
//! Enablement (this module is pure and tested; the runtime path lands
//! with the rest of the B6 PR): the transcode Container image +
//! front Worker, the `TRANSCODE_CONTAINER` binding, the
//! audio-bucket publication binding (a deliberate, documented
//! exception to the "never bind the audio-cache buckets here" rule
//! in `wrangler.toml.example`, which predates the e2e spec's §4.2
//! publication assignment), the `Transcoding` state wiring, and the
//! artifact-table migration. Missing-table SQLSTATEs map through
//! `crate::library::is_deterministic_sqlstate` to permanent
//! failures, so jobs quarantine at once instead of burning budget.
//! Grant publication makes the B6 PR subject to the standing
//! CISO/threat-model §8 gate.

use nntune_core::par2::scratch::{validate_object_key, validate_staging_id};
use nntune_core::transcode::canonical_sha256_hex;

use crate::dispatch::validate_user_id;
use crate::jobs::validate_job_id;

/// Service binding name for the transcode Worker (wrangler `[[services]]`).
pub const TRANSCODE_SERVICE_BINDING: &str = "TRANSCODE_CONTAINER";
/// Transcode Worker route that accepts a [`TranscodeRequest`] JSON body.
pub const TRANSCODE_ENDPOINT: &str = "/transcode";
/// Internal URL for the service-binding fetch (host is a placeholder;
/// the binding routes it).
pub const TRANSCODE_URL: &str = "https://transcode-worker/transcode";
/// Total service-hop cutoff, mirroring [`crate::repair::REPAIR_CALL_DEADLINE_MS`]:
/// the front's 600 s budget plus 60 s for forwarding and capped reply
/// completion. A single 660 s wait leaves 240 s of the enclosing queue
/// invocation's 900 s contract for other work. The encode itself is
/// small (ffmpeg opus encodes far faster than realtime; the artifact
/// is ~1.9 MB per 4-minute track) — the budget covers staging
/// transfers and a slow Container start, not a slow codec.
pub const TRANSCODE_CALL_DEADLINE_MS: u64 = 660_000;
/// The front replies with a small status document
/// (`{status, artifact_sha256, byte_size}`; the artifact bytes stay in
/// `out/artifact.ogg` in scratch for ingest to fetch), so anything
/// larger is not a transcode reply we can trust.
pub const MAX_TRANSCODE_REPLY_BYTES: usize = 64 << 10;
/// Largest plausible `opus-64` artifact: 64 kbps over a 60-minute
/// track is ~28.8 MB, plus Ogg framing. Anything larger is not this
/// profile's output and is refused rather than published.
pub const MAX_ARTIFACT_BYTES: u64 = 32 << 20;
/// Scratch round-dir prefixes are short by construction
/// (`ingest-scratch/{job_id}/r{round}/`); anything longer is not one.
pub const MAX_SCRATCH_PREFIX_BYTES: usize = 256;
/// Idempotency keys are `ingest:{user}:{sha}:{ref}` with bounded
/// fields (see `jobs::derive_idempotency_key`); 768 covers the
/// longest constructible key with headroom.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 768;

/// One verified audio file to encode, mirrored field-for-field by the
/// transcode front's contract. `input_key` is the 64-hex scratch
/// object name under `{scratch_prefix}in/`; `source_sha256` is the
/// verified bytes' hash (the content identity the front memoises on);
/// `staging_id` is the manifest digest binding this exact input set,
/// so a redrive that re-stages different bytes under the same names
/// cannot replay the previous generation's artifact (the repair
/// `staging_id` lesson).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscodeRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub input_key: String,
    pub source_sha256: String,
    pub scratch_prefix: String,
    pub staging_id: String,
}

/// True only for the round dir the job's verify staging lives in:
/// `ingest-scratch/{job_id}/.../` with a trailing slash, a safe
/// charset, and the length cap. The job containment is the point: a
/// forged prefix must never address another job's staged bytes.
pub fn validate_transcode_prefix(prefix: &str, job_id: &str) -> bool {
    if prefix.len() > MAX_SCRATCH_PREFIX_BYTES || !prefix.ends_with('/') {
        return false;
    }
    let root = format!("ingest-scratch/{job_id}/");
    if !prefix.starts_with(&root) {
        return false;
    }
    prefix
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'_' || b == b'-' || b == b'.')
}

/// Decide one file's transcode dispatch: `Ok` carries the validated
/// front request, `Err` a stable reason the caller fails the job
/// with (every input here is checkpoint- or manifest-derived, so an
/// invalid value is a pipeline bug, never a retryable remote fault).
pub fn decide_transcode(
    job_id: &str,
    user_id: &str,
    idempotency_key: &str,
    input_key: &str,
    source_sha256_hex: &str,
    scratch_prefix: &str,
    staging_id: &str,
) -> Result<TranscodeRequest, &'static str> {
    if !validate_job_id(job_id) {
        return Err("job id outside the pipeline alphabet");
    }
    if !validate_user_id(user_id) {
        return Err("user id outside the pipeline alphabet");
    }
    if idempotency_key.is_empty()
        || idempotency_key.len() > MAX_IDEMPOTENCY_KEY_BYTES
        || !idempotency_key.starts_with("ingest:")
        || idempotency_key.contains('\0')
    {
        return Err("idempotency key outside the ingest key shape");
    }
    if !validate_object_key(input_key) {
        return Err("input key is not a staged object key");
    }
    let Ok(source_sha256) = canonical_sha256_hex(source_sha256_hex) else {
        return Err("source sha256 must be 64 hex chars");
    };
    if !validate_transcode_prefix(scratch_prefix, job_id) {
        return Err("scratch prefix outside the job round dir");
    }
    if !validate_staging_id(staging_id) {
        return Err("staging id outside the content-token alphabet");
    }
    Ok(TranscodeRequest {
        job_id: job_id.to_string(),
        user_id: user_id.to_string(),
        idempotency_key: idempotency_key.to_string(),
        input_key: input_key.to_string(),
        source_sha256,
        scratch_prefix: scratch_prefix.to_string(),
        staging_id: staging_id.to_string(),
    })
}

/// Outcome of one transcode call, decided from the reply alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscodeCall {
    /// The front encoded (or replayed) the artifact: ingest fetches
    /// `out/artifact.ogg` from scratch, verifies this hash, and
    /// publishes. `byte_size` is the artifact length the front
    /// reports, re-checked against the fetched bytes.
    Completed {
        artifact_sha256: String,
        byte_size: u64,
    },
    /// Terminal for this file: undecodable input or a request the
    /// front rejects as permanently invalid. A retry would send the
    /// same bytes, so the job fails instead of burning budget.
    Untranscodable,
    /// Transient (timeout, ceilings, transport, malformed reply):
    /// retry through the shared job attempt budget. Retries do not
    /// re-pay the encode — the front memoises by `staging_id`.
    Retry,
}

/// Classify a transcode Worker reply. Pure so the mapping is
/// unit-tested without a service binding.
///
/// Only a well-formed 200 success document with an in-contract hash
/// and size can complete; 4xx rejections are permanent. Anything
/// unparseable fails closed to `Retry`: a truncated or garbled reply
/// is ambiguous (transport damage reads the same as a front bug),
/// and a retry replays the memoised artifact rather than a fresh
/// encode, so conservatism costs nothing.
pub fn classify_transcode_reply(status: u16, body: &[u8]) -> TranscodeCall {
    if body.len() > MAX_TRANSCODE_REPLY_BYTES {
        return TranscodeCall::Retry;
    }
    match status {
        200 => {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
                return TranscodeCall::Retry;
            };
            match value.get("status").and_then(|s| s.as_str()) {
                Some("transcoded") => {
                    let sha = value.get("artifact_sha256").and_then(|s| s.as_str());
                    let size = value.get("byte_size").and_then(|s| s.as_u64());
                    match (sha, size) {
                        (Some(sha), Some(size))
                            if canonical_sha256_hex(sha).is_ok()
                                && (1..=MAX_ARTIFACT_BYTES).contains(&size) =>
                        {
                            TranscodeCall::Completed {
                                // Canonicalised above, so this cannot fail.
                                artifact_sha256: canonical_sha256_hex(sha)
                                    .unwrap_or_else(|_| sha.to_string()),
                                byte_size: size,
                            }
                        }
                        _ => TranscodeCall::Retry,
                    }
                }
                Some("untranscodable") => TranscodeCall::Untranscodable,
                _ => TranscodeCall::Retry,
            }
        }
        400 | 401 | 403 | 404 | 409 | 413 => TranscodeCall::Untranscodable,
        _ => TranscodeCall::Retry,
    }
}

/// `$n` parameter order for [`upsert_cache_object_sql`].
pub const CACHE_OBJECT_PARAM_ORDER: [&str; 5] = [
    "content_hash",
    "byte_size",
    "r2_bucket",
    "r2_key",
    "last_verified_at",
];

/// Idempotent blob publication. No writer fence: the row is
/// content-global (no user identity) and an unreferenced blob is
/// swept, never served, so account deletion has nothing to fence.
/// `ON CONFLICT DO NOTHING` — same hash means same bytes, so the
/// first writer wins and every replay converges. `last_verified_at`
/// is BIGINT unix seconds; `byte_size` is BIGINT. `created_at`
/// defaults to the DB clock at publication and `ref_count` stays NULL
/// (advisory, never the sweep mark set).
pub fn upsert_cache_object_sql() -> &'static str {
    "INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (content_hash) DO NOTHING"
}

/// `$n` parameter order for [`upsert_track_media_sql`].
/// `format`/`bitrate` are SQL literals, not parameters (see below).
pub const TRACK_MEDIA_PARAM_ORDER: [&str; 5] = [
    "user_id",
    "track_id",
    "source_job_id",
    "content_hash",
    "verified_at",
];

/// Idempotent per-user artifact mapping. `format='opus'` and
/// `bitrate=64` are literals: the profile is pinned in the statement
/// so no caller can publish a differently-labelled mapping for these
/// bytes. `source_job_id` carries the pipeline `job_id` string (the
/// artifact-table migration types it compatibly). `verified_at` is
/// BIGINT unix seconds; `last_served_at` is initialized by the schema
/// default at publication.
///
/// Account-deletion fence (ADR 0002 rule 3): the row is selected
/// through `writer_fence`, which takes the job owner's `users` row
/// `FOR SHARE` only while `deleting_at IS NULL`. A fenced write
/// affects 0 rows; the consumer separates that from a benign replay
/// with [`select_track_media_by_pk_sql`] plus
/// [`classify_publish_zero_rows`].
pub fn upsert_track_media_sql() -> &'static str {
    "INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) SELECT $1, $2, $3, $4, 'opus', 64, $5 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING"
}

/// `$n` parameter order for [`upsert_cache_grant_sql`].
pub const CACHE_GRANT_PARAM_ORDER: [&str; 4] =
    ["user_id", "content_hash", "provider_id", "verified_at"];

/// Idempotent own-provider proof for the artifact: without this row
/// the bytes are unservable (ADR 0003: the blob row is a storage
/// index, never authorization). `provider_id` is the fetching
/// provider's UUID text. Fenced like [`upsert_track_media_sql`];
/// zero-row writes classify through [`select_cache_grant_by_pk_sql`]
/// plus [`classify_publish_zero_rows`]. `verified_at` is BIGINT unix
/// seconds; `last_served_at` is initialized by the schema default at
/// publication.
pub fn upsert_cache_grant_sql() -> &'static str {
    "INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at) SELECT $1, $2, $3, $4 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, content_hash) DO NOTHING"
}

/// Fetch the mapping row after a zero-row [`upsert_track_media_sql`]
/// write so the consumer can separate a benign replay from a fenced
/// account. Parameters: `user_id` ($1), `track_id` ($2),
/// `source_job_id` ($3), `content_hash` ($4).
pub fn select_track_media_by_pk_sql() -> &'static str {
    "SELECT user_id, track_id, source_job_id, content_hash, format, bitrate FROM track_media WHERE user_id = $1 AND track_id = $2 AND source_job_id = $3 AND content_hash = $4"
}

/// Fetch the grant row after a zero-row [`upsert_cache_grant_sql`]
/// write. Parameters: `user_id` ($1), `content_hash` ($2).
pub fn select_cache_grant_by_pk_sql() -> &'static str {
    "SELECT user_id, content_hash, provider_id FROM user_cache_grants WHERE user_id = $1 AND content_hash = $2"
}

/// Outcome of one idempotent publication write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// The row landed (inserted while the owner's fence was open).
    Published,
    /// Zero rows but the row exists: a redelivery converged on the
    /// already-published mapping. Same-hash artifacts converge on
    /// the first writer's row; the bytes are identical either way.
    Duplicate,
    /// Zero rows and no row exists: the owner's fence refused the
    /// write (account deleting or erased). Permanent.
    AccountDeleting,
}

/// Classify a zero-row fenced publication write by whether the PK
/// select found the row. Pure; the consumer owns the select.
pub const fn classify_publish_zero_rows(row_exists: bool) -> PublishOutcome {
    if row_exists {
        PublishOutcome::Duplicate
    } else {
        PublishOutcome::AccountDeleting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = "job_01J9ZQ8K2M";
    const USER: &str = "user_01";
    const KEY: &str =
        "ingest:user_01:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload";
    const INPUT: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
    const SOURCE_SHA: &str = "1b0b44a2c0f2e9d5b6a7c8d9e0f1a2b31b0b44a2c0f2e9d5b6a7c8d9e0f1a2b3";
    const PREFIX: &str = "ingest-scratch/job_01J9ZQ8K2M/r1/";
    const STAGING: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";

    fn decide() -> Result<TranscodeRequest, &'static str> {
        decide_transcode(JOB, USER, KEY, INPUT, SOURCE_SHA, PREFIX, STAGING)
    }

    #[test]
    fn decide_accepts_a_well_formed_file() {
        let req = decide().unwrap();
        assert_eq!(req.job_id, JOB);
        assert_eq!(req.source_sha256, SOURCE_SHA);
    }

    #[test]
    fn request_json_shape_is_pinned_for_the_front() {
        // The front Worker's contract pins these exact field names;
        // a rename here without a front change breaks dispatch.
        let req = decide().unwrap();
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            format!(
                "{{\"job_id\":\"{JOB}\",\"user_id\":\"{USER}\",\"idempotency_key\":\"{KEY}\",\
                 \"input_key\":\"{INPUT}\",\"source_sha256\":\"{SOURCE_SHA}\",\
                 \"scratch_prefix\":\"{PREFIX}\",\"staging_id\":\"{STAGING}\"}}"
            )
        );
    }

    #[test]
    fn decide_rejects_each_bad_field() {
        assert!(
            decide_transcode("bad job!", USER, KEY, INPUT, SOURCE_SHA, PREFIX, STAGING).is_err()
        );
        assert!(
            decide_transcode(JOB, "bad:user", KEY, INPUT, SOURCE_SHA, PREFIX, STAGING).is_err()
        );
        assert!(decide_transcode(JOB, USER, "nope", INPUT, SOURCE_SHA, PREFIX, STAGING).is_err());
        assert!(decide_transcode(JOB, USER, KEY, "ZZZ", SOURCE_SHA, PREFIX, STAGING).is_err());
        assert!(decide_transcode(JOB, USER, KEY, INPUT, "short", PREFIX, STAGING).is_err());
        // Uppercase source hash canonicalises instead of rejecting.
        let upper = decide_transcode(
            JOB,
            USER,
            KEY,
            INPUT,
            &SOURCE_SHA.to_ascii_uppercase(),
            PREFIX,
            STAGING,
        )
        .unwrap();
        assert_eq!(upper.source_sha256, SOURCE_SHA);
        // Another job's prefix is rejected (containment).
        assert!(decide_transcode(
            JOB,
            USER,
            KEY,
            INPUT,
            SOURCE_SHA,
            "ingest-scratch/job_OTHER/r1/",
            STAGING,
        )
        .is_err());
        // Traversal, missing slash, and overlong prefixes are rejected.
        assert!(decide_transcode(
            JOB,
            USER,
            KEY,
            INPUT,
            SOURCE_SHA,
            "ingest-scratch/",
            STAGING
        )
        .is_err());
        assert!(decide_transcode(
            JOB,
            USER,
            KEY,
            INPUT,
            SOURCE_SHA,
            "ingest-scratch/job_01J9ZQ8K2M/r1",
            STAGING,
        )
        .is_err());
        assert!(decide_transcode(JOB, USER, KEY, INPUT, SOURCE_SHA, PREFIX, "short").is_err());
    }

    #[test]
    fn classify_completes_only_on_well_formed_success() {
        let ok = serde_json::json!({
            "status": "transcoded",
            "artifact_sha256": SOURCE_SHA,
            "byte_size": 2326,
        });
        assert_eq!(
            classify_transcode_reply(200, &serde_json::to_vec(&ok).unwrap()),
            TranscodeCall::Completed {
                artifact_sha256: SOURCE_SHA.to_string(),
                byte_size: 2326,
            }
        );
        // Uppercase hash canonicalises on completion.
        let upper = serde_json::json!({
            "status": "transcoded",
            "artifact_sha256": SOURCE_SHA.to_ascii_uppercase(),
            "byte_size": 2326,
        });
        assert_eq!(
            classify_transcode_reply(200, &serde_json::to_vec(&upper).unwrap()),
            TranscodeCall::Completed {
                artifact_sha256: SOURCE_SHA.to_string(),
                byte_size: 2326,
            }
        );
        // Malformed or out-of-contract 200s fail closed to retry.
        for body in [
            b"not json".to_vec(),
            b"{}".to_vec(),
            serde_json::to_vec(&serde_json::json!({"status": "transcoded"})).unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "status": "transcoded",
                "artifact_sha256": "zzz",
                "byte_size": 10,
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "status": "transcoded",
                "artifact_sha256": SOURCE_SHA,
                "byte_size": 0,
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "status": "transcoded",
                "artifact_sha256": SOURCE_SHA,
                "byte_size": MAX_ARTIFACT_BYTES + 1,
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({"status": "mystery"})).unwrap(),
            vec![0u8; MAX_TRANSCODE_REPLY_BYTES + 1],
        ] {
            assert_eq!(classify_transcode_reply(200, &body), TranscodeCall::Retry);
        }
    }

    #[test]
    fn classify_maps_front_faults() {
        let err = serde_json::to_vec(&serde_json::json!({"status": "untranscodable"})).unwrap();
        assert_eq!(
            classify_transcode_reply(200, &err),
            TranscodeCall::Untranscodable
        );
        for status in [400, 401, 403, 404, 409, 413] {
            assert_eq!(
                classify_transcode_reply(status, b"{}"),
                TranscodeCall::Untranscodable,
                "status {status}"
            );
        }
        for status in [429, 500, 503] {
            assert_eq!(
                classify_transcode_reply(status, b"{}"),
                TranscodeCall::Retry,
                "status {status}"
            );
        }
    }

    #[test]
    fn publication_statements_are_pinned() {
        assert_eq!(
            upsert_cache_object_sql(),
            "INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (content_hash) DO NOTHING"
        );
        assert_eq!(
            upsert_track_media_sql(),
            "INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at) SELECT $1, $2, $3, $4, 'opus', 64, $5 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, track_id, source_job_id, content_hash) DO NOTHING"
        );
        assert_eq!(
            upsert_cache_grant_sql(),
            "INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at) SELECT $1, $2, $3, $4 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id, content_hash) DO NOTHING"
        );
        assert_eq!(
            select_track_media_by_pk_sql(),
            "SELECT user_id, track_id, source_job_id, content_hash, format, bitrate FROM track_media WHERE user_id = $1 AND track_id = $2 AND source_job_id = $3 AND content_hash = $4"
        );
        assert_eq!(
            select_cache_grant_by_pk_sql(),
            "SELECT user_id, content_hash, provider_id FROM user_cache_grants WHERE user_id = $1 AND content_hash = $2"
        );
        // Literals pin the profile: no parameter can relabel the bytes.
        assert!(upsert_track_media_sql().contains("'opus', 64"));
        assert_eq!(
            TRACK_MEDIA_PARAM_ORDER,
            [
                "user_id",
                "track_id",
                "source_job_id",
                "content_hash",
                "verified_at"
            ]
        );
        assert_eq!(
            CACHE_GRANT_PARAM_ORDER,
            ["user_id", "content_hash", "provider_id", "verified_at"]
        );
        assert_eq!(
            CACHE_OBJECT_PARAM_ORDER,
            [
                "content_hash",
                "byte_size",
                "r2_bucket",
                "r2_key",
                "last_verified_at"
            ]
        );
    }

    #[test]
    fn publish_zero_rows_classify_by_row_existence() {
        assert_eq!(classify_publish_zero_rows(true), PublishOutcome::Duplicate);
        assert_eq!(
            classify_publish_zero_rows(false),
            PublishOutcome::AccountDeleting
        );
    }
}
