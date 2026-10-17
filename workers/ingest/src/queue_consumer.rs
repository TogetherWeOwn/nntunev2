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
//! `Repairing` is also executable (it calls the repair Worker through
//! the `REPAIR_CONTAINER` service binding and returns to `Verifying`),
//! but it is only reachable once verify can stage its inputs. No other
//! live runtime boundary is claimed: indexer search (hence probing for
//! search-sourced jobs), verify, match and write steps have no
//! executable consumer wiring yet, so those states settle as transient
//! failures through the shared retry budget. The consumer therefore
//! stays disabled in `wrangler.toml.example` until those boundaries
//! land.

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
use crate::parse::ParsedNzb;
use crate::pool_probe::{call_pool_probe, check_outcome, PoolProbeError};
use crate::probe::{
    apply_probe_batch, build_probe_request, ProbeOutcome, ProbeRequest, ProbeTally,
};
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

/// Run the `Repairing` step: dispatch the recorded repair request to
/// the repair Worker, then return to `Verifying` on success. Verify has
/// no executable wiring yet, so a repaired job then settles as a
/// transient verify-boundary failure (same posture as `Parsing`).
/// A checkpoint in `Repairing` without recorded verify inputs is a
/// permanent failure: there is nothing a redelivery could add.
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
            return Err(JobError::transient(
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
            Err(JobError::transient(
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
fn prepare_probe_tally(cp: &mut JobCheckpoint, batches: &[Vec<String>]) -> Result<(), JobError> {
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
    batches: &[Vec<String>],
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
        let segment_ids: Vec<&str> = batches[batch_index].iter().map(String::as_str).collect();
        // The parser cross-check already validated these ids, so a build
        // failure here is a contract bug: same generic permanent refusal
        // as a pool-side rejection.
        let request = build_probe_request(msg, batch_index, &segment_ids)
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

/// Fetch, digest-check and parse the upload payload for `msg`. Re-parsing
/// on every `Parsing`/`Probing` delivery is deliberate: the checkpoint
/// stores no parse, and the content-addressed payload plus deterministic
/// parser make the re-parse converge on the same batches.
async fn load_parsed_upload(env: &Env, msg: &QueueMsg) -> Result<ParsedNzb, JobError> {
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
    crate::parse::parse_upload(payload.as_bytes(), &estimate)
        .map_err(|_| JobError::permanent(ERR_PARSE_FAILED, "shared NZB parser rejected payload"))
}

/// Run the executable part of one delivery, mutating `cp` in memory only.
///
/// Uploads fetch and digest-check the payload, run the intake guard and
/// the shared parser, advance `Parsing -> Probing`, and ask the user's
/// pool about not-yet-tallied batches ([`run_probing_batches`]) on this
/// delivery's probe budget — including on the same delivery that
/// parsed, so a fresh job does not spend a retry just to change state.
/// A budget stop settles as a progress continuation and the next
/// delivery resumes probing. Completed probing advances to
/// `Verifying` and then still reports that (not yet wired) boundary as a
/// transient failure, so the delivery settles through the shared retry
/// budget instead of claiming progress it cannot finish. `Repairing`
/// dispatches to the repair Worker (see [`run_repair_step`]). Every
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
    if !(cp.source == JobSource::NzbUpload
        && matches!(cp.state, JobState::Parsing | JobState::Probing))
    {
        return Err(JobError::transient(
            unavailable_code(cp.state),
            "ingest runtime boundary unavailable",
        ));
    }
    let parsed = load_parsed_upload(env, msg).await?;
    let batches = crate::parse::probe_batches(&parsed);
    if batches.is_empty() {
        return Err(JobError::permanent(
            ERR_PARSE_FAILED,
            "NZB has no segments to probe",
        ));
    }
    if cp.state == JobState::Parsing {
        advance_checkpoint(cp, false);
    }
    let probing_started_ms = Date::now().as_millis();
    run_probing_batches(
        msg,
        cp,
        &batches,
        MAX_PROBE_BATCHES_PER_DELIVERY,
        &|| Date::now().as_millis().saturating_sub(probing_started_ms) >= PROBE_DELIVERY_BUDGET_MS,
        |request| async move { call_pool_probe(env, &request).await },
    )
    .await?;
    Err(JobError::transient(
        ERR_VERIFY_FAILED,
        "verify runtime unavailable",
    ))
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
    let step = if matches!(cp.state, JobState::Done | JobState::Failed) {
        Ok(())
    } else {
        run_step(env, msg, &mut cp).await
    };
    let (must_persist, settlement) = plan_settlement(msg, &mut cp, step);
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
    let msg = match serde_json::from_value::<QueueMsg>(value) {
        Ok(msg) => msg,
        Err(_) => {
            retry_raw(&raw, UNPERSISTED_RETRY_SECS);
            return;
        }
    };
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
        failed.failure_reason = Some("pool probe unavailable".into());
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

    // --- Probing driver (pool probe fold / resume policy) ---

    use crate::pool_probe::PoolProbeError;
    use crate::probe::{ProbeOutcome, ProbeRequest, ProbeTally};

    fn probing_setup() -> (QueueMsg, JobCheckpoint, Vec<Vec<String>>) {
        let msg = build_upload_dispatch("user_01", crate::fixtures::SMALL_NZB).unwrap();
        let mut cp = crate::checkpoint::initial(&msg);
        cp.state = JobState::Probing;
        let batches = vec![
            vec!["seg-a@synthetic.invalid".to_string()],
            vec![
                "seg-b@synthetic.invalid".to_string(),
                "seg-c@synthetic.invalid".to_string(),
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
        batches: &[Vec<String>],
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
        let missing = drive_probing(&msg, &mut cp, &batches, |req| {
            asked.push(req.batch_index);
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
        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err));
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

        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err));
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
        batches: &[Vec<String>],
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
        let (persist, settlement) = plan_settlement(&msg, &mut cp, Err(err));
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
}
