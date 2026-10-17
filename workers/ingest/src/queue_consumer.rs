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
//! message is settled. A failed persist retries the message without
//! acking, so no attempt increment or terminal state is ever lost
//! silently.
//!
//! Terminal replay: a `Done` checkpoint acks without work; a `Failed`
//! checkpoint re-quarantines the existing failure to the DLQ (this is how
//! a DLQ-send failure redelivery retries the send). A *fresh* admission
//! of a failed job is restarted at the route instead
//! ([`crate::checkpoint::admission_write`]), never here, so redeliveries
//! of one enqueue cannot resurrect it.
//!
//! No live runtime boundary is claimed: indexer search, pool probe,
//! verify, repair, match and write steps have no executable consumer
//! wiring yet, so those states settle as transient failures through the
//! shared retry budget. The consumer therefore stays disabled in
//! `wrangler.toml.example` until those boundaries land.

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
/// claiming progress it cannot finish. Every other non-terminal state is
/// a transient boundary failure today.
async fn run_step(env: &Env, msg: &QueueMsg, cp: &mut JobCheckpoint) -> Result<(), JobError> {
    if cp.state == JobState::Queued {
        advance_checkpoint(cp, false);
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

async fn persist(env: &Env, cp: &JobCheckpoint) -> bool {
    match crate::checkpoint::put(env, cp).await {
        Ok(()) => true,
        Err(error) => {
            console_log!("ingest checkpoint write failed: {:?}", error);
            false
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
