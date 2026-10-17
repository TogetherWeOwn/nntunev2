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
//! Runtime boundaries: the pool probe (`Probing`, via
//! [`crate::pool_probe`]) and the Verifying step (`Verifying`, via
//! [`crate::verify`] over [`crate::pool_body`] and the scratch bucket)
//! are executable for upload jobs. Indexer search, repair, match and
//! write still have no executable consumer wiring, so those states
//! settle as transient failures through the shared retry budget. The
//! consumer therefore stays disabled in `wrangler.toml.example` until
//! those boundaries land.

use worker::{console_log, Env, MessageExt, QueueRetryOptionsBuilder};

use crate::dispatch::{
    content_sha256_hex, make_job_id, validate_user_id, PAYLOAD_KEY_PREFIX, PAYLOAD_KV_BINDING,
};
use crate::jobs::{
    advance_checkpoint, validate_job_id, JobCheckpoint, JobSource, JobState, MAX_ATTEMPTS,
    MAX_REPAIR_ROUNDS,
};
use crate::newznab::{MAX_CATEGORIES, MAX_LIMIT, MAX_PAGE};
use crate::parse::{ParsedFile, ParsedNzb};
use crate::pool_body::{build_body_request, call_pool_body, BodyFetch, PoolBodyError};
use crate::pool_probe::{call_pool_probe, PoolProbeError};
use crate::probe::{build_probe_request_with_ranges, ProbeTally};
use crate::queue::{validate_indexer_ref, QueueMsg};
use crate::retry::{
    decide_checkpoint, Disposition, DlqEntry, JobError, DLQ_QUEUE_BINDING, ERR_MATCH_FAILED,
    ERR_PARSE_FAILED, ERR_PAYLOAD_MISSING, ERR_PROBE_FAILED, ERR_REPAIR_FAILED, ERR_SEARCH_FAILED,
    ERR_VERIFY_FAILED, ERR_WRITE_FAILED,
};
use crate::verify::{
    BodySource, ScratchStore, StagedFile, VerifyCaps, VerifyError, VerifyOutcome, VerifyProgress,
    SCRATCH_BINDING,
};

/// Payload reads use KV's minimum `cacheTtl` (30 s) so an edge-cached miss
/// expires before the later payload-lag retries (see `dispatch`).
const PAYLOAD_CACHE_TTL_SECS: u64 = 30;
/// Delay for settlements that could not be persisted or quarantined: the
/// next delivery redoes the (idempotent) step with the checkpoint as-is.
const UNPERSISTED_RETRY_SECS: u32 = 5;

/// Probe batches called per delivery; the tally in the checkpoint makes
/// the rest resumable on the next delivery.
const MAX_PROBE_BATCHES_PER_DELIVERY: usize = 4;

/// Wall-clock budget for one Verifying delivery; the step checkpoints
/// per file and resumes on redelivery past this.
const VERIFY_WALL_CLOCK_BUDGET_MS: u64 = 20_000;

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
/// Upload jobs run `Parsing` (digest-check + shared parser), `Probing`
/// (pool STAT batches folded into the checkpoint tally) and `Verifying`
/// (pool BODY fetch + par2 verify + scratch staging). A step that
/// advances the state still settles transiently, so the next delivery
/// runs the next state — progress is only ever claimed through the
/// persisted checkpoint. Every other non-terminal state (and every
/// search-source state past `Queued`) is a transient boundary failure
/// today.
async fn run_step(env: &Env, msg: &QueueMsg, cp: &mut JobCheckpoint) -> Result<(), JobError> {
    if cp.state == JobState::Queued {
        advance_checkpoint(cp, false);
    }
    if cp.source != JobSource::NzbUpload
        || !matches!(
            cp.state,
            JobState::Parsing | JobState::Probing | JobState::Verifying
        )
    {
        return Err(JobError::transient(
            unavailable_code(cp.state),
            "ingest runtime boundary unavailable",
        ));
    }
    let (parsed, files) = load_upload_parse(env, msg).await?;
    match cp.state {
        JobState::Parsing => {
            if crate::parse::probe_batches(&parsed).is_empty() {
                return Err(JobError::permanent(
                    ERR_PARSE_FAILED,
                    "NZB has no segments to probe",
                ));
            }
            advance_checkpoint(cp, false);
            Err(JobError::transient(ERR_PROBE_FAILED, "probing pending"))
        }
        JobState::Probing => run_probing(env, msg, cp, &parsed).await,
        JobState::Verifying => run_verifying(env, msg, cp, &files).await,
        _ => unreachable!("state filtered above"),
    }
}

/// Fetch and digest-check the upload payload, then run the intake guard
/// and both shared-parser projections (flat for the probe, per-file for
/// Verifying). Nothing is persisted: every delivery re-derives the parse
/// from the payload KV, so the parse can never disagree with the bytes
/// the digest pins.
async fn load_upload_parse(
    env: &Env,
    msg: &QueueMsg,
) -> Result<(ParsedNzb, Vec<ParsedFile>), JobError> {
    let Some(payload_ref) = msg.payload_ref.as_deref() else {
        return Err(JobError::permanent(
            ERR_PAYLOAD_MISSING,
            "upload payload handle missing",
        ));
    };
    let unavailable = || JobError::transient(ERR_PAYLOAD_MISSING, "upload payload unavailable");
    let payload = match env.kv(PAYLOAD_KV_BINDING) {
        Ok(store) => match store
            .get(payload_ref)
            .cache_ttl(PAYLOAD_CACHE_TTL_SECS)
            .text()
            .await
        {
            Ok(Some(payload)) => payload,
            Ok(None) => return Err(unavailable()),
            Err(error) => {
                console_log!("ingest payload read failed: {:?}", error);
                return Err(unavailable());
            }
        },
        Err(error) => {
            console_log!("ingest payload binding unavailable: {:?}", error);
            return Err(unavailable());
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
    let files = crate::parse::parse_upload_files(payload.as_bytes(), &estimate)
        .map_err(|_| JobError::permanent(ERR_PARSE_FAILED, "shared NZB parser rejected payload"))?;
    Ok((parsed, files))
}

/// `Probing`: call the pool for the next unreported batches (bounded per
/// delivery) and fold answers into the checkpoint tally. The completed
/// tally stays on the checkpoint into `Verifying` (the gap count is the
/// probe's evidence; Verifying re-derives exact blocks from the bodies).
async fn run_probing(
    env: &Env,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    parsed: &ParsedNzb,
) -> Result<(), JobError> {
    let batches = crate::parse::probe_batches_with_ranges(parsed);
    if batches.is_empty() {
        return Err(JobError::permanent(
            ERR_PARSE_FAILED,
            "NZB has no segments to probe",
        ));
    }
    let mut tally = cp
        .probe_tally
        .take()
        .filter(|t| t.total_batches == batches.len())
        .unwrap_or_else(|| ProbeTally::new(batches.len()));
    let mut called = 0usize;
    for (index, batch) in batches.iter().enumerate() {
        if tally.missing_by_batch.contains_key(&index) {
            continue;
        }
        if called >= MAX_PROBE_BATCHES_PER_DELIVERY {
            break;
        }
        called += 1;
        let pairs: Vec<(&str, Option<u64>)> = batch
            .iter()
            .map(|(id, bytes)| (id.as_str(), *bytes))
            .collect();
        let req = match build_probe_request_with_ranges(msg, index, &pairs) {
            Ok(req) => req,
            Err(_) => {
                cp.probe_tally = Some(tally);
                return Err(JobError::permanent(
                    ERR_PROBE_FAILED,
                    "probe envelope invalid",
                ));
            }
        };
        match call_pool_probe(env, &req).await {
            Ok(outcome) => {
                tally.record(&outcome);
            }
            Err(PoolProbeError::Retry { .. }) => {
                cp.probe_tally = Some(tally);
                return Err(JobError::transient(
                    ERR_PROBE_FAILED,
                    "pool probe unavailable",
                ));
            }
            Err(PoolProbeError::Rejected) => {
                cp.probe_tally = Some(tally);
                return Err(JobError::permanent(
                    ERR_PROBE_FAILED,
                    "pool probe rejected the envelope",
                ));
            }
        }
    }
    let complete = tally.is_complete();
    cp.probe_tally = Some(tally);
    if complete {
        advance_checkpoint(cp, false);
        Err(JobError::transient(ERR_VERIFY_FAILED, "verifying pending"))
    } else {
        Err(JobError::transient(ERR_PROBE_FAILED, "probing in progress"))
    }
}

/// Map one verify outcome onto the checkpoint: advance on `Complete` and
/// on a dispatched repair, never move the state otherwise (permanent
/// step errors are failed by `decide_checkpoint`, which keeps the
/// settlement persist planner seeing a non-terminal checkpoint). Pure
/// over the checkpoint so the mapping is unit-tested without bindings.
fn apply_verify_outcome(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    outcome: VerifyOutcome,
) -> Result<(), JobError> {
    match outcome {
        VerifyOutcome::Complete => {
            // decide_repair is the single dispatch authority: zero
            // missing blocks must come back `Ok(None)`.
            match crate::repair::decide_repair(
                &msg.job_id,
                &msg.user_id,
                &msg.idempotency_key,
                "",
                0,
            ) {
                Ok(None) => {}
                _ => {
                    return Err(JobError::permanent(
                        ERR_VERIFY_FAILED,
                        "repair decision rejected a complete verify",
                    ))
                }
            }
            cp.probe_tally = None;
            advance_checkpoint(cp, false);
            Err(JobError::transient(ERR_MATCH_FAILED, "matching pending"))
        }
        VerifyOutcome::RepairNeeded {
            par2_set_id,
            missing_blocks,
        } => {
            match crate::repair::decide_repair(
                &msg.job_id,
                &msg.user_id,
                &msg.idempotency_key,
                &par2_set_id,
                missing_blocks,
            ) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(JobError::permanent(
                        ERR_VERIFY_FAILED,
                        "repair decision skipped missing blocks",
                    ))
                }
                Err(detail) => {
                    return Err(JobError::permanent(ERR_VERIFY_FAILED, detail));
                }
            }
            if cp.repair_rounds >= MAX_REPAIR_ROUNDS {
                return Err(JobError::permanent(
                    ERR_REPAIR_FAILED,
                    "repair budget exhausted",
                ));
            }
            cp.probe_tally = None;
            advance_checkpoint(cp, true);
            Err(JobError::transient(ERR_REPAIR_FAILED, "repair pending"))
        }
        VerifyOutcome::Unrecoverable { .. } => Err(JobError::permanent(
            ERR_VERIFY_FAILED,
            "release is missing blocks beyond par2 recovery",
        )),
        VerifyOutcome::Incomplete => Err(JobError::transient(
            ERR_VERIFY_FAILED,
            "verifying in progress",
        )),
    }
}

/// `Verifying`: run the verify driver over the pool body route and the
/// scratch bucket, persist the per-file progress beside the checkpoint,
/// then map the outcome onto the state machine.
async fn run_verifying(
    env: &Env,
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    files: &[ParsedFile],
) -> Result<(), JobError> {
    let loaded = match crate::checkpoint::get_verify(env, &msg.job_id).await {
        Ok(loaded) => loaded,
        Err(error) => {
            console_log!("ingest verify progress read failed: {:?}", error);
            return Err(JobError::transient(
                ERR_VERIFY_FAILED,
                "verify progress unavailable",
            ));
        }
    };
    let mut progress = match loaded {
        Some(progress) if progress.round == cp.repair_rounds => progress,
        _ => VerifyProgress::new(cp.repair_rounds),
    };
    let bucket = match env.bucket(SCRATCH_BINDING) {
        Ok(bucket) => bucket,
        Err(error) => {
            console_log!("ingest scratch binding unavailable: {:?}", error);
            return Err(JobError::transient(
                ERR_VERIFY_FAILED,
                "scratch storage unavailable",
            ));
        }
    };
    let scratch = R2Scratch { bucket };
    let mut source = PoolBodySource { env, msg };
    let started_ms = worker::Date::now().as_millis();
    let deadline = move || {
        worker::Date::now().as_millis().saturating_sub(started_ms) > VERIFY_WALL_CLOCK_BUDGET_MS
    };
    let outcome = crate::verify::run_verifying_step(
        &msg.job_id,
        files,
        &mut progress,
        &mut source,
        &scratch,
        &VerifyCaps::default(),
        &deadline,
    )
    .await;
    // Persist progress before the checkpoint settlement so a crash
    // between the two writes resumes from the older state with the
    // newer progress (scratch adoption covers a lost progress write).
    if let Err(error) = crate::checkpoint::put_verify(env, &msg.job_id, &progress).await {
        console_log!("ingest verify progress write failed: {:?}", error);
        return Err(JobError::transient(
            ERR_VERIFY_FAILED,
            "verify progress unavailable",
        ));
    }
    match outcome {
        Ok(outcome) => apply_verify_outcome(msg, cp, outcome),
        Err(VerifyError::Transient(detail)) => Err(JobError::transient(ERR_VERIFY_FAILED, detail)),
        Err(VerifyError::Permanent(detail)) => Err(JobError::permanent(ERR_VERIFY_FAILED, detail)),
    }
}

/// [`BodySource`] over the pool's BODY route. A pool `Retry-After`
/// collapses into the consumer's shared checkpoint backoff, exactly as
/// on the probe path (`run_probing`): the settlement contract has one
/// delay schedule, and the hint only orders retries within it.
struct PoolBodySource<'a> {
    env: &'a Env,
    msg: &'a QueueMsg,
}

impl BodySource for PoolBodySource<'_> {
    async fn fetch_body(&mut self, segment_id: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        let req = build_body_request(self.msg, segment_id)
            .map_err(|_| VerifyError::Permanent("invalid segment id".into()))?;
        match call_pool_body(self.env, &req).await {
            Ok(BodyFetch::Bytes(bytes)) => Ok(Some(bytes)),
            Ok(BodyFetch::Missing) => Ok(None),
            Err(PoolBodyError::Retry { .. }) => {
                Err(VerifyError::Transient("pool body fetch unavailable".into()))
            }
            Err(PoolBodyError::Rejected) => {
                Err(VerifyError::Permanent("pool body fetch rejected".into()))
            }
        }
    }
}

/// [`ScratchStore`] over the `INGEST_SCRATCH` R2 bucket. The staged
/// judgement rides the object's custom metadata, so a resume can adopt
/// a staged file without re-reading its bytes.
struct R2Scratch {
    bucket: worker::Bucket,
}

fn staged_from_metadata(
    key: &str,
    size: u64,
    meta: &std::collections::HashMap<String, String>,
) -> Option<StagedFile> {
    Some(StagedFile {
        key: key.to_string(),
        size,
        sha256: meta.get("sha256")?.clone(),
        missing_blocks: meta
            .get("missing_blocks")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        par2_set_id: meta.get("par2_set_id").cloned(),
    })
}

impl ScratchStore for R2Scratch {
    async fn head(&self, key: &str) -> Result<Option<StagedFile>, VerifyError> {
        let object = self
            .bucket
            .head(key.to_string())
            .await
            .map_err(|_| VerifyError::Transient("scratch head failed".into()))?;
        let Some(object) = object else {
            return Ok(None);
        };
        let meta = object
            .custom_metadata()
            .map_err(|_| VerifyError::Transient("scratch metadata unreadable".into()))?;
        Ok(staged_from_metadata(key, object.size(), &meta))
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, VerifyError> {
        let object = self
            .bucket
            .get(key.to_string())
            .execute()
            .await
            .map_err(|_| VerifyError::Transient("scratch read failed".into()))?;
        let Some(object) = object else {
            return Ok(None);
        };
        let Some(body) = object.body() else {
            return Ok(None);
        };
        body.bytes()
            .await
            .map(Some)
            .map_err(|_| VerifyError::Transient("scratch read failed".into()))
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, meta: &StagedFile) -> Result<(), VerifyError> {
        let mut md = std::collections::HashMap::new();
        md.insert("sha256".to_string(), meta.sha256.clone());
        md.insert(
            "missing_blocks".to_string(),
            meta.missing_blocks.to_string(),
        );
        if let Some(set_id) = &meta.par2_set_id {
            md.insert("par2_set_id".to_string(), set_id.clone());
        }
        self.bucket
            .put(key.to_string(), bytes)
            .custom_metadata(md)
            .execute()
            .await
            .map_err(|_| VerifyError::Transient("scratch write failed".into()))?;
        Ok(())
    }
}

/// Decide one delivery's settlement and whether the mutated checkpoint
/// must be persisted (the single write of this delivery) before settling.
///
/// Pure over the checkpoint + step result so the one-write policy is
/// unit-tested without a KV binding: terminal replays (`Done`, and a
/// `Failed` re-quarantine, which [`decide_checkpoint`] builds without
/// mutating the checkpoint) need no write; every other outcome persists
/// exactly once.
fn plan_settlement(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    step: Result<(), JobError>,
) -> (bool, Settlement) {
    let persist = !matches!(cp.state, JobState::Done | JobState::Failed);
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
    let step = if matches!(cp.state, JobState::Done | JobState::Failed) {
        Ok(())
    } else {
        run_step(env, msg, &mut cp).await
    };
    let (must_persist, settlement) = plan_settlement(msg, &mut cp, step);
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
        let (persist, settlement) = plan_settlement(&msg, &mut done, Ok(()));
        assert!(!persist);
        assert_eq!(settlement, Settlement::Ack);

        let mut failed = crate::checkpoint::initial(&msg);
        failed.state = JobState::Failed;
        failed.failure_code = Some(ERR_PROBE_FAILED.into());
        failed.failure_reason = Some("pool probe runtime unavailable".into());
        let (persist, settlement) = plan_settlement(&msg, &mut failed, Ok(()));
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
        );
        assert!(persist);
        assert_eq!(settlement, Settlement::RetryAfter(5));
        assert_eq!(cp.attempt, 2);
        assert_eq!(cp.state, JobState::Searching);
    }

    fn verifying_checkpoint(msg: &QueueMsg) -> JobCheckpoint {
        let mut cp = crate::checkpoint::initial(msg);
        cp.state = JobState::Verifying;
        cp
    }

    #[test]
    fn verify_complete_advances_to_matching_and_clears_the_tally() {
        let msg = search_message();
        let mut cp = verifying_checkpoint(&msg);
        cp.probe_tally = Some(crate::probe::ProbeTally::new(1));
        let step = apply_verify_outcome(&msg, &mut cp, crate::verify::VerifyOutcome::Complete);
        // The next state has no executable step in this delivery: settle
        // transiently so the redelivery runs it (the Parsing pattern).
        let err = step.expect_err("matching runs on the next delivery");
        assert!(err.transient);
        assert_eq!(err.code, ERR_MATCH_FAILED);
        assert_eq!(cp.state, JobState::Matching);
        assert!(cp.probe_tally.is_none());
    }

    #[test]
    fn verify_repair_needed_dispatches_through_decide_repair() {
        let msg = search_message();
        let mut cp = verifying_checkpoint(&msg);
        let step = apply_verify_outcome(
            &msg,
            &mut cp,
            crate::verify::VerifyOutcome::RepairNeeded {
                par2_set_id: "ab".repeat(16),
                missing_blocks: 2,
            },
        );
        let err = step.expect_err("repair runs on the next delivery");
        assert!(err.transient);
        assert_eq!(err.code, ERR_REPAIR_FAILED);
        assert_eq!(cp.state, JobState::Repairing);
        assert_eq!(cp.repair_rounds, 1);
    }

    #[test]
    fn verify_repair_at_the_round_cap_fails_permanently_without_advancing() {
        // advance_checkpoint would set Failed in-memory, which the
        // settlement planner must never see (it would skip the persist):
        // the exhausted budget is a permanent step error instead, and
        // decide_checkpoint marks the job Failed.
        let msg = search_message();
        let mut cp = verifying_checkpoint(&msg);
        cp.repair_rounds = MAX_REPAIR_ROUNDS;
        let step = apply_verify_outcome(
            &msg,
            &mut cp,
            crate::verify::VerifyOutcome::RepairNeeded {
                par2_set_id: "ab".repeat(16),
                missing_blocks: 1,
            },
        );
        let err = step.expect_err("budget spent");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_REPAIR_FAILED);
        assert_eq!(cp.state, JobState::Verifying);
    }

    #[test]
    fn verify_unrecoverable_and_incomplete_never_advance() {
        let msg = search_message();
        let mut cp = verifying_checkpoint(&msg);
        let err = apply_verify_outcome(
            &msg,
            &mut cp,
            crate::verify::VerifyOutcome::Unrecoverable {
                par2_set_id: None,
                missing_blocks: 3,
            },
        )
        .expect_err("unrecoverable");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_VERIFY_FAILED);
        assert_eq!(cp.state, JobState::Verifying);

        let err = apply_verify_outcome(&msg, &mut cp, crate::verify::VerifyOutcome::Incomplete)
            .expect_err("incomplete");
        assert!(err.transient);
        assert_eq!(err.code, ERR_VERIFY_FAILED);
        assert_eq!(cp.state, JobState::Verifying);
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
        );
        assert!(persist);
        assert_eq!(cp.state, JobState::Failed);
        assert_eq!(cp.failure_code.as_deref(), Some(ERR_PARSE_FAILED));
        assert!(matches!(settlement, Settlement::Quarantine(_)));
    }
}
