//! Per-message Queue delivery: validated `event(queue)` consumer.
//!
//! Settlement is decided by [`crate::retry::decide_checkpoint`] (the retry
//! budget + DLQ contract): this module validates each message against its
//! deterministic job identity, loads or creates the durable checkpoint,
//! runs the one executable step (upload payload digest + shared-parser
//! validation), and maps the decision onto ack / delayed retry / DLQ
//! quarantine per message.
//!
//! **One checkpoint write per delivery.** Workers KV allows 1 write per
//! second to the same key and returns 429 otherwise, so every state
//! transition of a delivery is applied to the in-memory checkpoint and
//! persisted exactly once (see [`plan_settlement`]) just before the
//! message is settled. The admission route is the key's only other
//! writer and its write can land less than a second before the first
//! delivery, so a rejected persist waits out the 1 s window and retries
//! that same write once in-invocation (see [`persist`]); only a second
//! failure retries the message without acking, so no attempt increment
//! or terminal state is ever lost silently.
//!
//! Terminal replay: a `Done` checkpoint acks without work; a `Failed`
//! checkpoint re-quarantines the existing failure to the DLQ (this is how
//! a DLQ-send failure redelivery retries the send). A *fresh* admission
//! of a failed job is restarted at the route instead
//! ([`crate::checkpoint::admission_plan`]), never here, so redeliveries
//! of one enqueue cannot resurrect it.
//!
//! Match and write are wired here (MB search via the `MB_API` service
//! binding, the library upsert via `LIBRARY_DB`, one meter event per
//! transition; see [`run_match_write`]). Enablement is additionally gated
//! on the library schema: no migration creates the upsert's `library`
//! table yet (see [`crate::library`]). No live runtime boundary is
//! claimed for the rest: indexer search, pool probe, verify and repair
//! steps have no executable consumer wiring yet, so those states settle
//! as transient failures through the shared retry budget. The consumer
//! therefore stays disabled in `wrangler.toml.example` until those
//! boundaries land.

use futures_util::StreamExt;
use worker::{console_log, Env, MessageExt, QueueRetryOptionsBuilder};

use crate::dispatch::{
    content_sha256_hex, make_job_id, validate_user_id, PAYLOAD_KEY_PREFIX, PAYLOAD_KV_BINDING,
};
use crate::jobs::{
    advance_checkpoint, validate_job_id, JobCheckpoint, JobSource, JobState, MAX_ATTEMPTS,
    MAX_REPAIR_ROUNDS,
};
use crate::newznab::{MAX_CATEGORIES, MAX_LIMIT, MAX_PAGE};
use crate::queue::{validate_indexer_ref, QueueMsg};
use crate::retry::{
    decide_checkpoint, Disposition, DlqEntry, JobError, DLQ_QUEUE_BINDING, ERR_MATCH_FAILED,
    ERR_PARSE_FAILED, ERR_PAYLOAD_MISSING, ERR_PROBE_FAILED, ERR_REPAIR_FAILED, ERR_SEARCH_FAILED,
    ERR_VERIFY_FAILED, ERR_WRITE_FAILED,
};

/// Payload reads use KV's minimum `cacheTtl` (30 s) so an edge-cached miss
/// expires before the later payload-lag retries (see `dispatch`).
const PAYLOAD_CACHE_TTL_SECS: u64 = 30;
/// Delay for settlements that could not be persisted or quarantined: the
/// next delivery redoes the (idempotent) step with the checkpoint as-is.
const UNPERSISTED_RETRY_SECS: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Settlement {
    Ack,
    RetryAfter(u32),
    Quarantine(Box<DlqEntry>),
}

fn validate_queue_msg(msg: &QueueMsg) -> Result<(), &'static str> {
    if !validate_job_id(&msg.job_id) || !validate_user_id(&msg.user_id) {
        return Err("invalid job or user id");
    }
    if msg.state != JobState::Queued || msg.attempt != 1 || msg.repair_rounds != 0 {
        return Err("invalid initial queue state");
    }

    match msg.source {
        JobSource::IndexerSearch => {
            let query = msg.query.as_deref().ok_or("missing search query")?;
            let indexer_ref = msg.indexer_ref.as_deref().ok_or("missing indexer handle")?;
            let limit = msg.limit.ok_or("missing search limit")?;
            let page = msg.page.ok_or("missing search page")?;
            if msg.payload_ref.is_some()
                || query.trim().is_empty()
                || !validate_indexer_ref(indexer_ref)
                || limit == 0
                || limit > MAX_LIMIT
                || page > MAX_PAGE
                || msg.categories.len() > MAX_CATEGORIES
            {
                return Err("invalid search input");
            }
            let expected = crate::dispatch::build_search_dispatch(
                &msg.user_id,
                query,
                indexer_ref,
                &msg.categories,
                limit,
                page,
            )
            .map_err(|_| "invalid search input")?;
            if expected != *msg {
                return Err("search fields do not match job identity");
            }
        }
        JobSource::NzbUpload => {
            let payload_ref = msg.payload_ref.as_deref().ok_or("missing payload handle")?;
            let sha = payload_ref
                .strip_prefix(PAYLOAD_KEY_PREFIX)
                .ok_or("invalid payload handle")?;
            if sha.len() != 64
                || !sha.bytes().all(|b| b.is_ascii_hexdigit())
                || sha != sha.to_ascii_lowercase()
                || msg.query.is_some()
                || msg.indexer_ref.is_some()
                || !msg.categories.is_empty()
                || msg.limit.is_some()
                || msg.page.is_some()
            {
                return Err("invalid upload input");
            }
            let key = crate::jobs::derive_idempotency_key(&msg.user_id, sha, "upload")
                .map_err(|_| "invalid upload identity")?;
            if msg.idempotency_key != key || msg.job_id != make_job_id(&msg.user_id, sha, "upload")
            {
                return Err("upload fields do not match job identity");
            }
        }
    }
    Ok(())
}

fn checkpoint_matches(msg: &QueueMsg, cp: &JobCheckpoint) -> bool {
    cp.job_id == msg.job_id
        && cp.user_id == msg.user_id
        && cp.idempotency_key == msg.idempotency_key
        && cp.source == msg.source
        && (1..=MAX_ATTEMPTS).contains(&cp.attempt)
        && cp.repair_rounds <= MAX_REPAIR_ROUNDS
}

/// Machine-readable error code for a state whose runtime boundary is not
/// executable from the consumer yet.
fn unavailable_code(state: JobState) -> &'static str {
    match state {
        JobState::Searching => ERR_SEARCH_FAILED,
        JobState::Parsing => ERR_PARSE_FAILED,
        JobState::Verifying => ERR_VERIFY_FAILED,
        JobState::Repairing => ERR_REPAIR_FAILED,
        JobState::Matching => ERR_MATCH_FAILED,
        JobState::Writing => ERR_WRITE_FAILED,
        _ => ERR_PROBE_FAILED,
    }
}

/// Run the executable part of one delivery, mutating `cp` in memory only.
///
/// Uploads in `Parsing` fetch and digest-check the payload, run the intake
/// guard and the shared parser, and advance to `Probing` on success — then
/// still report the (not yet wired) probe boundary as a transient failure,
/// so the delivery settles through the shared retry budget instead of
/// claiming progress it cannot finish. `Matching`/`Writing` run for real
/// ([`run_match_write`]). Every other non-terminal state is a transient
/// boundary failure today.
async fn run_step(env: &Env, msg: &QueueMsg, cp: &mut JobCheckpoint) -> Result<(), JobError> {
    if cp.state == JobState::Queued {
        advance_checkpoint(cp, false);
    }
    if matches!(cp.state, JobState::Matching | JobState::Writing) {
        return run_match_write(env, msg, cp).await;
    }
    if !(cp.state == JobState::Parsing && cp.source == JobSource::NzbUpload) {
        return Err(JobError::transient(
            unavailable_code(cp.state),
            "ingest runtime boundary unavailable",
        ));
    }
    let Some(payload_ref) = msg.payload_ref.as_deref() else {
        return Err(JobError::permanent(
            ERR_PAYLOAD_MISSING,
            "upload payload handle missing",
        ));
    };
    let payload = match env.kv(PAYLOAD_KV_BINDING) {
        Ok(store) => match store
            .get(payload_ref)
            .cache_ttl(PAYLOAD_CACHE_TTL_SECS)
            .text()
            .await
        {
            Ok(Some(payload)) => payload,
            Ok(None) => {
                return Err(JobError::transient(
                    ERR_PAYLOAD_MISSING,
                    "upload payload unavailable",
                ))
            }
            Err(error) => {
                console_log!("ingest payload read failed: {:?}", error);
                return Err(JobError::transient(
                    ERR_PAYLOAD_MISSING,
                    "upload payload unavailable",
                ));
            }
        },
        Err(error) => {
            console_log!("ingest payload binding unavailable: {:?}", error);
            return Err(JobError::transient(
                ERR_PAYLOAD_MISSING,
                "upload payload unavailable",
            ));
        }
    };
    if payload_ref
        != format!(
            "{PAYLOAD_KEY_PREFIX}{}",
            content_sha256_hex(payload.as_bytes())
        )
    {
        return Err(JobError::permanent(
            ERR_PARSE_FAILED,
            "upload payload digest mismatch",
        ));
    }
    let estimate = crate::nzb_intake::estimate_nzb(&payload).map_err(|_| {
        JobError::permanent(ERR_PARSE_FAILED, "upload payload failed intake validation")
    })?;
    let parsed = crate::parse::parse_upload(payload.as_bytes(), &estimate)
        .map_err(|_| JobError::permanent(ERR_PARSE_FAILED, "shared NZB parser rejected payload"))?;
    if crate::parse::probe_batches(&parsed).is_empty() {
        return Err(JobError::permanent(
            ERR_PARSE_FAILED,
            "NZB has no segments to probe",
        ));
    }
    advance_checkpoint(cp, false);
    Err(JobError::transient(
        ERR_PROBE_FAILED,
        "pool probe runtime unavailable",
    ))
}

/// Cap on the MB search response body read into memory (25 small JSON
/// hits; anything larger is a contract break, handled as transient).
const MAX_MB_RESPONSE_BYTES: usize = 64 * 1024;

/// Cap on the MB release-recordings response body (the route returns at
/// most 1000 recording objects; at a few hundred JSON bytes each, 1 MiB
/// is generous headroom — anything larger is a contract break, handled
/// as transient like the search cap).
const MAX_MB_RECORDINGS_RESPONSE_BYTES: usize = 1024 * 1024;

/// Load the upload payload for match-query derivation only (it was
/// digest-checked and parsed in `Parsing`). Absence is distinguished
/// from failure: a genuinely missing payload (its 30-day TTL expired
/// before the job reached `Matching`) yields `Ok(None)` and the job is
/// written unmatched, while a binding/read failure is a transient
/// `ERR_MATCH_FAILED` — a KV blip must not bake a permanent unmatched
/// `Done` into the library.
async fn load_payload_for_match(env: &Env, msg: &QueueMsg) -> Result<Option<String>, JobError> {
    let transient = |detail: &'static str| JobError::transient(ERR_MATCH_FAILED, detail);
    let Some(payload_ref) = msg.payload_ref.as_deref() else {
        return Ok(None);
    };
    let store = env
        .kv(PAYLOAD_KV_BINDING)
        .map_err(|_| transient("upload payload unavailable"))?;
    store
        .get(payload_ref)
        .cache_ttl(PAYLOAD_CACHE_TTL_SECS)
        .text()
        .await
        .map_err(|error| {
            console_log!("ingest match payload read failed: {:?}", error);
            transient("upload payload unavailable")
        })
}

/// The query the MB lookup runs for this job: the search text for
/// `IndexerSearch` jobs, or a candidate derived from the NZB subjects for
/// uploads. `None` means no honest query exists (the job is then written
/// unmatched, never guessed at).
async fn match_query(env: &Env, msg: &QueueMsg) -> Result<Option<String>, JobError> {
    match msg.source {
        JobSource::IndexerSearch => Ok(crate::matching::validate_match_query(
            msg.query.as_deref().unwrap_or(""),
        )),
        JobSource::NzbUpload => {
            let Some(payload) = load_payload_for_match(env, msg).await? else {
                return Ok(None);
            };
            let subjects: Vec<String> = match nntune_core::nzb::parse(payload.as_bytes()) {
                Ok(document) => document
                    .files
                    .into_iter()
                    .map(|file| file.subject)
                    .collect(),
                Err(_) => return Ok(None),
            };
            Ok(crate::matching::derive_upload_query(&subjects))
        }
    }
}

/// One fetch through the `MB_API` service binding, body capped at
/// `max_bytes`. Every failure mode (missing binding, transport, oversize
/// body, non-UTF-8) collapses to `Err(())`: callers classify all of them
/// as transient, matching the retry-budget split in `matching`.
async fn fetch_mb_body(env: &Env, url: String, max_bytes: usize) -> Result<(u16, String), ()> {
    let fetcher = env
        .service(crate::matching::MB_API_BINDING)
        .map_err(|_| ())?;
    let response = fetcher.fetch(url, None).await.map_err(|_| ())?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| ())?;
        if bytes.len() + chunk.len() > max_bytes {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes).map_err(|_| ())?;
    Ok((status, text))
}

/// One MB release search through the `MB_API` service binding, classified
/// into the retry budget by [`crate::matching::parse_mb_search_response`].
async fn fetch_release_match(env: &Env, query: &str) -> Result<Option<String>, JobError> {
    let unavailable = |detail: &'static str| JobError::transient(ERR_MATCH_FAILED, detail);
    let (status, text) = fetch_mb_body(
        env,
        crate::matching::mb_search_url(query),
        MAX_MB_RESPONSE_BYTES,
    )
    .await
    .map_err(|()| unavailable("musicbrainz lookup unavailable"))?;
    let hits =
        crate::matching::parse_mb_search_response(status, &text).map_err(
            |failure| match failure {
                crate::matching::MatchFailure::Transient => {
                    unavailable("musicbrainz lookup unavailable")
                }
                crate::matching::MatchFailure::Permanent => {
                    JobError::permanent(ERR_MATCH_FAILED, "musicbrainz rejected the match query")
                }
            },
        )?;
    Ok(crate::matching::pick_release_mbid(&hits))
}

/// One MB release-recordings fetch through the `MB_API` service binding,
/// classified into the retry budget by
/// [`crate::matching::parse_mb_recordings_response`]. Transport failures
/// are `Transient`, exactly like a 5xx.
async fn fetch_release_recordings(
    env: &Env,
    release_mbid: &str,
) -> Result<Vec<String>, crate::matching::MatchFailure> {
    let (status, text) = fetch_mb_body(
        env,
        crate::matching::mb_release_recordings_url(release_mbid),
        MAX_MB_RECORDINGS_RESPONSE_BYTES,
    )
    .await
    .map_err(|()| crate::matching::MatchFailure::Transient)?;
    crate::matching::parse_mb_recordings_response(status, &text)
}

/// Resolve the job's library write: MB lookup when a query exists, an
/// unmatched write when it does not. Resolution is a pure function of the
/// query and the MB mirror, so `Writing` re-resolves on redelivery
/// instead of the checkpoint carrying the match between states.
///
/// A picked release also fetches its recordings. That failure must not
/// lose the release match ([`crate::matching::recording_mbids_or_fallback`]):
/// a permanent recordings failure writes the release with an empty list,
/// while a transient one retries the job so redelivery re-resolves the
/// match — recordings included.
async fn resolve_library_write(
    env: &Env,
    msg: &QueueMsg,
) -> Result<crate::queue::LibraryWrite, JobError> {
    let release_mbid = match match_query(env, msg).await? {
        Some(query) => fetch_release_match(env, &query).await?,
        None => None,
    };
    let recording_mbids = match release_mbid.as_deref() {
        Some(release_mbid) => crate::matching::recording_mbids_or_fallback(
            fetch_release_recordings(env, release_mbid).await,
        )
        .map_err(|_| JobError::transient(ERR_MATCH_FAILED, "musicbrainz lookup unavailable"))?,
        None => Vec::new(),
    };
    Ok(crate::matching::library_write_for(
        msg,
        release_mbid,
        recording_mbids,
    ))
}

/// The side effects [`run_match_write_with`] sequences, behind a trait so
/// the meter-before-advance ordering is regression-tested with a fake
/// instead of resting on prose (the edge impl needs an `Env`). `meter`
/// failures are unit errors: the sequencer maps them to the failing
/// stage's error code, so a fake cannot accidentally bless the wrong code.
trait MatchWriteRuntime {
    async fn resolve(&self, msg: &QueueMsg) -> Result<crate::queue::LibraryWrite, JobError>;
    async fn meter(&self, msg: &QueueMsg, stage: &'static str) -> Result<(), ()>;
    async fn upsert(&self, write: &crate::queue::LibraryWrite) -> Result<(), JobError>;
}

/// Edge [`MatchWriteRuntime`]: MB resolution, Analytics Engine meter, and
/// the Hyperdrive library upsert.
struct EdgeMatchWrite<'a> {
    env: &'a Env,
}

impl MatchWriteRuntime for EdgeMatchWrite<'_> {
    async fn resolve(&self, msg: &QueueMsg) -> Result<crate::queue::LibraryWrite, JobError> {
        resolve_library_write(self.env, msg).await
    }

    async fn meter(&self, msg: &QueueMsg, stage: &'static str) -> Result<(), ()> {
        crate::try_meter(self.env, msg, stage, crate::metering::MeterOutcome::Ok).map_err(|_| ())
    }

    async fn upsert(&self, write: &crate::queue::LibraryWrite) -> Result<(), JobError> {
        let client = crate::library_db_client(self.env)
            .await
            .map_err(|()| JobError::transient(ERR_WRITE_FAILED, "library database unavailable"))?;
        let params = crate::library::upsert_params(write);
        client
            .execute(
                crate::library::upsert_library_sql(),
                &[
                    &params.job_id,
                    &params.user_id,
                    &params.idempotency_key,
                    &params.release_mbid,
                    &params.recording_mbids,
                ],
            )
            .await
            .map_err(|error| {
                // Log the SQLSTATE only (never connection detail) and fail
                // fast on deterministic classes: a schema mismatch such as
                // the not-yet-migrated `library` table (42P01/42P10) must
                // quarantine with an accurate reason, not burn the retry
                // budget as "unavailable".
                let sqlstate = error.as_db_error().map(|db| db.code().code());
                console_log!(
                    "ingest library upsert failed: sqlstate={}",
                    sqlstate.unwrap_or("none")
                );
                if crate::library::is_deterministic_sqlstate(sqlstate) {
                    JobError::permanent(ERR_WRITE_FAILED, "library database schema mismatch")
                } else {
                    JobError::transient(ERR_WRITE_FAILED, "library database unavailable")
                }
            })?;
        Ok(())
    }
}

/// Upsert, then meter `write`, then advance to `Done`.
///
/// The terminal checkpoint is only persisted (`plan_settlement`) after
/// this returns `Ok`, so a `Done` checkpoint exists only once the upsert
/// has committed. A meter failure after the commit settles as a transient
/// retry *before* the `Done` advance: the redelivery replays the upsert
/// (a converged no-op on the idempotency key) and retries the meter, so
/// the event is never silently dropped.
async fn execute_write_with<R: MatchWriteRuntime>(
    runtime: &R,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    write: &crate::queue::LibraryWrite,
) -> Result<(), JobError> {
    crate::library::validate_library_write(write)
        .map_err(|_| JobError::permanent(ERR_WRITE_FAILED, "library write failed validation"))?;
    runtime.upsert(write).await?;
    runtime
        .meter(msg, "write")
        .await
        .map_err(|()| JobError::transient(ERR_WRITE_FAILED, "meter write unavailable"))?;
    advance_checkpoint(cp, false);
    Ok(())
}

/// `Matching` + `Writing` in one delivery: resolve the match, meter
/// `match`, advance, and write. Metering happens *before* each advance,
/// so a meter failure retries the same state (re-resolving is idempotent)
/// instead of dropping the event. A delivery that enters in `Writing`
/// (checkpoint persisted after the match transition) re-resolves and
/// writes without re-emitting `match`.
async fn run_match_write_with<R: MatchWriteRuntime>(
    runtime: &R,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
) -> Result<(), JobError> {
    let write = runtime.resolve(msg).await?;
    if cp.state == JobState::Matching {
        runtime
            .meter(msg, "match")
            .await
            .map_err(|()| JobError::transient(ERR_MATCH_FAILED, "meter write unavailable"))?;
        advance_checkpoint(cp, false);
    }
    execute_write_with(runtime, msg, cp, &write).await
}

async fn run_match_write(
    env: &Env,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
) -> Result<(), JobError> {
    run_match_write_with(&EdgeMatchWrite { env }, msg, cp).await
}

/// Decide one delivery's settlement and whether the mutated checkpoint
/// must be persisted (the single write of this delivery) before settling.
///
/// Pure over the checkpoint + step result so the one-write policy is
/// unit-tested without a KV binding: terminal replays (`Done`, and a
/// `Failed` re-quarantine, which [`decide_checkpoint`] builds without
/// mutating the checkpoint) need no write; every outcome of a step that
/// ran persists exactly once — including a step that just advanced the
/// checkpoint to `Done`, whose terminal state exists only in memory until
/// this write lands.
fn plan_settlement(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    step: Result<(), JobError>,
    step_ran: bool,
) -> (bool, Settlement) {
    let persist = step_ran || !matches!(cp.state, JobState::Done | JobState::Failed);
    match decide_checkpoint(msg, cp, step) {
        Ok(Disposition::Ack) => (persist, Settlement::Ack),
        Ok(Disposition::Retry { delay_secs }) => (true, Settlement::RetryAfter(delay_secs)),
        Ok(Disposition::Quarantine(entry)) => (persist, Settlement::Quarantine(entry)),
        // Identity was checked by `checkpoint_matches` before the step, so
        // a mismatch here is unreachable in practice; retry unpersisted
        // rather than acking work we could not account for.
        Err(_) => (false, Settlement::RetryAfter(UNPERSISTED_RETRY_SECS)),
    }
}

/// In-invocation retry policy for a rejected checkpoint write: exactly one
/// retry, after waiting out KV's 1 s same-key write window (plus margin).
/// The colliding write is the admission route's initial/reset checkpoint
/// (or a duplicate copy's persist), which landed before this attempt, so
/// one window from the failure is always past it. Pure so the policy is
/// unit-tested without a KV binding.
fn persist_retry_delay_ms(failed_attempts: u32) -> Option<u64> {
    match failed_attempts {
        1 => Some(crate::checkpoint::KV_SAME_KEY_WRITE_WINDOW_MS + 100),
        _ => None,
    }
}

async fn persist(env: &Env, cp: &JobCheckpoint) -> bool {
    let mut failed_attempts = 0;
    loop {
        match crate::checkpoint::put(env, cp).await {
            Ok(()) => return true,
            Err(error) => {
                failed_attempts += 1;
                console_log!("ingest checkpoint write failed: {:?}", error);
                match persist_retry_delay_ms(failed_attempts) {
                    Some(delay_ms) => {
                        worker::Delay::from(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    None => return false,
                }
            }
        }
    }
}

async fn process_message(env: &Env, msg: &QueueMsg) -> Settlement {
    if validate_queue_msg(msg).is_err() {
        // Poison shape: no trustworthy job identity to checkpoint against.
        // Native Queue retries move it to the dead-letter queue.
        return Settlement::RetryAfter(UNPERSISTED_RETRY_SECS);
    }
    let mut cp = match crate::checkpoint::get(env, &msg.job_id).await {
        Ok(Some(cp)) => {
            if !checkpoint_matches(msg, &cp) {
                return Settlement::RetryAfter(UNPERSISTED_RETRY_SECS);
            }
            cp
        }
        Ok(None) => crate::checkpoint::initial(msg),
        Err(error) => {
            console_log!("ingest checkpoint read failed: {:?}", error);
            return Settlement::RetryAfter(UNPERSISTED_RETRY_SECS);
        }
    };

    // Terminal checkpoints skip the step: `Done` acks, `Failed`
    // re-quarantines the recorded failure (both without a write).
    let step_ran = !matches!(cp.state, JobState::Done | JobState::Failed);
    let step = if step_ran {
        run_step(env, msg, &mut cp).await
    } else {
        Ok(())
    };
    let (must_persist, settlement) = plan_settlement(msg, &mut cp, step, step_ran);
    if must_persist && !persist(env, &cp).await {
        return Settlement::RetryAfter(UNPERSISTED_RETRY_SECS);
    }
    settlement
}

async fn send_to_dlq(env: &Env, entry: &DlqEntry) -> bool {
    match env.queue(DLQ_QUEUE_BINDING) {
        Ok(queue) => match queue.send(entry.clone()).await {
            Ok(()) => true,
            Err(error) => {
                console_log!("ingest DLQ send failed: {:?}", error);
                false
            }
        },
        Err(error) => {
            console_log!("ingest DLQ binding missing: {:?}", error);
            false
        }
    }
}

fn retry_raw(raw: &worker::RawMessage, seconds: u32) {
    let options = QueueRetryOptionsBuilder::new()
        .with_delay_seconds(seconds)
        .build();
    raw.retry_with_options(&options);
}

async fn process_raw_message(env: &Env, raw: worker::RawMessage) {
    let value = match serde_wasm_bindgen::from_value::<serde_json::Value>(raw.body()) {
        Ok(value) => value,
        Err(_) => {
            retry_raw(&raw, UNPERSISTED_RETRY_SECS);
            return;
        }
    };
    let msg = match serde_json::from_value::<QueueMsg>(value) {
        Ok(msg) => msg,
        Err(_) => {
            retry_raw(&raw, UNPERSISTED_RETRY_SECS);
            return;
        }
    };
    match process_message(env, &msg).await {
        Settlement::Ack => raw.ack(),
        Settlement::RetryAfter(seconds) => retry_raw(&raw, seconds),
        Settlement::Quarantine(entry) => {
            // Never ack on DLQ-send failure: the redelivery re-plans from
            // the persisted `Failed` checkpoint and retries the send.
            if send_to_dlq(env, &entry).await {
                raw.ack()
            } else {
                retry_raw(&raw, UNPERSISTED_RETRY_SECS)
            }
        }
    }
}

#[worker::event(queue)]
pub async fn ingest_queue(
    batch: worker::MessageBatch<serde_json::Value>,
    env: Env,
    _ctx: worker::Context,
) -> worker::Result<()> {
    for raw in batch.raw_iter() {
        process_raw_message(&env, raw).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{build_search_dispatch, build_upload_dispatch};

    fn search_message() -> QueueMsg {
        build_search_dispatch("user_01", "dark side", "vault_idx_01", &[3000], 25, 0).unwrap()
    }

    #[test]
    fn validates_message_identity_and_source_inputs_before_checkpointing() {
        let valid = search_message();
        assert!(validate_queue_msg(&valid).is_ok());

        let mut wrong_user = valid.clone();
        wrong_user.user_id = "other_user".into();
        assert!(validate_queue_msg(&wrong_user).is_err());

        let mut wrong_state = valid.clone();
        wrong_state.state = JobState::Done;
        assert!(validate_queue_msg(&wrong_state).is_err());

        let mut incomplete_search = valid;
        incomplete_search.indexer_ref = None;
        assert!(validate_queue_msg(&incomplete_search).is_err());

        let mut incomplete_upload =
            build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB).unwrap();
        incomplete_upload.payload_ref = None;
        assert!(validate_queue_msg(&incomplete_upload).is_err());
    }

    #[test]
    fn checkpoint_identity_must_match_the_redelivered_message() {
        let msg = search_message();
        let cp = crate::checkpoint::initial(&msg);
        assert!(checkpoint_matches(&msg, &cp));

        let mut other = msg.clone();
        other.job_id = "job_other".into();
        assert!(!checkpoint_matches(&other, &cp));
    }

    #[test]
    fn terminal_replays_settle_without_a_checkpoint_write() {
        let msg = search_message();
        let mut done = crate::checkpoint::initial(&msg);
        done.state = JobState::Done;
        let (persist, settlement) = plan_settlement(&msg, &mut done, Ok(()), false);
        assert!(!persist);
        assert_eq!(settlement, Settlement::Ack);

        let mut failed = crate::checkpoint::initial(&msg);
        failed.state = JobState::Failed;
        failed.failure_code = Some(ERR_PROBE_FAILED.into());
        failed.failure_reason = Some("pool probe runtime unavailable".into());
        let (persist, settlement) = plan_settlement(&msg, &mut failed, Ok(()), false);
        assert!(!persist);
        match settlement {
            Settlement::Quarantine(entry) => assert_eq!(entry.error_code, ERR_PROBE_FAILED),
            other => panic!("failed replay must re-quarantine, got {other:?}"),
        }
    }

    #[test]
    fn transient_failure_persists_once_and_retries_with_the_shared_backoff() {
        let msg = search_message();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Searching;
        let (persist, settlement) = plan_settlement(
            &msg,
            &mut cp,
            Err(JobError::transient(ERR_SEARCH_FAILED, "unavailable")),
            true,
        );
        assert!(persist);
        assert_eq!(settlement, Settlement::RetryAfter(5));
        assert_eq!(cp.attempt, 2);
        assert_eq!(cp.state, JobState::Searching);
    }

    #[test]
    fn step_completed_done_persists_once_and_acks() {
        // A delivery whose step just advanced the checkpoint to Done must
        // persist that terminal state (exactly one write) before acking;
        // only a terminal *replay* (step did not run) skips the write.
        let msg = search_message();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Done;
        let (persist, settlement) = plan_settlement(&msg, &mut cp, Ok(()), true);
        assert!(persist);
        assert_eq!(settlement, Settlement::Ack);
    }

    /// Fake [`MatchWriteRuntime`] recording every effect in call order, so
    /// the meter-before-advance sequencing is asserted, not just prose.
    struct FakeMatchWrite {
        calls: std::cell::RefCell<Vec<&'static str>>,
        fail_meter_stage: Option<&'static str>,
    }

    impl FakeMatchWrite {
        fn new(fail_meter_stage: Option<&'static str>) -> Self {
            Self {
                calls: std::cell::RefCell::new(Vec::new()),
                fail_meter_stage,
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl MatchWriteRuntime for FakeMatchWrite {
        async fn resolve(&self, msg: &QueueMsg) -> Result<crate::queue::LibraryWrite, JobError> {
            self.calls.borrow_mut().push("resolve");
            Ok(crate::matching::library_write_for(msg, None, Vec::new()))
        }

        async fn meter(&self, _msg: &QueueMsg, stage: &'static str) -> Result<(), ()> {
            self.calls.borrow_mut().push(match stage {
                "match" => "meter:match",
                _ => "meter:write",
            });
            if self.fail_meter_stage == Some(stage) {
                return Err(());
            }
            Ok(())
        }

        async fn upsert(&self, _write: &crate::queue::LibraryWrite) -> Result<(), JobError> {
            self.calls.borrow_mut().push("upsert");
            Ok(())
        }
    }

    #[tokio::test]
    async fn matching_entry_meters_match_before_advancing_then_writes() {
        let msg = search_message();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Matching;
        let runtime = FakeMatchWrite::new(None);
        run_match_write_with(&runtime, &msg, &mut cp)
            .await
            .expect("matching delivery succeeds");
        assert_eq!(
            runtime.calls(),
            ["resolve", "meter:match", "upsert", "meter:write"]
        );
        assert_eq!(cp.state, JobState::Done);
    }

    #[tokio::test]
    async fn writing_entry_re_resolves_without_re_emitting_match() {
        let msg = search_message();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Writing;
        let runtime = FakeMatchWrite::new(None);
        run_match_write_with(&runtime, &msg, &mut cp)
            .await
            .expect("writing delivery succeeds");
        assert_eq!(runtime.calls(), ["resolve", "upsert", "meter:write"]);
        assert_eq!(cp.state, JobState::Done);
    }

    #[tokio::test]
    async fn meter_failures_stop_before_the_advance_they_guard() {
        let msg = search_message();
        // `match` meter down: no advance out of Matching, no upsert spent.
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Matching;
        let runtime = FakeMatchWrite::new(Some("match"));
        let err = run_match_write_with(&runtime, &msg, &mut cp)
            .await
            .expect_err("match meter failure must fail the step");
        assert_eq!(err.code, ERR_MATCH_FAILED);
        assert!(err.transient);
        assert_eq!(runtime.calls(), ["resolve", "meter:match"]);
        assert_eq!(cp.state, JobState::Matching);

        // `write` meter down after the upsert: the checkpoint stays in
        // Writing, so redelivery replays the converged upsert and retries
        // the meter instead of acking a Done that was never metered.
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Writing;
        let runtime = FakeMatchWrite::new(Some("write"));
        let err = run_match_write_with(&runtime, &msg, &mut cp)
            .await
            .expect_err("write meter failure must fail the step");
        assert_eq!(err.code, ERR_WRITE_FAILED);
        assert!(err.transient);
        assert_eq!(runtime.calls(), ["resolve", "upsert", "meter:write"]);
        assert_eq!(cp.state, JobState::Writing);
    }

    #[test]
    fn persist_retries_once_after_waiting_out_the_kv_write_window() {
        // The admission route's write to the same key can land <1 s before
        // the first delivery's persist; the retry must wait strictly past
        // KV's same-key window, and a second failure gives the delivery
        // back (RetryAfter) instead of looping in-invocation.
        let delay = persist_retry_delay_ms(1).expect("first failure retries in-invocation");
        assert!(delay > crate::checkpoint::KV_SAME_KEY_WRITE_WINDOW_MS);
        assert_eq!(persist_retry_delay_ms(2), None);
        assert_eq!(persist_retry_delay_ms(0), None);
    }

    #[test]
    fn exhausted_and_permanent_failures_persist_failed_once_then_quarantine() {
        let msg = search_message();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Searching;
        cp.attempt = MAX_ATTEMPTS;
        let (persist, settlement) = plan_settlement(
            &msg,
            &mut cp,
            Err(JobError::transient(ERR_SEARCH_FAILED, "unavailable")),
            true,
        );
        assert!(persist);
        assert_eq!(cp.state, JobState::Failed);
        assert!(matches!(settlement, Settlement::Quarantine(_)));

        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Parsing;
        let (persist, settlement) = plan_settlement(
            &msg,
            &mut cp,
            Err(JobError::permanent(ERR_PARSE_FAILED, "bad nzb")),
            true,
        );
        assert!(persist);
        assert_eq!(cp.state, JobState::Failed);
        assert_eq!(cp.failure_code.as_deref(), Some(ERR_PARSE_FAILED));
        assert!(matches!(settlement, Settlement::Quarantine(_)));
    }
}
