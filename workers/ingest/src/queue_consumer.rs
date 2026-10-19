//! Per-message Queue delivery: validated `event(queue)` consumer.
//!
//! Settlement is decided by [`crate::retry::decide_checkpoint`] (the retry
//! budget + DLQ contract): this module validates each message against its
//! deterministic job identity, loads or creates the durable checkpoint,
//! runs the executable steps (upload payload digest + shared-parser
//! validation, then the pool-DO segment probe), and maps the decision onto
//! ack / delayed retry / DLQ quarantine per message.
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
//! table yet (see [`crate::library`]).
//!
//! Probing folds each answered batch into the checkpoint's probe tally
//! in memory as it lands; the delivery's single persist (before
//! settlement, above) is what makes the folded batches durable. KV's
//! 1 write/s/key cap forbids a write per batch (a cap-sized NZB has ~120
//! batches), so a crash before that persist simply re-asks the batches it
//! had folded: `STAT` is idempotent and the tally is keyed by batch
//! index, so a redelivery resumes without ever double-counting.
//!
//! That crash argument alone is not enough: asking about *every*
//! pending batch in one delivery makes the re-ask deterministic. A
//! cap-sized NZB is ~120 batches of 500 sequential `STAT` round-trips
//! (the pool stats ids one after another), about 900 s at a nominal
//! 15 ms provider RTT — the whole 15-minute Queue consumer invocation
//! limit, before the up-to-`max_batch_size` (10) messages sharing the
//! invocation are counted. A runtime-killed delivery never persists,
//! so every redelivery would restart at batch 0 and die at the same
//! wall. Probing therefore runs on a per-delivery budget (see
//! [`run_probing_batches`]): at most [`MAX_PROBE_BATCHES_PER_DELIVERY`]
//! batches and [`PROBE_DELIVERY_BUDGET_MS`] of wall time per message,
//! then the delivery persists its partial tally and settles as a
//! *progress continuation* ([`JobError::progress`]) — a fresh copy of
//! the message is enqueued and the original acked, spending neither
//! the checkpoint attempt budget nor a native `max_retries` slot. A
//! cap-sized job finishes in ~30 continuations, each asking only the
//! batches its tally has not yet recorded.
//!
//! Runtime boundaries: the pool probe (`Probing`, via
//! [`crate::pool_probe`]), the Verifying step (`Verifying`, via
//! [`crate::verify`] over [`crate::pool_body`] and the scratch bucket),
//! `Repairing` (the repair Worker through the `REPAIR_CONTAINER`
//! service binding, fed by verify's staged inputs and returning to
//! `Verifying`), and `Matching`/`Writing` are executable. Indexer
//! search (hence probing for search-sourced jobs) has no executable
//! consumer wiring yet, so those states settle as transient failures
//! through the shared retry budget. The consumer therefore stays
//! disabled in `wrangler.toml.example` until that boundary lands.

use futures_util::{Stream, StreamExt};
use worker::{console_log, Date, Env, MessageBuilder, MessageExt, QueueRetryOptionsBuilder};

use crate::dispatch::{
    content_sha256_hex, make_job_id, validate_user_id, INGEST_QUEUE_BINDING, PAYLOAD_KEY_PREFIX,
    PAYLOAD_KV_BINDING,
};
use crate::jobs::{
    advance_checkpoint, validate_job_id, JobCheckpoint, JobSource, JobState, MAX_ATTEMPTS,
    MAX_REPAIR_ROUNDS,
};
use crate::newznab::{MAX_CATEGORIES, MAX_LIMIT, MAX_PAGE};
use crate::parse::{ParsedFile, ParsedNzb};
use crate::pool_body::{build_body_request, call_pool_body, BodyFetch, PoolBodyError};
use crate::pool_probe::{call_pool_probe, check_outcome, PoolProbeError};
use crate::probe::{
    apply_probe_batch, build_probe_request_with_ranges, ProbeOutcome, ProbeRequest, ProbeTally,
};
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
/// Most probe batches one delivery may ask about. A batch is at most
/// [`crate::probe::MAX_PROBE_SEGMENTS`] (500) sequential `STAT`
/// round-trips on the pool side, so this caps one message at 2 000
/// round-trips — about 30 s at a nominal 15 ms provider RTT, the same
/// bound as [`PROBE_DELIVERY_BUDGET_MS`]. The batch cap is the
/// deterministic half of the budget; the wall cap catches providers
/// much slower than nominal between batches. Both halves are pinned
/// against the invocation-limit arithmetic by the const asserts below.
const MAX_PROBE_BATCHES_PER_DELIVERY: usize = 4;
/// Most wall time one delivery may spend probing, checked between
/// batches (overshoot is at most the one in-flight batch). Sized so a
/// full consumer invocation — `max_batch_size = 10` messages processed
/// one after another, each spending the whole budget — uses 300 s of
/// the 15-minute invocation limit, leaving the rest for payload
/// fetch/parse, the checkpoint persist, and slower-than-nominal RTTs.
const PROBE_DELIVERY_BUDGET_MS: u64 = 30_000;
/// Queue-side delay on a probe continuation copy, matching the first
/// shared-backoff step: the just-persisted checkpoint gets a moment to
/// propagate before the continuation reads it. A stale read is safe in
/// any case — it re-asks already-folded batches, and `STAT` is
/// idempotent with the tally keyed by batch index.
const PROBE_CONTINUATION_DELAY_SECS: u32 = 5;

// Compile-time: the two budget halves agree at the nominal 15 ms
// sequential STAT round-trip, and a full consumer invocation
// (`max_batch_size = 10` in wrangler.toml.example — keep in sync) of
// worst-case messages, each spending its whole wall budget, stays
// inside the 15-minute invocation limit with 3x headroom.
const _: () = assert!(
    MAX_PROBE_BATCHES_PER_DELIVERY as u64 * crate::probe::MAX_PROBE_SEGMENTS as u64 * 15
        == PROBE_DELIVERY_BUDGET_MS
);
const _: () = assert!(PROBE_DELIVERY_BUDGET_MS * 10 < 15 * 60 * 1_000);

/// Wall-clock budget for one Verifying delivery; the step checkpoints
/// per file and resumes on redelivery past this.
const VERIFY_WALL_CLOCK_BUDGET_MS: u64 = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Settlement {
    Ack,
    /// Progress continuation: enqueue a fresh copy of the message and
    /// ack the original (see [`crate::retry::Disposition::Continue`]).
    Continue,
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
            let mut expected = crate::dispatch::build_search_dispatch(
                &msg.user_id,
                query,
                indexer_ref,
                &msg.categories,
                limit,
                page,
            )
            .map_err(|_| "invalid search input")?;
            // Trace IDs are observability metadata, not job identity: the
            // route stamps them after building, and the consumer re-stamps
            // every delivery, so they stay out of the identity comparison.
            expected.trace_id = msg.trace_id.clone();
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

/// Run the `Repairing` step: dispatch the recorded repair request to
/// the repair Worker, then return to `Verifying` on success (the next
/// delivery re-runs the verify step). Note the re-verify feedback loop
/// is still open: `Verifying` re-fetches from the pool and does not yet
/// consume the repair round's `out/` objects, so this step must not be
/// exercised live until that consumption lands (the consumer stays
/// disabled meanwhile). A checkpoint in `Repairing` without recorded
/// verify inputs is a permanent failure: there is nothing a redelivery
/// could add.
async fn run_repair_step(env: &Env, cp: &mut JobCheckpoint) -> Result<(), JobError> {
    use crate::repair::RepairCall;

    let Some(par2_set_id) = cp.par2_set_id.clone() else {
        return Err(JobError::permanent(
            ERR_REPAIR_FAILED,
            "repair inputs missing from checkpoint",
        ));
    };
    let req = match crate::repair::decide_repair(
        &cp.job_id,
        &cp.user_id,
        &cp.idempotency_key,
        &par2_set_id,
        cp.missing_blocks,
        cp.repair_rounds,
    ) {
        Ok(Some(req)) => req,
        Ok(None) => {
            advance_checkpoint(cp, false);
            return Err(JobError::progress(
                ERR_VERIFY_FAILED,
                "ingest runtime boundary unavailable",
            ));
        }
        Err(_) => {
            return Err(JobError::permanent(
                ERR_REPAIR_FAILED,
                "repair request cannot be built from checkpoint",
            ))
        }
    };
    match crate::repair::call_repair(env, &req).await {
        RepairCall::Repaired => {
            advance_checkpoint(cp, false);
            Err(JobError::progress(
                ERR_VERIFY_FAILED,
                "ingest runtime boundary unavailable",
            ))
        }
        RepairCall::Unrepairable => Err(JobError::permanent(
            ERR_REPAIR_FAILED,
            "par2 repair reported the set unrepairable",
        )),
        RepairCall::Retry => Err(JobError::transient(
            ERR_REPAIR_FAILED,
            "repair runtime unavailable",
        )),
    }
}

/// Map a pool probe refusal/failure onto the job's error contract. The
/// detail stays generic: pool/provider internals never reach job status
/// or the DLQ reason.
fn pool_probe_job_error(error: PoolProbeError) -> JobError {
    match error {
        PoolProbeError::Retry { after_secs } => {
            JobError::transient_after(ERR_PROBE_FAILED, "pool probe unavailable", after_secs)
        }
        PoolProbeError::Rejected => {
            JobError::permanent(ERR_PROBE_FAILED, "pool probe rejected the request")
        }
    }
}

/// Prepare the checkpoint tally for `batches`: create it on first entry
/// to `Probing`, and refuse a stored tally that cannot belong to this
/// parse (different batch count, out-of-range index, or a missing count
/// larger than its batch). The payload is content-addressed and the
/// parser deterministic, so a mismatch is corrupt checkpoint state:
/// permanent, not something a redelivery can repair.
fn prepare_probe_tally<T>(cp: &mut JobCheckpoint, batches: &[Vec<T>]) -> Result<(), JobError> {
    let inconsistent = || {
        JobError::permanent(
            ERR_PROBE_FAILED,
            "probe tally disagrees with the parsed NZB",
        )
    };
    match cp.probe_tally.as_ref() {
        None => {
            cp.probe_tally = Some(ProbeTally::new(batches.len()));
            Ok(())
        }
        Some(tally) if tally.total_batches != batches.len() => Err(inconsistent()),
        Some(tally) => {
            for (batch_index, missing) in &tally.missing_by_batch {
                match batches.get(*batch_index) {
                    Some(batch) if (*missing as usize) <= batch.len() => {}
                    _ => return Err(inconsistent()),
                }
            }
            Ok(())
        }
    }
}

/// Ask the pool about batches the tally has not yet recorded, folding
/// each answer in as it lands ([`apply_probe_batch`]), until every batch
/// has reported or this delivery's budget runs out. Returns the total
/// missing-segment count once complete, with `cp` moved to `Verifying`
/// and the completed tally retained for that step to read.
///
/// The budget (`max_batches` answered batches, `budget_expired` for the
/// wall-time half, polled between batches) is what keeps the delivery —
/// and the up-to-10-message invocation around it — well inside the
/// 15-minute consumer limit (see the module docs). Stopping on budget
/// returns [`JobError::progress`]: the batches folded this delivery ride
/// the settlement persist and the continuation delivery asks only the
/// remainder. The budget is only honored *after* at least one new batch
/// has folded, so a continuation always carries progress and the
/// sequence terminates; a stop with the tally unchanged degrades to an
/// ordinary transient failure inside the attempt budget.
///
/// An answer is checked against its request before folding (the live
/// caller, [`call_pool_probe`], checks too; the driver refuses an
/// unpartitioned answer regardless of caller). On a retryable pool
/// failure the batches folded so far stay in `cp.probe_tally`, so the
/// delivery's settlement persist carries them and the redelivery only
/// asks the remainder. Injected `call` and `budget_expired` keep the
/// fold policy unit-testable without a Durable Object binding or a
/// live clock.
async fn run_probing_batches<C, Fut>(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    batches: &[Vec<(String, Option<u64>)>],
    max_batches: usize,
    budget_expired: &dyn Fn() -> bool,
    mut call: C,
) -> Result<u32, JobError>
where
    C: FnMut(ProbeRequest) -> Fut,
    Fut: core::future::Future<Output = Result<ProbeOutcome, PoolProbeError>>,
{
    if cp.state != JobState::Probing {
        // Never spend a provider connection from the wrong state; the
        // caller settles this like any other unavailable boundary.
        return Err(JobError::transient(
            unavailable_code(cp.state),
            "ingest runtime boundary unavailable",
        ));
    }
    if batches.is_empty() {
        // A zero-batch tally would read as trivially complete; intake
        // rejects segment-less NZBs, so this is a parse contract bug.
        return Err(JobError::permanent(
            ERR_PARSE_FAILED,
            "NZB has no segments to probe",
        ));
    }
    prepare_probe_tally(cp, batches)?;
    let (folded_at_start, pending): (usize, Vec<usize>) = {
        let tally = cp.probe_tally.as_ref().expect("tally prepared above");
        (
            tally.missing_by_batch.len(),
            (0..tally.total_batches)
                .filter(|batch_index| !tally.missing_by_batch.contains_key(batch_index))
                .collect(),
        )
    };
    // A budget stop is only a continuation when the tally actually grew;
    // otherwise it must burn the shared attempt budget like any other
    // transient failure, or a no-progress job could re-enqueue forever.
    let stop_error = |cp: &JobCheckpoint| {
        let grew = cp
            .probe_tally
            .as_ref()
            .is_some_and(|tally| tally.missing_by_batch.len() > folded_at_start);
        if grew {
            JobError::progress(ERR_PROBE_FAILED, "pool probe continuing")
        } else {
            JobError::transient(ERR_PROBE_FAILED, "pool probe incomplete")
        }
    };
    // At least one batch is always asked, whatever the budget says:
    // progress this delivery is the precondition for continuing at all.
    let max_batches = max_batches.max(1);
    for (asked, batch_index) in pending.into_iter().enumerate() {
        if asked >= max_batches || (asked > 0 && budget_expired()) {
            return Err(stop_error(cp));
        }
        let segments: Vec<(&str, Option<u64>)> = batches[batch_index]
            .iter()
            .map(|(id, bytes)| (id.as_str(), *bytes))
            .collect();
        // The parser cross-check already validated these ids, so a build
        // failure here is a contract bug: same generic permanent refusal
        // as a pool-side rejection.
        let request = build_probe_request_with_ranges(msg, batch_index, &segments)
            .map_err(|_| pool_probe_job_error(PoolProbeError::Rejected))?;
        let outcome = call(request.clone()).await.map_err(pool_probe_job_error)?;
        check_outcome(&request, &outcome).map_err(pool_probe_job_error)?;
        let tally = cp.probe_tally.as_mut().expect("tally prepared above");
        let (next, _) = apply_probe_batch(cp.state, tally, &outcome);
        cp.state = next;
    }
    let tally = cp.probe_tally.as_ref().expect("tally prepared above");
    if !tally.is_complete() {
        // Defensive: the pending snapshot covered every unrecorded batch,
        // so incompleteness means an answer folded against a different
        // index than asked. Resume from the tally: a continuation when
        // this delivery did fold something new, a budgeted retry when
        // it folded nothing.
        return Err(stop_error(cp));
    }
    cp.state = JobState::Verifying;
    Ok(tally.missing_segments())
}

/// Fetch and digest-check the upload payload, then run the intake guard
/// and both shared-parser projections (flat for the probe, per-file for
/// Verifying). Re-parsing on every `Parsing`/`Probing`/`Verifying`
/// delivery is deliberate: the checkpoint stores no parse, and the
/// content-addressed payload plus deterministic parser make the
/// re-parse converge on the same batches, so the parse can never
/// disagree with the bytes the digest pins.
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

/// Run the executable part of one delivery, mutating `cp` in memory only.
///
/// Upload jobs run `Parsing` (digest-check + shared parser), `Probing`
/// (pool STAT batches folded into the checkpoint tally on this
/// delivery's probe budget, [`run_probing_batches`] — including on the
/// same delivery that parsed, so a fresh job does not spend a retry
/// just to change state) and `Verifying` (pool BODY fetch, par2
/// verify and scratch staging). A probing budget stop — and the
/// Probing -> Verifying state change itself — settles as a progress
/// continuation, and the continuation delivery resumes from the
/// persisted checkpoint; other state-advancing steps settle
/// transiently so the next delivery runs the next state. `Repairing`
/// dispatches to the repair Worker (see [`run_repair_step`]).
/// `Matching`/`Writing` run for real ([`run_match_write`]). Every
/// other non-terminal state (including `Probing` for search-sourced
/// jobs, whose NZB bytes do not exist until the search step is wired)
/// is a transient boundary failure today.
async fn run_step(env: &Env, msg: &QueueMsg, cp: &mut JobCheckpoint) -> Result<(), JobError> {
    if cp.state == JobState::Queued {
        advance_checkpoint(cp, false);
    }
    if cp.state == JobState::Repairing {
        return run_repair_step(env, cp).await;
    }
    if matches!(cp.state, JobState::Matching | JobState::Writing) {
        return run_match_write(env, msg, cp).await;
    }
    if !(cp.source == JobSource::NzbUpload
        && matches!(
            cp.state,
            JobState::Parsing | JobState::Probing | JobState::Verifying
        ))
    {
        return Err(JobError::transient(
            unavailable_code(cp.state),
            "ingest runtime boundary unavailable",
        ));
    }
    let (parsed, files) = load_upload_parse(env, msg).await?;
    match cp.state {
        JobState::Parsing | JobState::Probing => {
            if cp.state == JobState::Parsing {
                advance_checkpoint(cp, false);
            }
            let batches = crate::parse::probe_batches_with_ranges(&parsed);
            let probing_started_ms = Date::now().as_millis();
            run_probing_batches(
                msg,
                cp,
                &batches,
                MAX_PROBE_BATCHES_PER_DELIVERY,
                &|| {
                    Date::now().as_millis().saturating_sub(probing_started_ms)
                        >= PROBE_DELIVERY_BUDGET_MS
                },
                |request| async move { call_pool_probe(env, &request).await },
            )
            .await?;
            // Probing completed and advanced the checkpoint to
            // Verifying: continuing to the next state is progress, not
            // a failure, so it must not spend the attempt budget.
            Err(JobError::progress(ERR_VERIFY_FAILED, "verifying pending"))
        }
        JobState::Verifying => run_verifying(env, msg, cp, &files).await,
        _ => unreachable!("state filtered above"),
    }
}

/// Map one verify outcome onto the checkpoint: advance on `Complete` and
/// on a dispatched repair, never move the state otherwise (permanent
/// step errors are failed by `decide_checkpoint`, which keeps the
/// settlement persist planner seeing a non-terminal checkpoint). Pure
/// over the checkpoint so the mapping is unit-tested without bindings.
/// `made_progress` is whether the step staged another file, spooled
/// another segment, or advanced a streaming judgement during this
/// delivery; it decides if an `Incomplete` step spends retry budget.
fn apply_verify_outcome(
    msg: &QueueMsg,
    cp: &mut JobCheckpoint,
    outcome: VerifyOutcome,
    made_progress: bool,
) -> Result<(), JobError> {
    match outcome {
        VerifyOutcome::Complete => {
            // decide_repair is the single dispatch authority: zero
            // missing blocks must come back `Ok(None)` (it returns that
            // before any round validation).
            match crate::repair::decide_repair(
                &msg.job_id,
                &msg.user_id,
                &msg.idempotency_key,
                "",
                0,
                cp.repair_rounds.saturating_add(1),
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
            // The checkpoint state advanced: this transition is
            // progress, not a failure. Settling it transient would
            // spend an attempt per state boundary, so a job needing
            // repair rounds could exhaust MAX_ATTEMPTS on transitions
            // alone (and a Complete at the attempt cap would fail).
            Err(JobError::progress(ERR_MATCH_FAILED, "matching pending"))
        }
        VerifyOutcome::RepairNeeded {
            par2_set_id,
            missing_blocks,
        } => {
            // The exhausted budget fails before any dispatch
            // validation: decide_repair would reject the out-of-budget
            // round with its own reason, masking this one.
            if cp.repair_rounds >= MAX_REPAIR_ROUNDS {
                return Err(JobError::permanent(
                    ERR_REPAIR_FAILED,
                    "repair budget exhausted",
                ));
            }
            // Validate the dispatch with the round about to be spent
            // (advance_checkpoint increments repair_rounds into
            // Repairing, and run_repair_step re-derives the same
            // request from the checkpoint inputs recorded below).
            match crate::repair::decide_repair(
                &msg.job_id,
                &msg.user_id,
                &msg.idempotency_key,
                &par2_set_id,
                missing_blocks,
                cp.repair_rounds.saturating_add(1),
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
            // Record the repair inputs on the checkpoint:
            // run_repair_step rebuilds the request from them.
            cp.par2_set_id = Some(par2_set_id);
            cp.missing_blocks = missing_blocks;
            cp.probe_tally = None;
            advance_checkpoint(cp, true);
            Err(JobError::progress(ERR_REPAIR_FAILED, "repair pending"))
        }
        VerifyOutcome::Unrecoverable {
            size_refused_blocks,
            ..
        } => {
            // Size-refused damage fails under its own reason: triage
            // reads "over cap" here, not "provider lost it" — and no
            // `decide_repair` dispatch is spent on either arm.
            if size_refused_blocks > 0 {
                // Worker-only line (`console_log!` aborts off-wasm):
                // the terminal reason below already distinguishes the
                // refusal class on every target.
                #[cfg(target_arch = "wasm32")]
                console_log!(
                    "ingest verify unrecoverable: size-refused damage is terminal job={} refused={}",
                    msg.job_id,
                    size_refused_blocks
                );
                return Err(JobError::permanent(
                    ERR_VERIFY_FAILED,
                    "release has size-refused blocks beyond repair",
                ));
            }
            Err(JobError::permanent(
                ERR_VERIFY_FAILED,
                "release is missing blocks beyond par2 recovery",
            ))
        }
        // A capped step that staged/spooled more is progress, not a
        // failure: redeliver without spending the retry budget, or a
        // valid large upload would be quarantined while every delivery
        // succeeds. No growth keeps the ordinary transient budget, so a
        // step that can never stage anything still exhausts and DLQs.
        VerifyOutcome::Incomplete if made_progress => Err(JobError::progress(
            ERR_VERIFY_FAILED,
            "verifying in progress",
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
    // Scratch rounds are 1-based (the repair scratch contract's
    // `r{round}`), and the round repair will be dispatched for is
    // `repair_rounds + 1` — so verify stages its inputs under that
    // round, and a post-repair re-verify (repair_rounds spent) stages
    // the next round.
    let round = cp.repair_rounds.saturating_add(1);
    let mut progress = match loaded {
        Some(progress) if progress.round == round => progress,
        _ => VerifyProgress::new(round),
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
    let staged_before = progress.staged.len();
    let spooled_before: usize = progress.spool.values().map(Vec::len).sum();
    let judged_before: u64 = progress.judging.values().map(|c| c.pos).sum();
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
    let spooled_after: usize = progress.spool.values().map(Vec::len).sum();
    let judged_after: u64 = progress.judging.values().map(|c| c.pos).sum();
    // Durable segment-level spool growth is progress exactly like a
    // newly staged file: a file spanning several deliveries (more
    // segments than one delivery's budget) must not burn the attempt
    // budget while every delivery succeeds. The same holds for a
    // streaming judgement whose walk advanced before the deadline
    // stopped it: the persisted judging cursor makes that advance
    // durable.
    let made_progress = progress.staged.len() > staged_before
        || spooled_after > spooled_before
        || judged_after > judged_before;
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
        Ok(outcome) => apply_verify_outcome(msg, cp, outcome, made_progress),
        Err(VerifyError::Transient(detail)) => Err(JobError::transient(ERR_VERIFY_FAILED, detail)),
        Err(VerifyError::Permanent(detail)) => Err(JobError::permanent(ERR_VERIFY_FAILED, detail)),
    }
}

/// [`BodySource`] over the pool's BODY route. A pool `Retry-After`
/// collapses into the consumer's shared checkpoint backoff, exactly as
/// on the probe path (`run_probing_batches`): the settlement contract
/// has one delay schedule, and the hint only orders retries within it.
struct PoolBodySource<'a> {
    env: &'a Env,
    msg: &'a QueueMsg,
}

impl BodySource for PoolBodySource<'_> {
    async fn fetch_body(
        &mut self,
        segment_id: &str,
    ) -> Result<crate::verify::ArticleFetch, VerifyError> {
        let req = build_body_request(self.msg, segment_id)
            .map_err(|_| VerifyError::Permanent("invalid segment id".into()))?;
        match call_pool_body(self.env, &req).await {
            Ok(BodyFetch::Bytes(bytes)) => Ok(crate::verify::ArticleFetch::Present(bytes)),
            Ok(BodyFetch::Missing) => Ok(crate::verify::ArticleFetch::Missing),
            // `413` is per-article data: the segment is judged absent
            // and flagged size-refused (never a permanent job failure
            // by itself), so the round can skip the repair dispatch a
            // refetch-identical refusal could never satisfy.
            Ok(BodyFetch::TooLarge) => Ok(crate::verify::ArticleFetch::TooLarge),
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
        // The recorded logical size wins over the object size: a spool
        // index object's bytes are the JSON segment list, not the file.
        // Objects written before the `size` metadata existed are
        // assembled files, whose object size is the file size.
        size: meta
            .get("size")
            .and_then(|v| v.parse().ok())
            .unwrap_or(size),
        sha256: meta.get("sha256")?.clone(),
        // An absent/unparseable block count must not read as "intact":
        // refuse the record (`None`) so the file is fetched and judged
        // again instead of adopted as healthy sight unseen. The refusal
        // count instead backfills 0: records written before the field
        // existed predate refusal tracking, and 0 preserves their
        // repair-eligible adoption exactly as judged then.
        missing_blocks: meta.get("missing_blocks")?.parse().ok()?,
        size_refused_blocks: meta
            .get("size_refused_blocks")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        par2_set_id: meta.get("par2_set_id").cloned(),
        spool: Vec::new(),
        crc32: meta.get("crc32").and_then(|v| v.parse().ok()).unwrap_or(0),
        missing: meta.get("missing").is_some_and(|v| v == "1"),
    })
}

/// The R2 custom-metadata map [`staged_from_metadata`] parses back: the
/// refusal count rides alongside the judgement so a resume adopts it
/// without re-reading bytes. Extracted so the write side is unit-pinned
/// together with the parse side (see the round-trip tests).
fn staged_to_metadata(meta: &StagedFile) -> std::collections::HashMap<String, String> {
    let mut md = std::collections::HashMap::new();
    md.insert("sha256".to_string(), meta.sha256.clone());
    md.insert("size".to_string(), meta.size.to_string());
    md.insert("crc32".to_string(), meta.crc32.to_string());
    if meta.missing {
        md.insert("missing".to_string(), "1".to_string());
    }
    md.insert(
        "missing_blocks".to_string(),
        meta.missing_blocks.to_string(),
    );
    md.insert(
        "size_refused_blocks".to_string(),
        meta.size_refused_blocks.to_string(),
    );
    if let Some(set_id) = &meta.par2_set_id {
        md.insert("par2_set_id".to_string(), set_id.clone());
    }
    md
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
        let md = staged_to_metadata(meta);
        self.bucket
            .put(key.to_string(), bytes)
            .custom_metadata(md)
            .execute()
            .await
            .map_err(|_| VerifyError::Transient("scratch write failed".into()))?;
        Ok(())
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, VerifyError> {
        // Follow the listing cursor until the prefix is exhausted:
        // one R2 operation per 1,000-key page.
        let mut keys = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut list = self.bucket.list().prefix(prefix.to_string());
            if let Some(cursor) = cursor {
                list = list.cursor(cursor);
            }
            let page = list
                .execute()
                .await
                .map_err(|_| VerifyError::Transient("scratch list failed".into()))?;
            keys.extend(page.objects().iter().map(|object| object.key()));
            if !page.truncated() {
                return Ok(keys);
            }
            let Some(next) = page.cursor() else {
                return Err(VerifyError::Transient("scratch list failed".into()));
            };
            cursor = Some(next);
        }
    }
}

/// Cap on the MB search response body read into memory (25 small JSON
/// hits; anything larger is a contract break, handled as transient).
const MAX_MB_RESPONSE_BYTES: usize = 64 * 1024;

/// Cap on the MB release-recordings response body (the route returns at
/// most 1000 recording objects; at a few hundred JSON bytes each, 1 MiB
/// is generous headroom — anything larger is a contract break, handled
/// as transient like the search cap).
const MAX_MB_RECORDINGS_RESPONSE_BYTES: usize = 1024 * 1024;

/// Total deadline for one MB body read (ms): exactly the indexer client's
/// total (`crate::indexer_fetch::FETCH_TIMEOUT_MS`, the reused vault
/// total), so both untrusted-upstream reads share one budget that composes
/// with the existing retry budget instead of inventing a second number.
/// Armed in `fetch_mb_body` before the fetch is issued, so slow headers
/// count against the same total as a stalled body.
pub const MB_FETCH_DEADLINE_MS: u64 = 15_000;

/// Why an MB body read failed. Every variant classifies as `Transient`
/// (see [`MbFetchFailure::match_failure`]): a stalled mirror may answer
/// on redelivery, exactly like a 5xx, an oversize body, or a non-UTF-8
/// body today — never `Permanent`, never a partial body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MbFetchFailure {
    /// The body was still incomplete when the total deadline fired.
    Deadline,
    /// A chunk would have pushed the buffered total over `max_bytes`.
    Oversize,
    /// The buffered bytes are not valid UTF-8.
    NonUtf8,
    /// A chunk-level transport error mid-body.
    Transport,
}

impl MbFetchFailure {
    /// Every MB fetch failure stays retryable through the existing retry
    /// budget, never quarantined as permanent.
    pub fn match_failure(self) -> crate::matching::MatchFailure {
        match self {
            MbFetchFailure::Deadline
            | MbFetchFailure::Oversize
            | MbFetchFailure::NonUtf8
            | MbFetchFailure::Transport => crate::matching::MatchFailure::Transient,
        }
    }
}

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
/// `max_bytes` and aborted at [`MB_FETCH_DEADLINE_MS`]. Every failure mode
/// (missing binding, transport, oversize body, non-UTF-8, stalled body
/// past the deadline) collapses to `Err(())`: callers classify all of them
/// as transient, matching the retry-budget split in `matching`.
async fn fetch_mb_body(env: &Env, url: String, max_bytes: usize) -> Result<(u16, String), ()> {
    // Arm the total deadline BEFORE issuing the fetch so slow headers
    // count against the same budget as a stalled body. `worker::Delay`
    // only registers its `setTimeout` on the first `poll`, so merely
    // constructing it before the service-binding fetch is issued would
    // leave the headers wait unbounded; racing both futures here polls
    // (arms) the deadline from the start and reuses the same deadline
    // for the body drain below, so one 15 s budget spans headers plus
    // body.
    let mut deadline = Box::pin(worker::Delay::from(std::time::Duration::from_millis(
        MB_FETCH_DEADLINE_MS,
    )));
    let fetcher = env
        .service(crate::matching::MB_API_BINDING)
        .map_err(|_| ())?;
    let mut fetch = Box::pin(fetcher.fetch(url, None));
    let response = std::future::poll_fn(|cx| {
        use std::task::Poll;
        // Deadline first: an already-fired deadline aborts even when
        // headers are also ready, matching `collect_mb_body` below.
        // (Fully qualified: `Future` is not imported in this module, the
        // same reason `collect_mb_body` calls `Stream::poll_next` by path.)
        if std::future::Future::poll(deadline.as_mut(), cx).is_ready() {
            return Poll::Ready(None);
        }
        match std::future::Future::poll(fetch.as_mut(), cx) {
            Poll::Ready(response) => Poll::Ready(Some(response)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await;
    let response = match response {
        Some(Ok(response)) => response,
        Some(Err(_)) | None => return Err(()),
    };
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .map(|chunk| chunk.map(|bytes| bytes.to_vec()).map_err(|_| ()));
    let text = match collect_mb_body(Box::pin(body), max_bytes, deadline).await {
        Ok(text) => text,
        Err(_) => {
            return Err(());
        }
    };
    Ok((status, text))
}

/// Drain one MB body stream with a byte cap and a total deadline: the seam
/// `fetch_mb_body` drains through, proven by the `mb_fetch_deadline`
/// acceptance suite.
///
/// The deadline is TOTAL for the whole drain, not per-chunk idle time: one
/// `deadline` future spans every chunk, partial progress never re-arms it,
/// and a body still incomplete when `deadline` resolves aborts whole with
/// [`MbFetchFailure::Deadline`] — never a partial body, never `Ok`.
///
/// Before the deadline fires the behavior is byte-for-byte the old inline
/// loop: chunks accumulate (multi-byte UTF-8 split across chunks decodes),
/// the exact `max_bytes` fit is admitted, the chunk that would cross
/// `max_bytes` fails [`MbFetchFailure::Oversize`], invalid UTF-8 fails
/// [`MbFetchFailure::NonUtf8`], a chunk-level transport error fails
/// [`MbFetchFailure::Transport`], and an empty body is `Ok("")`.
pub async fn collect_mb_body<S, D>(
    body: S,
    max_bytes: usize,
    deadline: D,
) -> Result<String, MbFetchFailure>
where
    S: Stream<Item = Result<Vec<u8>, ()>> + Unpin,
    D: std::future::Future<Output = ()> + Unpin,
{
    use std::task::Poll;
    let mut body = body;
    let mut deadline = deadline;
    let mut bytes = Vec::new();
    loop {
        enum Turn {
            Chunk(Option<Result<Vec<u8>, ()>>),
            Deadline,
        }
        let turn = std::future::poll_fn(|cx| {
            // Deadline first: an already-fired deadline aborts even when a
            // chunk is also ready, so no completed prefix escapes as a
            // truncated body.
            if std::pin::Pin::new(&mut deadline).poll(cx).is_ready() {
                return Poll::Ready(Turn::Deadline);
            }
            match Stream::poll_next(std::pin::Pin::new(&mut body), cx) {
                Poll::Ready(chunk) => Poll::Ready(Turn::Chunk(chunk)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        match turn {
            Turn::Deadline => return Err(MbFetchFailure::Deadline),
            Turn::Chunk(None) => break,
            Turn::Chunk(Some(Ok(chunk))) => {
                if bytes.len() + chunk.len() > max_bytes {
                    return Err(MbFetchFailure::Oversize);
                }
                bytes.extend_from_slice(&chunk);
            }
            Turn::Chunk(Some(Err(()))) => return Err(MbFetchFailure::Transport),
        }
    }
    String::from_utf8(bytes).map_err(|_| MbFetchFailure::NonUtf8)
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
    Ok(crate::matching::pick_release_mbid(query, &hits))
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
        let affected = client
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
        // The statement is fenced on the owner's users row: 0 affected
        // rows means the account is being deleted. Fail permanently with
        // the stable code — a fenced writer never queues behind the
        // delete, and the (already-metered) match work must not settle
        // as a phantom Done for a row that was never written. The mapping
        // lives in [`crate::library::upsert_job_result`] (pinned there),
        // so this arm stays a forward.
        crate::library::upsert_job_result(affected)
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
        // A continuation's whole point is the persisted progress, so it
        // always takes the delivery's one write before the fresh copy
        // is enqueued.
        Ok(Disposition::Continue) => (true, Settlement::Continue),
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
    match settlement {
        // The persisted tally is the continuation's payload: enqueue a
        // fresh copy of the (identity-stable) message — a new native
        // retry count for a delivery that spent no attempt — and ack
        // the original. On enqueue failure fall back to a native retry,
        // which resumes from the same persisted tally; never ack, or
        // the job would stall mid-probe with no delivery outstanding.
        Settlement::Continue => {
            if send_continuation(env, msg).await {
                Settlement::Ack
            } else {
                Settlement::RetryAfter(UNPERSISTED_RETRY_SECS)
            }
        }
        other => other,
    }
}

/// Enqueue the fresh copy a progress continuation continues on, delayed
/// by [`PROBE_CONTINUATION_DELAY_SECS`]. A send/ack race can deliver the
/// job twice; the batch-index-keyed tally and checkpoint identity make
/// that an idempotent re-ask, never a double-count.
async fn send_continuation(env: &Env, msg: &QueueMsg) -> bool {
    match env.queue(INGEST_QUEUE_BINDING) {
        Ok(queue) => {
            match queue
                .send(
                    MessageBuilder::new(msg.clone())
                        .delay_seconds(PROBE_CONTINUATION_DELAY_SECS)
                        .build(),
                )
                .await
            {
                Ok(()) => true,
                Err(error) => {
                    console_log!("ingest continuation enqueue failed: {:?}", error);
                    false
                }
            }
        }
        Err(error) => {
            console_log!("ingest queue binding missing: {:?}", error);
            false
        }
    }
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
    let mut msg = match serde_json::from_value::<QueueMsg>(value) {
        Ok(msg) => msg,
        Err(_) => {
            retry_raw(&raw, UNPERSISTED_RETRY_SECS);
            return;
        }
    };
    // The route stamped this job's trace on the message; keep a valid ID so
    // the consumer's line correlates with the intake lines, and hand a valid
    // ID to any continuation this delivery enqueues. An invalid value is
    // replaced, never trusted or logged. Only opaque job facts enter the line.
    let trace_id = nntune_logging::resolve_trace_id(msg.trace_id.as_deref());
    msg.trace_id = Some(trace_id.clone());
    // Build identity rides every line (mirrors the api worker and the fetch
    // routes above): the consumer runs in the same deployment, so its own
    // env carries the same public BUILD_SHA var, or explicit "unknown".
    let build_sha = crate::build_sha_from_env(env);
    crate::emit(
        nntune_logging::Level::Info,
        &trace_id,
        "ingest.job.consumed",
        serde_json::json!({"job_id": msg.job_id, "build_sha": build_sha}),
    );
    match process_message(env, &msg).await {
        Settlement::Ack => raw.ack(),
        // `process_message` resolves a continuation into Ack (fresh copy
        // enqueued) or a native retry, so this never reaches here; if it
        // ever did, retry rather than ack a job with no copy outstanding.
        Settlement::Continue => retry_raw(&raw, UNPERSISTED_RETRY_SECS),
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
    fn traced_search_messages_keep_their_identity() {
        // The route stamps the resolved inbound trace after building, and the
        // consumer re-stamps every delivery: the trace must not affect the
        // identity comparison, or no search job ever runs.
        let mut traced = search_message();
        traced.trace_id = Some("9f3a1b2c4d5e6f708192a3b4c5d64e6f".into());
        assert!(validate_queue_msg(&traced).is_ok());

        // A tampered search still fails with a trace present: leaving the
        // trace out of the comparison must not mask an identity mismatch.
        let mut tampered = traced.clone();
        tampered.query = Some("other query".into());
        assert!(validate_queue_msg(&tampered).is_err());

        // A legacy untraced message, stamped on delivery the way
        // `process_raw_message` does, validates too.
        let mut legacy = search_message();
        assert!(legacy.trace_id.is_none());
        let trace_id = nntune_logging::resolve_trace_id(legacy.trace_id.as_deref());
        legacy.trace_id = Some(trace_id);
        assert!(validate_queue_msg(&legacy).is_ok());
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
        failed.failure_reason = Some("pool probe unavailable".into());
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
        let step =
            apply_verify_outcome(&msg, &mut cp, crate::verify::VerifyOutcome::Complete, true);
        // The next state runs on the next delivery: the advanced
        // checkpoint settles as progress, spending no attempt (a
        // transition is not a failure).
        let err = step.expect_err("matching runs on the next delivery");
        assert!(err.transient);
        assert!(err.made_progress);
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
            true,
        );
        let err = step.expect_err("repair runs on the next delivery");
        assert!(err.transient);
        assert!(err.made_progress);
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
            true,
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
                size_refused_blocks: 0,
            },
            true,
        )
        .expect_err("unrecoverable");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_VERIFY_FAILED);
        assert_eq!(cp.state, JobState::Verifying);

        // Incomplete with no staged growth is an ordinary transient
        // failure: it spends retry budget in decide_checkpoint.
        let err = apply_verify_outcome(
            &msg,
            &mut cp,
            crate::verify::VerifyOutcome::Incomplete,
            false,
        )
        .expect_err("incomplete");
        assert!(err.transient);
        assert!(!err.made_progress);
        assert_eq!(err.code, ERR_VERIFY_FAILED);
        assert_eq!(cp.state, JobState::Verifying);

        // Incomplete after staging more files is progress: the
        // redelivery must not spend the retry budget.
        let err = apply_verify_outcome(
            &msg,
            &mut cp,
            crate::verify::VerifyOutcome::Incomplete,
            true,
        )
        .expect_err("incomplete with progress");
        assert!(err.transient);
        assert!(err.made_progress);
        assert_eq!(err.code, ERR_VERIFY_FAILED);
        assert_eq!(cp.state, JobState::Verifying);
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

    // --- Probing driver (pool probe fold / resume policy) ---

    use crate::pool_probe::PoolProbeError;
    use crate::probe::{ProbeOutcome, ProbeRequest, ProbeTally};

    type ProbeBatches = Vec<Vec<(String, Option<u64>)>>;

    fn probing_setup() -> (QueueMsg, JobCheckpoint, ProbeBatches) {
        let msg = build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB).unwrap();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Probing;
        let batches = vec![
            vec![("seg-a@synthetic.invalid".to_string(), Some(100))],
            vec![
                ("seg-b@synthetic.invalid".to_string(), Some(200)),
                ("seg-c@synthetic.invalid".to_string(), None),
            ],
        ];
        (msg, cp, batches)
    }

    /// Drive the probing loop with a scripted synchronous pool fake. The
    /// fake resolves without yielding, so no executor is needed (same
    /// pattern as the admission-driver tests in `lib.rs`).
    fn drive_probing<F>(
        msg: &QueueMsg,
        cp: &mut JobCheckpoint,
        batches: &[Vec<(String, Option<u64>)>],
        call: F,
    ) -> Result<u32, JobError>
    where
        F: FnMut(&ProbeRequest) -> Result<ProbeOutcome, PoolProbeError>,
    {
        // Unbudgeted: the budget behavior has its own driver below.
        drive_probing_budgeted(msg, cp, batches, usize::MAX, &|| false, call)
    }

    fn all_present(req: &ProbeRequest) -> Result<ProbeOutcome, PoolProbeError> {
        Ok(ProbeOutcome {
            batch_index: req.batch_index,
            present: req.segment_ids.clone(),
            missing: vec![],
        })
    }

    #[test]
    fn probing_folds_every_pending_batch_and_advances_to_verifying() {
        let (msg, mut cp, batches) = probing_setup();
        let mut asked = Vec::new();
        let mut asked_bytes = Vec::new();
        let missing = drive_probing(&msg, &mut cp, &batches, |req| {
            asked.push(req.batch_index);
            asked_bytes.push(req.segment_bytes.clone());
            if req.batch_index == 1 {
                Ok(ProbeOutcome {
                    batch_index: 1,
                    present: vec![req.segment_ids[0].clone()],
                    missing: vec![req.segment_ids[1].clone()],
                })
            } else {
                all_present(req)
            }
        })
        .expect("both batches answer");
        assert_eq!(asked, vec![0, 1]);
        // Per-segment byte ranges ride the probe envelope, parallel to ids.
        assert_eq!(asked_bytes, vec![vec![Some(100)], vec![Some(200), None]]);
        assert_eq!(missing, 1);
        assert_eq!(cp.state, JobState::Verifying);
        let tally = cp.probe_tally.as_ref().expect("tally retained");
        assert!(tally.is_complete());
        assert_eq!(tally.missing_segments(), 1);
    }

    #[test]
    fn probing_resumes_skipping_tallied_batches_without_double_counting() {
        let (msg, mut cp, batches) = probing_setup();
        let mut tally = ProbeTally::new(2);
        assert!(tally.record(&ProbeOutcome {
            batch_index: 0,
            present: vec![],
            missing: vec!["seg-a@synthetic.invalid".to_string()],
        }));
        cp.probe_tally = Some(tally);

        let mut asked = Vec::new();
        let missing = drive_probing(&msg, &mut cp, &batches, |req| {
            asked.push(req.batch_index);
            all_present(req)
        })
        .expect("remaining batch answers");
        assert_eq!(asked, vec![1], "tallied batch must not be re-asked");
        assert_eq!(missing, 1, "the earlier gap is counted exactly once");
        assert_eq!(cp.state, JobState::Verifying);
    }

    #[test]
    fn probing_retry_keeps_folded_batches_in_the_checkpoint_to_persist() {
        let (msg, mut cp, batches) = probing_setup();
        let err = drive_probing(&msg, &mut cp, &batches, |req| {
            if req.batch_index == 0 {
                all_present(req)
            } else {
                Err(PoolProbeError::Retry { after_secs: 42 })
            }
        })
        .expect_err("busy pool must surface as a retryable error");
        assert!(err.transient);
        assert_eq!(err.code, ERR_PROBE_FAILED);
        assert_eq!(err.retry_after_secs, Some(42));
        assert_eq!(cp.state, JobState::Probing);
        let tally = cp.probe_tally.as_ref().expect("folded batch kept");
        assert!(tally.missing_by_batch.contains_key(&0));
        assert!(!tally.missing_by_batch.contains_key(&1));

        // Settlement persists that tally (the delivery's one write) and
        // retries no sooner than the pool asked, inside the attempt budget.
        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err), true);
        assert!(persist);
        assert_eq!(settlement, Settlement::RetryAfter(42));
        assert_eq!(cp.attempt, 2);
        assert!(cp
            .probe_tally
            .as_ref()
            .is_some_and(|t| t.missing_by_batch.contains_key(&0)));
    }

    #[test]
    fn probing_rejected_fails_the_job_with_a_generic_reason() {
        let (msg, mut cp, batches) = probing_setup();
        let err = drive_probing(&msg, &mut cp, &batches, |_| Err(PoolProbeError::Rejected))
            .expect_err("a refused envelope cannot be retried");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_PROBE_FAILED);
        assert_eq!(err.detail, "pool probe rejected the request");

        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err), true);
        assert!(persist);
        assert_eq!(cp.state, JobState::Failed);
        assert_eq!(cp.failure_code.as_deref(), Some(ERR_PROBE_FAILED));
        assert!(matches!(settlement, Settlement::Quarantine(_)));
    }

    #[test]
    fn probing_refuses_answers_that_do_not_partition_the_request() {
        let (msg, mut cp, batches) = probing_setup();
        let err = drive_probing(&msg, &mut cp, &batches, |req| {
            Ok(ProbeOutcome {
                batch_index: req.batch_index,
                present: vec!["foreign@synthetic.invalid".to_string()],
                missing: vec![],
            })
        })
        .expect_err("a foreign answer must never move the tally");
        assert!(!err.transient);
        assert_eq!(cp.state, JobState::Probing);
        assert_eq!(
            cp.probe_tally.as_ref().map(|t| t.missing_segments()),
            Some(0)
        );
        assert!(cp
            .probe_tally
            .as_ref()
            .is_some_and(|t| t.missing_by_batch.is_empty()));
    }

    #[test]
    fn probing_with_a_complete_tally_advances_without_calling_the_pool() {
        let (msg, mut cp, batches) = probing_setup();
        let mut tally = ProbeTally::new(2);
        for batch_index in 0..2 {
            assert!(tally.record(&ProbeOutcome {
                batch_index,
                present: vec![],
                missing: vec![],
            }));
        }
        cp.probe_tally = Some(tally);

        let missing = drive_probing(&msg, &mut cp, &batches, |_| {
            panic!("a complete tally needs no pool call")
        })
        .expect("complete tally finishes probing");
        assert_eq!(missing, 0);
        assert_eq!(cp.state, JobState::Verifying);
    }

    #[test]
    fn probing_rejects_a_tally_that_disagrees_with_the_parse() {
        let (msg, mut cp, batches) = probing_setup();
        cp.probe_tally = Some(ProbeTally::new(3));
        let err = drive_probing(&msg, &mut cp, &batches, |_| {
            panic!("an inconsistent tally must stop before any pool call")
        })
        .expect_err("tally for a different parse is a contract bug");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_PROBE_FAILED);
        assert_eq!(cp.state, JobState::Probing);
    }

    #[test]
    fn probing_refuses_an_empty_batch_list_without_a_pool_call() {
        let (msg, mut cp, _) = probing_setup();
        let err = drive_probing(&msg, &mut cp, &[], |_| {
            panic!("an empty parse needs no pool call")
        })
        .expect_err("zero batches can never complete probing");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_PARSE_FAILED);
        assert_eq!(cp.state, JobState::Probing);
        assert!(cp.probe_tally.is_none());
    }

    #[test]
    fn probing_driver_makes_no_pool_call_outside_probing_state() {
        let (msg, mut cp, batches) = probing_setup();
        cp.state = JobState::Verifying;
        let err = drive_probing(&msg, &mut cp, &batches, |_| {
            panic!("no provider connection may be spent outside Probing")
        })
        .expect_err("the driver only runs in Probing");
        assert!(err.transient);
        assert_eq!(cp.state, JobState::Verifying);
        assert!(cp.probe_tally.is_none());
    }

    fn drive_probing_budgeted<F>(
        msg: &QueueMsg,
        cp: &mut JobCheckpoint,
        batches: &[Vec<(String, Option<u64>)>],
        max_batches: usize,
        budget_expired: &dyn Fn() -> bool,
        mut call: F,
    ) -> Result<u32, JobError>
    where
        F: FnMut(&ProbeRequest) -> Result<ProbeOutcome, PoolProbeError>,
    {
        use futures_util::future::FutureExt as _;
        run_probing_batches(msg, cp, batches, max_batches, budget_expired, |req| {
            let result = call(&req);
            async move { result }
        })
        .now_or_never()
        .expect("scripted probe resolves without yielding")
    }

    #[test]
    fn probing_batch_budget_stops_the_delivery_with_progress_kept() {
        let (msg, mut cp, batches) = probing_setup();
        let mut asked = Vec::new();
        let err = drive_probing_budgeted(&msg, &mut cp, &batches, 1, &|| false, |req| {
            asked.push(req.batch_index);
            all_present(req)
        })
        .expect_err("a budget-capped delivery must stop instead of asking every batch");
        assert_eq!(asked, vec![0], "the cap allows exactly one batch");
        assert!(err.transient);
        assert!(err.made_progress, "the folded batch is durable progress");
        assert_eq!(err.code, ERR_PROBE_FAILED);
        assert_eq!(cp.state, JobState::Probing);
        assert!(cp
            .probe_tally
            .as_ref()
            .is_some_and(|t| t.missing_by_batch.contains_key(&0)));

        // Settlement persists that tally and continues on a fresh copy:
        // no attempt is spent, even though probing is unfinished.
        let attempt_before = cp.attempt;
        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err), true);
        assert!(persist);
        assert_eq!(settlement, Settlement::Continue);
        assert_eq!(cp.attempt, attempt_before);

        // The continuation delivery resumes from the tally and finishes.
        let missing = drive_probing_budgeted(&msg, &mut cp, &batches, 1, &|| false, |req| {
            assert_eq!(req.batch_index, 1, "only the unprobed batch remains");
            all_present(req)
        })
        .expect("the continuation completes probing");
        assert_eq!(missing, 0);
        assert_eq!(cp.state, JobState::Verifying);
    }

    #[test]
    fn probing_wall_budget_never_stops_before_the_first_fold() {
        // An already-expired wall budget must not produce a no-progress
        // continuation (that could re-enqueue forever): the delivery
        // always folds at least one batch, then stops.
        let (msg, mut cp, batches) = probing_setup();
        let mut asked = Vec::new();
        let err = drive_probing_budgeted(&msg, &mut cp, &batches, usize::MAX, &|| true, |req| {
            asked.push(req.batch_index);
            all_present(req)
        })
        .expect_err("an expired wall budget stops after the first fold");
        assert_eq!(asked, vec![0]);
        assert!(err.made_progress);
        assert_eq!(cp.state, JobState::Probing);
    }

    #[test]
    fn edge_upsert_forwards_the_fenced_write_through_upsert_job_result() {
        // The 0-row to permanent `account_deleting` mapping is pinned pure
        // in `library::upsert_job_result`; pin here that the edge arm
        // forwards through it, so weakening the arm to `Ok(())` (the
        // phantom `Done` the comment warns about) fails here. Slice from
        // the edge impl out of the pre-test source, so this test's own
        // literal cannot satisfy the assertion.
        let source = include_str!("queue_consumer.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        let edge = production
            .split_once("impl MatchWriteRuntime for EdgeMatchWrite")
            .expect("edge impl must exist")
            .1;
        let body = edge
            .split_once("async fn upsert(")
            .expect("edge upsert must exist")
            .1
            .split_once("\nasync fn execute_write_with")
            .expect("execute_write_with must bound the edge impl")
            .0;
        assert!(
            body.contains("upsert_job_result(affected)"),
            "edge upsert must forward the affected-row count through upsert_job_result"
        );
    }

    #[test]
    fn probing_rejects_corrupt_tally_entries() {
        let (msg, mut cp, batches) = probing_setup();
        let mut tally = ProbeTally::new(2);
        tally.missing_by_batch.insert(7, 1);
        cp.probe_tally = Some(tally);
        let err = drive_probing(&msg, &mut cp, &batches, |_| {
            panic!("a corrupt tally must stop before any pool call")
        })
        .expect_err("out-of-range tally entries are corrupt checkpoint state");
        assert!(!err.transient);
        assert_eq!(err.code, ERR_PROBE_FAILED);
    }

    fn refusal_test_meta() -> std::collections::HashMap<String, String> {
        // A staged data-file record as `staged_to_metadata` writes it.
        [
            ("sha256", "abc123"),
            ("size", "512"),
            ("crc32", "0"),
            ("missing_blocks", "1"),
            ("size_refused_blocks", "2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn staged_from_metadata_adopts_a_present_refusal_count() {
        // The adopted-after-lost-progress path must keep the refusal
        // count: resetting it to 0 here would re-enter a size-refused
        // file as plain missing and spend the repair round this work
        // exists to prevent.
        let meta = refusal_test_meta();
        let staged = staged_from_metadata("k", 512, &meta).expect("record must adopt");
        assert_eq!(staged.size_refused_blocks, 2);
        assert_eq!(staged.missing_blocks, 1);
    }

    #[test]
    fn staged_from_metadata_backfills_an_absent_refusal_count_to_zero() {
        // Records written before the field existed predate refusal
        // tracking: 0 preserves their repair-eligible adoption exactly
        // as judged then.
        let mut meta = refusal_test_meta();
        meta.remove("size_refused_blocks");
        let staged = staged_from_metadata("k", 512, &meta).expect("record must adopt");
        assert_eq!(staged.size_refused_blocks, 0);
        assert_eq!(staged.missing_blocks, 1);
    }

    #[test]
    fn staged_from_metadata_backfills_an_unparseable_refusal_count_to_zero() {
        let mut meta = refusal_test_meta();
        meta.insert("size_refused_blocks".to_string(), "many".to_string());
        let staged = staged_from_metadata("k", 512, &meta).expect("record must adopt");
        assert_eq!(staged.size_refused_blocks, 0);
    }

    #[test]
    fn staged_metadata_write_read_round_trips_the_refusal_count() {
        // The `put` side must write the count `staged_from_metadata`
        // parses back: deleting the `md.insert` keeps every double-based
        // test green (they clone the full `StagedFile`), so this map
        // assertion plus the round trip is the pin.
        let meta = StagedFile {
            key: "k".into(),
            size: 512,
            sha256: "abc123".into(),
            missing_blocks: 1,
            size_refused_blocks: 2,
            par2_set_id: None,
            spool: Vec::new(),
            crc32: 0,
            missing: false,
        };
        let written = staged_to_metadata(&meta);
        assert_eq!(
            written.get("size_refused_blocks").map(String::as_str),
            Some("2"),
            "the put metadata map must carry the refusal count"
        );
        let adopted = staged_from_metadata("k", 512, &written).expect("written record must adopt");
        assert_eq!(adopted.size_refused_blocks, 2);
        assert_eq!(adopted.missing_blocks, 1);
        assert_eq!(adopted.sha256, "abc123");
    }
}
