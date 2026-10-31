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
//! idempotent statement each, all converging on redelivery. This step
//! publishes the content-global row only; the two user rows stay
//! pinned here but unwired until the tracks/providers writers land
//! (a follow-up wires them, with the §8 proof, once their FK targets
//! exist — publishing them today would fail closed on every job):
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
//! Runtime: the `Transcoding` step (`run_transcode_step` in
//! `queue_consumer.rs`) dispatches one recorded input per delivery through
//! [`call_transcode`], fetches `out/artifact.ogg` from scratch,
//! verifies its SHA-256, puts the bytes to the audio bucket, and
//! publishes the [`upsert_cache_object_sql`] row — then advances the
//! cursor, or to `Writing` past the last input. Enablement outside
//! this repo slice: the `TRANSCODE_CONTAINER` service binding, the
//! `AUDIO_PUBLISH` audio-bucket binding plus the `AUDIO_BUCKET_NAME`
//! var (a deliberate, documented exception to the "never bind the
//! audio-cache buckets here" rule in `wrangler.toml.example`, which
//! predates the e2e spec's §4.2 publication assignment), and the
//! deployed Container image + front Worker (both live in-repo under
//! `containers/transcode` and `workers/transcode`; deploy itself is
//! an operator action). Missing-table SQLSTATEs map through
//! `crate::library::is_deterministic_sqlstate` to permanent
//! failures, so jobs quarantine at once instead of burning budget.
//! Grant publication (the follow-up, not this step) carries the
//! standing threat-model §8 gate with it.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use worker::{wasm_bindgen::JsValue, Delay, Env, Headers, Method, Request, RequestInit};

use futures_util::StreamExt;

use nntune_core::par2::scratch::{sha256_hex, validate_staging_id};
use nntune_core::transcode::{canonical_sha256_hex, validate_transcode_input_key};

use crate::dispatch::validate_user_id;
use crate::jobs::validate_job_id;
use crate::parse::ParsedFile;
use crate::verify::{StagedFile, VerifyProgress};

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
/// R2 binding carrying artifact publication: the environment's audio
/// bucket (wrangler `[[r2_buckets]]`). A deliberate, documented
/// exception to the "never bind the audio-cache buckets here" rule in
/// `wrangler.toml.example`, assigned by the e2e spec's §4.2
/// publication step; the bucket name rides
/// [`AUDIO_BUCKET_NAME_VAR`].
pub const AUDIO_PUBLISH_BINDING: &str = "AUDIO_PUBLISH";
/// Worker var (or secret) naming the audio bucket behind
/// [`AUDIO_PUBLISH_BINDING`] (`nntunev2-audio-{staging,prod}`), bound
/// into the [`upsert_cache_object_sql`] row's `r2_bucket` column.
pub const AUDIO_BUCKET_NAME_VAR: &str = "AUDIO_BUCKET_NAME";
/// The front replies with a small status document
/// (`{status, artifact_sha256, byte_size}`; the artifact bytes stay in
/// `out/artifact.ogg` in scratch for ingest to fetch), so anything
/// larger is not a transcode reply we can trust.
pub const MAX_TRANSCODE_REPLY_BYTES: usize = 64 << 10;
/// Largest plausible `opus-64` artifact: 64 kbps over a 60-minute
/// track is ~28.8 MB, plus Ogg framing. Anything larger is not this
/// profile's output and is refused rather than published.
pub const MAX_ARTIFACT_BYTES: u64 = 32 << 20;
/// Idempotency keys are `ingest:{user}:{sha}:{ref}` with bounded
/// fields (see `jobs::derive_idempotency_key`); 768 covers the
/// longest constructible key with headroom.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 768;

/// One verified audio file to encode, mirrored field-for-field by the
/// transcode front's contract. `input_key` is the verify-staged
/// object name (`{index:04}-{sha256(file_name)}`) under
/// `{scratch_prefix}in/`; `source_sha256` is the verified bytes'
/// hash (the content identity the front memoises on);
/// `staging_id` binds the profile, job, verify-round scratch prefix,
/// input key and verified content hash (see [`derive_transcode_staging`]).
/// It identifies one file's encode generation, not the full manifest;
/// another job, round, key or source cannot replay its artifact.
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

/// True only for the round dir the job's verify staging lives in.
/// The rule is shared with the transcode front parse
/// (`nntune_core::transcode::validate_transcode_prefix`) so dispatch
/// and front can never disagree on job containment.
pub fn validate_transcode_prefix(prefix: &str, job_id: &str) -> bool {
    nntune_core::transcode::validate_transcode_prefix(prefix, job_id)
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
    if !validate_transcode_input_key(input_key) {
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

/// Fold one more reply chunk into the buffer, enforcing `cap` on the running
/// total. Pure so the streaming policy is unit-tested without a service
/// binding: the chunk that would cross the cap is refused and the capped
/// prefix stays intact, never a truncated body handed to the classifier
/// afterwards.
fn push_transcode_chunk(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if buf.len().saturating_add(chunk.len()) > cap {
        return false;
    }
    buf.extend_from_slice(chunk);
    true
}

/// Drain a reply chunk stream within `cap`. A chunk-level transport error
/// fails closed to `Retry`, and the chunk that would cross the ceiling fails
/// closed to `Retry` from its length: the remainder is never retained and no
/// truncated prefix escapes for classification.
async fn drain_transcode_stream<S, C, E>(
    mut stream: S,
    cap: usize,
) -> Result<Vec<u8>, TranscodeCall>
where
    S: futures_util::Stream<Item = Result<C, E>> + Unpin,
    C: AsRef<[u8]>,
{
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => return Err(TranscodeCall::Retry),
        };
        if !push_transcode_chunk(&mut buf, chunk.as_ref(), cap) {
            return Err(TranscodeCall::Retry);
        }
    }
    Ok(buf)
}

/// Dispatch one transcode request through the service binding. Transport
/// or binding failures are `Retry`, never a guessed terminal outcome.
///
/// The reply body streams within `MAX_TRANSCODE_REPLY_BYTES`: the chunk that
/// would cross the ceiling returns `Retry` from the length. The remainder
/// is never retained and no truncated prefix reaches
/// `classify_transcode_reply`.
pub async fn call_transcode(env: &Env, req: &TranscodeRequest) -> TranscodeCall {
    call_transcode_with_deadline(env, req, TRANSCODE_CALL_DEADLINE_MS).await
}

/// Settle headers and the capped reply under one owned, cancellable deadline.
/// The same deadline is polled first at both stages; body progress never
/// replenishes it. Ownership releases the timer on every return (including
/// transport failure). Local settlement does not prove remote cancellation.
/// Generic chunks let the runtime retain its zero-copy HTTP body adapter.
pub async fn settle_transcode_call<F, S, D, C, E>(fetch: F, mut deadline: D) -> TranscodeCall
where
    F: Future<Output = Result<(u16, S), ()>> + Unpin,
    S: futures_util::Stream<Item = Result<C, E>> + Unpin,
    C: AsRef<[u8]>,
    D: Future<Output = ()> + Unpin,
{
    use crate::pool_deadline::race_against_deadline;

    let (status, stream) = match race_against_deadline(fetch, &mut deadline).await {
        Some(Ok(reply)) => reply,
        Some(Err(())) | None => return TranscodeCall::Retry,
    };
    let drain = Box::pin(drain_transcode_stream(stream, MAX_TRANSCODE_REPLY_BYTES));
    let body = match race_against_deadline(drain, &mut deadline).await {
        Some(Ok(body)) => body,
        Some(Err(_)) | None => return TranscodeCall::Retry,
    };
    classify_transcode_reply(status, &body)
}

/// Dispatch with an injectable total headers-plus-body budget in milliseconds.
pub async fn call_transcode_with_deadline(
    env: &Env,
    req: &TranscodeRequest,
    deadline_ms: u64,
) -> TranscodeCall {
    let Ok(body) = serde_json::to_string(req) else {
        return TranscodeCall::Retry;
    };
    let fetcher = match env.service(TRANSCODE_SERVICE_BINDING) {
        Ok(fetcher) => fetcher,
        Err(_) => {
            crate::emit(
                nntune_logging::Level::Error,
                &nntune_logging::resolve_trace_id(None),
                "ingest.transcode.binding_missing",
                serde_json::json!({
                    "job_id": req.job_id,
                    "build_sha": crate::build_sha_from_env(env),
                }),
            );
            return TranscodeCall::Retry;
        }
    };
    let headers = Headers::new();
    if headers.set("content-type", "application/json").is_err() {
        return TranscodeCall::Retry;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body)));
    let request = match Request::new_with_init(TRANSCODE_URL, &init) {
        Ok(request) => request,
        Err(_) => return TranscodeCall::Retry,
    };
    // Delay registers on first poll: the settlement polls it before the
    // lazy fetch, then reuses it through the capped HTTP body drain. Dropping
    // the owned Delay cancels its local timer, not necessarily remote work.
    let deadline = Box::pin(Delay::from(Duration::from_millis(deadline_ms)));
    let fetch = Box::pin(async {
        let response = fetcher.fetch_request(request).await.map_err(|_| {
            crate::emit(
                nntune_logging::Level::Error,
                &nntune_logging::resolve_trace_id(None),
                "ingest.transcode.call_failed",
                serde_json::json!({
                    "job_id": req.job_id,
                    "build_sha": crate::build_sha_from_env(env),
                }),
            );
        })?;
        Ok((response.status().as_u16(), response.into_body()))
    });
    settle_transcode_call(fetch, deadline).await
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

/// Classify a [`upsert_cache_object_sql`] write by its affected-row
/// count. `ON CONFLICT DO NOTHING` yields at most one row: 1 is the
/// first writer's publication, 0 is a redelivery converging on it
/// (same hash means same bytes, so there is no third case — the
/// statement carries no fence to refuse through).
pub const fn classify_cache_object_rows(affected: u64) -> PublishOutcome {
    if affected == 1 {
        PublishOutcome::Published
    } else {
        PublishOutcome::Duplicate
    }
}

/// One verified audio input awaiting `Transcoding`, recorded on the
/// checkpoint by `Verifying` on `Complete`. `key` is the full staged
/// object key; `sha256` the canonical content hash; `size` the
/// staged byte count; `scratch_prefix` the round dir the key lives
/// under (recomputed rounds stage under a new dir, so the prefix is
/// recorded per input, never re-derived at encode time).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TranscodeInput {
    pub file_index: usize,
    pub key: String,
    pub sha256: String,
    pub size: u64,
    pub scratch_prefix: String,
}

/// Audio containers the profile accepts, matched case-insensitively
/// against the extension after the final dot. Everything here is an
/// ffmpeg-decodable audio shape (`-map 0:a:0` selects the first audio
/// stream out of containers that may also carry video or images);
/// anything else is never worth encoding. Pinned by tests: extending
/// the list changes which releases transcode.
pub const AUDIO_EXTENSIONS: [&str; 16] = [
    "flac", "mp3", "m4a", "mp4", "aac", "ogg", "oga", "opus", "wav", "wave", "aiff", "aif", "wma",
    "ape", "mpc", "wv",
];

/// True for parsed file names worth encoding: a non-par2 data file
/// whose extension (after the final dot) is in
/// [`AUDIO_EXTENSIONS`]. Display-name only — storage keys never
/// derive from it.
pub fn is_audio_name(name: &str) -> bool {
    if !name.contains('.') {
        return false;
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    if ext.is_empty() {
        return false;
    }
    AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Select the verified audio inputs from a completed verify round:
/// every staged, intact, single-object, non-par2 audio file, in NZB
/// file order. Pure over the parse plus the staged records so the
/// selection is unit-tested without bindings.
///
/// Skips are silent only when they are structurally not audio (par2
/// files, `.nfo`/`.sfv`/artwork). Everything else fails the job with
/// a stable reason: a staged index outside the parse, a key outside
/// the derived layout, damage on a supposedly complete round, a
/// spooled (multi-object) audio file this input cannot consume, a
/// non-content hash, or an empty audio object are all pipeline bugs
/// or corrupt progress — never something a redelivery could fix, and
/// never silently droppable (a release that encodes some tracks but
/// not others is incorrect output, not a partial success).
pub fn select_transcode_inputs(
    files: &[ParsedFile],
    staged: &BTreeMap<usize, StagedFile>,
    job_id: &str,
    round: u32,
) -> Result<Vec<TranscodeInput>, &'static str> {
    let mut inputs = Vec::new();
    for (index, rec) in staged {
        let file = files
            .get(*index)
            .ok_or("staged index outside parsed files")?;
        if file.is_par2 || !is_audio_name(&file.name) {
            continue;
        }
        if rec.key != VerifyProgress::file_key(job_id, round, *index, &file.name) {
            return Err("staged key outside derived layout");
        }
        if rec.missing || rec.missing_blocks != 0 {
            return Err("damaged file reached transcode selection");
        }
        if !rec.spool.is_empty() {
            return Err("spooled audio has no single-object encode input");
        }
        let sha256 = canonical_sha256_hex(&rec.sha256)
            .map_err(|_| "staged hash outside content identity")?;
        if rec.size == 0 {
            return Err("empty staged audio file");
        }
        inputs.push(TranscodeInput {
            file_index: *index,
            key: rec.key.clone(),
            sha256,
            size: rec.size,
            scratch_prefix: VerifyProgress::scratch_prefix(job_id, round),
        });
    }
    Ok(inputs)
}

/// Record a completed verify round's transcode inputs on the
/// checkpoint and reset the encode cursor. Pure over the checkpoint
/// plus the selection inputs; the caller (`run_verifying`, on
/// `Complete`) owns the failure mapping.
pub fn record_transcode_inputs(
    cp: &mut crate::jobs::JobCheckpoint,
    files: &[ParsedFile],
    staged: &BTreeMap<usize, StagedFile>,
    job_id: &str,
    round: u32,
) -> Result<(), &'static str> {
    cp.transcode_inputs = select_transcode_inputs(files, staged, job_id, round)?;
    cp.transcode_next = 0;
    Ok(())
}

/// Generation-bound staging id for one encode request: sha256 over
/// the profile tag, job, verify-round scratch prefix, verified content
/// hash and staged input key. Redeliveries of the same recorded input
/// replay the memoised artifact; another job or round never does, even
/// for identical bytes. All fields are validated before hashing, and
/// their alphabets exclude the `:` separator.
pub fn derive_transcode_staging(
    source_sha256_hex: &str,
    input_key: &str,
    job_id: &str,
    scratch_prefix: &str,
) -> Result<String, &'static str> {
    if !validate_job_id(job_id) {
        return Err("job id outside the pipeline alphabet");
    }
    if !validate_transcode_prefix(scratch_prefix, job_id) {
        return Err("scratch prefix outside the job round dir");
    }
    if !validate_transcode_input_key(input_key) {
        return Err("input key is not a staged object key");
    }
    let sha = canonical_sha256_hex(source_sha256_hex)
        .map_err(|_| "source sha256 must be 64 hex chars")?;
    Ok(sha256_hex(
        format!("opus-64:v2:{job_id}:{scratch_prefix}:{sha}:{input_key}").as_bytes(),
    ))
}

/// Verify fetched artifact bytes against a `Completed` reply's claims:
/// exact size plus exact SHA-256 (either case on the claimed hash).
/// Pure so the gate is unit-tested without bindings; the caller
/// fails a mismatch permanently (complete-but-wrong bytes are
/// deterministic — transport damage fails earlier, at the fetch).
pub fn verify_transcode_artifact(
    bytes: &[u8],
    artifact_sha256_hex: &str,
    byte_size: u64,
) -> Result<(), &'static str> {
    if bytes.len() as u64 != byte_size {
        return Err("artifact size mismatch");
    }
    let claimed =
        canonical_sha256_hex(artifact_sha256_hex).map_err(|_| "artifact hash mismatch")?;
    if sha256_hex(bytes) != claimed {
        return Err("artifact hash mismatch");
    }
    Ok(())
}

/// Why a scratch artifact fetch failed. Neutral (no job codes): the
/// consumer maps `Missing`/`Transport` to transient retries and
/// `Oversize` to permanent (only this pipeline writes the artifact
/// key, always within the profile ceiling, so an oversize object is
/// deterministic corruption, never a lagging write).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactFetchError {
    Missing,
    Oversize,
    Transport,
}

/// Why a [`upsert_cache_object_sql`] write failed. Neutral: the
/// consumer maps `Deterministic` (missing table, constraint
/// violation — replays identically) to permanent quarantine and
/// `Transient` to the shared attempt budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishError {
    Deterministic,
    Transient,
}

/// One [`upsert_cache_object_sql`] write's bound values, in
/// [`CACHE_OBJECT_PARAM_ORDER`]: the verified content hash, the
/// artifact byte count, the audio bucket name and content-addressed
/// key, and the verification epoch seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodePublication {
    pub content_hash: String,
    pub byte_size: u64,
    pub r2_bucket: String,
    pub r2_key: String,
    pub verified_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = "job_01J9ZQ8K2M";
    const USER: &str = "user_01";
    const KEY: &str =
        "ingest:user_01:9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734:upload";
    const INPUT: &str = "0007-9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
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
        // The bare 64-hex core-contract shape has no producer: only the
        // indexed verify-staged name dispatches.
        assert!(decide_transcode(
            JOB,
            USER,
            KEY,
            "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734",
            SOURCE_SHA,
            PREFIX,
            STAGING
        )
        .is_err());
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

    fn transcoded_body_of_len(total_len: usize) -> Vec<u8> {
        let prefix = format!(
            "{{\"status\":\"transcoded\",\"artifact_sha256\":\"{SOURCE_SHA}\",\"byte_size\":2326,\"pad\":\""
        );
        const SUFFIX: &[u8] = br#""}"#;
        assert!(
            total_len >= prefix.len() + SUFFIX.len(),
            "body must fit the transcoded envelope"
        );
        let mut body = Vec::with_capacity(total_len);
        body.extend_from_slice(prefix.as_bytes());
        body.extend_from_slice(&vec![b'x'; total_len - prefix.len() - SUFFIX.len()]);
        body.extend_from_slice(SUFFIX);
        body
    }

    #[test]
    fn transcode_chunk_fold_keeps_capped_prefix() {
        // No stream involved: chunks summing past the cap must fail while
        // only the capped prefix is buffered, never after a full read.
        let mut buf = Vec::new();
        assert!(push_transcode_chunk(
            &mut buf,
            b"hello",
            MAX_TRANSCODE_REPLY_BYTES
        ));
        assert!(push_transcode_chunk(
            &mut buf,
            b" world!",
            MAX_TRANSCODE_REPLY_BYTES
        ));
        assert_eq!(buf, b"hello world!");
        assert!(!push_transcode_chunk(
            &mut buf,
            &vec![b'x'; MAX_TRANSCODE_REPLY_BYTES],
            MAX_TRANSCODE_REPLY_BYTES
        ));
        // The failed push leaves the capped prefix intact.
        assert_eq!(buf, b"hello world!");
        // An exact fit is accepted; one byte more is not.
        let mut exact = Vec::new();
        assert!(push_transcode_chunk(
            &mut exact,
            &vec![0u8; MAX_TRANSCODE_REPLY_BYTES],
            MAX_TRANSCODE_REPLY_BYTES
        ));
        assert_eq!(exact.len(), MAX_TRANSCODE_REPLY_BYTES);
        assert!(!push_transcode_chunk(
            &mut exact,
            &[0u8; 1],
            MAX_TRANSCODE_REPLY_BYTES
        ));
        assert_eq!(exact.len(), MAX_TRANSCODE_REPLY_BYTES);
    }

    #[tokio::test]
    async fn transcode_stream_pins_exact_ceiling_across_chunks() {
        use futures_util::stream::{self};
        let body = transcoded_body_of_len(MAX_TRANSCODE_REPLY_BYTES);
        assert_eq!(body.len(), 65_536);
        // Split across chunks so the boundary is exercised by the drain,
        // not by a single whole-body buffer.
        let mid = body.len() / 2;
        let chunks = vec![
            Ok::<Vec<u8>, ()>(body[..mid].to_vec()),
            Ok(body[mid..].to_vec()),
        ];
        let drained = drain_transcode_stream(stream::iter(chunks), MAX_TRANSCODE_REPLY_BYTES)
            .await
            .expect("exact ceiling across chunks must drain");
        assert_eq!(drained.len(), MAX_TRANSCODE_REPLY_BYTES);
        assert_eq!(
            classify_transcode_reply(200, &drained),
            TranscodeCall::Completed {
                artifact_sha256: SOURCE_SHA.to_string(),
                byte_size: 2326,
            },
            "a well-formed transcoded reply at exactly 65,536 bytes must stay Completed"
        );
    }

    #[tokio::test]
    async fn transcode_stream_rejects_ceiling_plus_one_from_length() {
        use futures_util::stream::{self};
        let body = transcoded_body_of_len(MAX_TRANSCODE_REPLY_BYTES + 1);
        assert_eq!(body.len(), 65_537);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("body must stay well-formed JSON");
        assert_eq!(
            value.get("status").and_then(|s| s.as_str()),
            Some("transcoded"),
            "body must keep status=transcoded so only the byte ceiling flips the verdict"
        );
        let mid = body.len() / 2;
        let chunks = vec![
            Ok::<Vec<u8>, ()>(body[..mid].to_vec()),
            Ok(body[mid..].to_vec()),
        ];
        // The drain itself must refuse the 65,537th byte: asserting only the
        // classifier afterwards would stay green even if the loop cap were
        // raised, because the classifier also rejects over-ceiling bodies.
        assert_eq!(
            drain_transcode_stream(stream::iter(chunks), MAX_TRANSCODE_REPLY_BYTES).await,
            Err(TranscodeCall::Retry),
            "the 65,537th byte must fail closed from its length without retaining it"
        );
    }

    #[tokio::test]
    async fn transcode_stream_chunk_error_fails_closed() {
        use futures_util::stream::{self};
        let chunks = vec![
            Ok::<Vec<u8>, ()>(br#"{"status":"transcoded"}"#.to_vec()),
            Err(()),
        ];
        // A stream that yields a complete chunk and then errors must not fall
        // through to classify the partial body as Completed.
        assert_eq!(
            drain_transcode_stream(stream::iter(chunks), MAX_TRANSCODE_REPLY_BYTES).await,
            Err(TranscodeCall::Retry),
            "a chunk error after a complete prefix must fail closed to Retry"
        );
    }

    #[test]
    fn transcode_request_serializes_for_the_front_post() {
        let req = decide().unwrap();
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["input_key"], INPUT);
        assert_eq!(body["staging_id"], STAGING);
        assert_eq!(TRANSCODE_ENDPOINT, "/transcode");
        assert_eq!(TRANSCODE_SERVICE_BINDING, "TRANSCODE_CONTAINER");
    }

    #[test]
    fn audio_names_match_the_pinned_extension_list() {
        for name in [
            "track01.flac",
            "track02.MP3",
            "a.m4a",
            "b.mp4",
            "c.aac",
            "d.ogg",
            "e.oga",
            "f.opus",
            "g.wav",
            "h.WAVE",
            "i.aiff",
            "j.aif",
            "k.wma",
            "l.ape",
            "m.mpc",
            "n.wv",
            "deep.name.with.dots.flac",
        ] {
            assert!(is_audio_name(name), "{name} must be audio");
        }
        for name in [
            "track01.nfo",
            "track01.sfv",
            "cover.jpg",
            "set.par2",
            "noextension",
            "trailing-dot.",
            ".",
            "",
        ] {
            assert!(!is_audio_name(name), "{name} must not be audio");
        }
        // The allowlist is closed: every entry is exercised above, and
        // the count pins the list against silent extension.
        assert_eq!(AUDIO_EXTENSIONS.len(), 16);
    }

    fn parsed_file(name: &str, is_par2: bool) -> ParsedFile {
        ParsedFile {
            name: name.to_string(),
            is_par2,
            segment_ids: Vec::new(),
            segment_bytes: Vec::new(),
        }
    }

    fn staged_audio(key: String, sha256: &str, size: u64) -> StagedFile {
        StagedFile {
            key,
            size,
            sha256: sha256.to_string(),
            missing_blocks: 0,
            size_refused_blocks: 0,
            par2_set_id: None,
            spool: Vec::new(),
            crc32: 0,
            missing: false,
        }
    }

    #[test]
    fn selection_picks_intact_audio_in_file_order() {
        const SHA: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
        let files = vec![
            parsed_file("track02.mp3", false),
            parsed_file("release.nfo", false),
            parsed_file("track01.flac", false),
            parsed_file("set.par2", true),
        ];
        let staged: BTreeMap<usize, StagedFile> = [0, 1, 2, 3]
            .into_iter()
            .map(|index| {
                (
                    index,
                    staged_audio(
                        VerifyProgress::file_key(JOB, 1, index, &files[index].name),
                        SHA,
                        512,
                    ),
                )
            })
            .collect();
        let inputs = select_transcode_inputs(&files, &staged, JOB, 1).unwrap();
        // Audio only, in NZB file order (index 0 before 2); the .nfo
        // and the par2 row skip silently.
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].file_index, 0);
        assert_eq!(inputs[1].file_index, 2);
        assert_eq!(inputs[0].sha256, SHA);
        assert_eq!(inputs[0].size, 512);
        assert_eq!(inputs[0].scratch_prefix, PREFIX);
        assert_eq!(
            inputs[0].key,
            VerifyProgress::file_key(JOB, 1, 0, "track02.mp3")
        );
    }

    #[test]
    fn selection_fails_each_corrupt_shape() {
        const SHA: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
        let files = vec![parsed_file("track01.flac", false)];
        let staged_one = |rec: StagedFile| BTreeMap::from([(0usize, rec)]);
        let key = VerifyProgress::file_key(JOB, 1, 0, "track01.flac");
        // Index outside the parse.
        let mut staged = staged_one(staged_audio(key.clone(), SHA, 512));
        staged.insert(7, staged_audio(key.clone(), SHA, 512));
        assert!(select_transcode_inputs(&files, &staged, JOB, 1).is_err());
        // Key outside the derived layout.
        assert!(select_transcode_inputs(
            &files,
            &staged_one(staged_audio(
                "ingest-scratch/job_01J9ZQ8K2M/r1/in/0000-evil".to_string(),
                SHA,
                512
            )),
            JOB,
            1
        )
        .is_err());
        // Damage on a supposedly complete round.
        let mut damaged = staged_audio(key.clone(), SHA, 512);
        damaged.missing_blocks = 2;
        assert!(select_transcode_inputs(&files, &staged_one(damaged), JOB, 1).is_err());
        // Spooled audio has no single-object input (never silently
        // dropped: a release that encodes some tracks but not others
        // is incorrect output).
        let mut spooled = staged_audio(key.clone(), SHA, 512);
        spooled.spool = vec![crate::verify::StagedSpoolSegment {
            key: "seg".to_string(),
            size: 1,
            crc32: 0,
            missing: false,
            size_refused: false,
        }];
        assert!(select_transcode_inputs(&files, &staged_one(spooled), JOB, 1).is_err());
        // Non-content hash and empty audio.
        assert!(select_transcode_inputs(
            &files,
            &staged_one(staged_audio(key.clone(), "xyz", 512)),
            JOB,
            1
        )
        .is_err());
        assert!(select_transcode_inputs(
            &files,
            &staged_one(staged_audio(key.clone(), SHA, 0)),
            JOB,
            1
        )
        .is_err());
    }

    #[test]
    fn selection_without_audio_selects_nothing() {
        let files = vec![
            parsed_file("release.nfo", false),
            parsed_file("set.par2", true),
        ];
        let staged: BTreeMap<usize, StagedFile> = [0, 1]
            .into_iter()
            .map(|index| {
                (
                    index,
                    staged_audio(
                        VerifyProgress::file_key(JOB, 1, index, &files[index].name),
                        "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734",
                        512,
                    ),
                )
            })
            .collect();
        // Empty is a valid selection: the step fails it permanently
        // with "no verified audio", naming the input instead of the
        // verify that recorded it.
        assert!(select_transcode_inputs(&files, &staged, JOB, 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn record_stores_inputs_and_resets_the_cursor() {
        use crate::jobs::{JobCheckpoint, JobSource, JobState};

        const SHA: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
        let files = vec![parsed_file("track01.flac", false)];
        let staged = BTreeMap::from([(
            0usize,
            staged_audio(
                VerifyProgress::file_key(JOB, 2, 0, "track01.flac"),
                SHA,
                512,
            ),
        )]);
        // Serde-built so a future defaulted checkpoint field compiles
        // without touching this fake: unknown fields are ignored now and
        // defaulted then. Values mirror the previous literal exactly.
        let mut cp: JobCheckpoint = serde_json::from_value(serde_json::json!({
            "job_id": JOB,
            "user_id": USER,
            "idempotency_key": KEY,
            "source": JobSource::NzbUpload,
            "state": JobState::Verifying,
            "attempt": 1,
            "repair_rounds": 1,
            "probe_tally": null,
            "par2_set_id": null,
            "missing_blocks": 0,
            "failure_code": null,
            "failure_reason": null,
            "provider_id": null,
            "transcode_inputs": [],
            "transcode_next": 7,
            // Future track-identity map: ignored until the field lands,
            // then deserializes to its empty default.
            "track_ids": null,
        }))
        .expect("test checkpoint shape must deserialize");
        record_transcode_inputs(&mut cp, &files, &staged, JOB, 2).unwrap();
        assert_eq!(cp.transcode_inputs.len(), 1);
        assert_eq!(cp.transcode_next, 0);
        // The re-verify round's own prefix is recorded, never the
        // previous round's: inputs always address their own staging.
        assert_eq!(
            cp.transcode_inputs[0].scratch_prefix,
            "ingest-scratch/job_01J9ZQ8K2M/r2/"
        );
    }

    #[test]
    fn staging_id_is_deterministic_and_content_sensitive() {
        const SHA: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
        let first = derive_transcode_staging(SHA, INPUT, JOB, PREFIX).unwrap();
        // Independent SHA-256 fixture for the specified v2 domain and scope.
        assert_eq!(
            first,
            "64e7ca9681a8c87e6d2acc5629774dcdfd27387c9b64b71d81273e075f923a9e"
        );
        assert_eq!(
            derive_transcode_staging(SHA, INPUT, JOB, PREFIX).unwrap(),
            first
        );
        assert_eq!(
            derive_transcode_staging(&SHA.to_ascii_uppercase(), INPUT, JOB, PREFIX).unwrap(),
            first
        );
        assert_ne!(
            derive_transcode_staging(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                INPUT,
                JOB,
                PREFIX,
            )
            .unwrap(),
            first
        );
        assert_ne!(
            derive_transcode_staging(
                SHA,
                "0008-9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734",
                JOB,
                PREFIX,
            )
            .unwrap(),
            first
        );
    }

    #[test]
    fn staging_id_separates_jobs_with_valid_matching_prefixes_and_rounds() {
        const SHA: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
        let first = derive_transcode_staging(SHA, INPUT, JOB, PREFIX).unwrap();
        let other_job =
            derive_transcode_staging(SHA, INPUT, "job_OTHER", "ingest-scratch/job_OTHER/r1/")
                .unwrap();
        assert_eq!(
            other_job,
            "1dde7b664004ae89588f5a23862a6c0bb5dbd4d9d6b1a25ecec95487bee1bee2"
        );
        let other_round =
            derive_transcode_staging(SHA, INPUT, JOB, "ingest-scratch/job_01J9ZQ8K2M/r2/").unwrap();
        assert_eq!(
            other_round,
            "e1e734e2df252e228387255e6008ff79cf1b6c84d2d66b1c29d92827bd991cde"
        );
        assert_ne!(first, other_job);
        assert_ne!(first, other_round);
        assert_ne!(other_job, other_round);
    }

    #[test]
    fn staging_id_rejects_invalid_source_key_job_and_prefix() {
        for sha in ["", "short", &"z".repeat(64)] {
            assert!(derive_transcode_staging(sha, INPUT, JOB, PREFIX).is_err());
        }
        for key in ["", "../input", "0007-short", "0007/invalid", STAGING] {
            assert!(
                derive_transcode_staging(SOURCE_SHA, key, JOB, PREFIX).is_err(),
                "key {key}"
            );
        }
        for job in ["", "bad job!", "../job", "job/other"] {
            assert!(
                derive_transcode_staging(SOURCE_SHA, INPUT, job, PREFIX).is_err(),
                "job {job}"
            );
        }
        for prefix in [
            "",
            "ingest-scratch/",
            "ingest-scratch/job_OTHER/r1/",
            "ingest-scratch/job_01J9ZQ8K2M/r1",
            "ingest-scratch/job_01J9ZQ8K2M/r1:/",
            "ingest-scratch/job_01J9ZQ8K2M/r1 /",
        ] {
            assert!(
                derive_transcode_staging(SOURCE_SHA, INPUT, JOB, prefix).is_err(),
                "prefix {prefix}"
            );
        }
    }

    #[test]
    fn queue_derives_staging_from_the_recorded_job_and_verify_round() {
        let source = include_str!("queue_consumer.rs");
        let start = source.find("async fn run_transcode_step_with<").unwrap();
        let function = &source[start..];
        let end = function[10..]
            .find("async fn ")
            .map(|offset| offset + 10)
            .unwrap_or(function.len());
        let function = function[..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            function.contains("crate::transcode::derive_transcode_staging( &input.sha256, input_key, &msg.job_id, &input.scratch_prefix, )"),
            "queue caller must derive from the recorded job and scratch prefix, not a reconstructed default round"
        );
    }

    #[test]
    fn stored_marker_fault_retries_but_request_size_fault_is_permanent() {
        assert_eq!(
            classify_transcode_reply(500, br#"{"error":"transcode_unavailable"}"#),
            TranscodeCall::Retry,
        );
        assert_eq!(
            classify_transcode_reply(413, br#"{"error":"payload_too_large"}"#),
            TranscodeCall::Untranscodable,
        );
    }

    #[test]
    fn artifact_verification_checks_size_then_hash() {
        use nntune_core::par2::scratch::sha256_hex;

        let bytes = b"artifact-bytes";
        let sha = sha256_hex(bytes);
        assert!(verify_transcode_artifact(bytes, &sha, bytes.len() as u64).is_ok());
        assert!(
            verify_transcode_artifact(bytes, &sha.to_ascii_uppercase(), bytes.len() as u64).is_ok()
        );
        assert_eq!(
            verify_transcode_artifact(bytes, &sha, bytes.len() as u64 + 1),
            Err("artifact size mismatch")
        );
        assert_eq!(
            verify_transcode_artifact(
                bytes,
                "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734",
                bytes.len() as u64
            ),
            Err("artifact hash mismatch")
        );
        assert_eq!(
            verify_transcode_artifact(bytes, "xyz", bytes.len() as u64),
            Err("artifact hash mismatch")
        );
    }

    #[test]
    fn cache_object_rows_classify_first_writer_vs_replay() {
        assert_eq!(classify_cache_object_rows(1), PublishOutcome::Published);
        assert_eq!(classify_cache_object_rows(0), PublishOutcome::Duplicate);
    }
}
