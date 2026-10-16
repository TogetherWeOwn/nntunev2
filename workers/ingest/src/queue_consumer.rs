//! Per-message Queue delivery and checkpoint policy.

use worker::{console_log, Env, MessageExt, QueueRetryOptionsBuilder};

use crate::dispatch::{
    content_sha256_hex, make_job_id, payload_retry_delay_ms, validate_user_id, PAYLOAD_KEY_PREFIX,
    PAYLOAD_KV_BINDING,
};
use crate::jobs::{
    advance_checkpoint, can_retry, is_terminal, validate_job_id, JobCheckpoint, JobSource,
    JobState, MAX_ATTEMPTS, MAX_REPAIR_ROUNDS,
};
use crate::newznab::{MAX_CATEGORIES, MAX_LIMIT, MAX_PAGE};
use crate::queue::{validate_indexer_ref, QueueMsg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageDisposition {
    Ack,
    RetryAfter(u32),
}

fn validate_queue_msg(msg: &QueueMsg) -> core::result::Result<(), &'static str> {
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

fn initial_checkpoint(msg: &QueueMsg) -> core::result::Result<JobCheckpoint, &'static str> {
    validate_queue_msg(msg)?;
    Ok(JobCheckpoint {
        job_id: msg.job_id.clone(),
        user_id: msg.user_id.clone(),
        idempotency_key: msg.idempotency_key.clone(),
        source: msg.source,
        state: JobState::Queued,
        attempt: 1,
        repair_rounds: 0,
        probe_tally: None,
        failure_reason: None,
    })
}

fn checkpoint_matches(msg: &QueueMsg, cp: &JobCheckpoint) -> bool {
    cp.job_id == msg.job_id
        && cp.user_id == msg.user_id
        && cp.idempotency_key == msg.idempotency_key
        && cp.source == msg.source
        && (1..=MAX_ATTEMPTS).contains(&cp.attempt)
        && cp.repair_rounds <= MAX_REPAIR_ROUNDS
}

fn retry_or_fail(cp: &mut JobCheckpoint, reason: &'static str) -> MessageDisposition {
    if can_retry(cp.attempt) {
        let delay = payload_retry_delay_ms(cp.attempt.saturating_sub(1));
        cp.attempt += 1;
        MessageDisposition::RetryAfter(delay.div_ceil(1_000) as u32)
    } else {
        cp.state = JobState::Failed;
        cp.failure_reason = Some(reason.to_string());
        MessageDisposition::Ack
    }
}

fn fail_checkpoint(cp: &mut JobCheckpoint, reason: &'static str) {
    if !is_terminal(cp.state) {
        cp.state = JobState::Failed;
        cp.failure_reason = Some(reason.to_string());
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

async fn retry_with_checkpoint(
    env: &Env,
    cp: &mut JobCheckpoint,
    reason: &'static str,
) -> MessageDisposition {
    let disposition = retry_or_fail(cp, reason);
    if persist(env, cp).await {
        disposition
    } else {
        MessageDisposition::RetryAfter(5)
    }
}

async fn process_message(env: &Env, msg: &QueueMsg) -> MessageDisposition {
    if validate_queue_msg(msg).is_err() {
        return MessageDisposition::RetryAfter(5);
    }

    let mut cp = match crate::checkpoint::get(env, &msg.job_id).await {
        Ok(Some(cp)) => {
            if !checkpoint_matches(msg, &cp) {
                return MessageDisposition::RetryAfter(5);
            }
            if is_terminal(cp.state) {
                return MessageDisposition::Ack;
            }
            cp
        }
        Ok(None) => match initial_checkpoint(msg) {
            Ok(cp) => cp,
            Err(_) => return MessageDisposition::RetryAfter(5),
        },
        Err(error) => {
            console_log!("ingest checkpoint read failed: {:?}", error);
            return MessageDisposition::RetryAfter(5);
        }
    };

    if cp.state == JobState::Queued {
        advance_checkpoint(&mut cp, false);
        if !persist(env, &cp).await {
            return MessageDisposition::RetryAfter(5);
        }
    }

    match cp.state {
        JobState::Parsing if cp.source == JobSource::NzbUpload => {
            let Some(payload_ref) = msg.payload_ref.as_deref() else {
                return MessageDisposition::RetryAfter(5);
            };
            let payload = match env.kv(PAYLOAD_KV_BINDING) {
                Ok(store) => match store.get(payload_ref).cache_ttl(60).text().await {
                    Ok(Some(payload)) => payload,
                    Ok(None) => {
                        return retry_with_checkpoint(env, &mut cp, "upload payload unavailable")
                            .await
                    }
                    Err(error) => {
                        console_log!("ingest payload read failed: {:?}", error);
                        return retry_with_checkpoint(env, &mut cp, "upload payload unavailable")
                            .await;
                    }
                },
                Err(error) => {
                    console_log!("ingest payload binding unavailable: {:?}", error);
                    return retry_with_checkpoint(env, &mut cp, "upload payload unavailable").await;
                }
            };
            if payload_ref
                != format!(
                    "{PAYLOAD_KEY_PREFIX}{}",
                    content_sha256_hex(payload.as_bytes())
                )
            {
                fail_checkpoint(&mut cp, "upload payload digest mismatch");
                return if persist(env, &cp).await {
                    MessageDisposition::Ack
                } else {
                    MessageDisposition::RetryAfter(5)
                };
            }
            let estimate = match crate::nzb_intake::estimate_nzb(&payload) {
                Ok(estimate) => estimate,
                Err(_) => {
                    fail_checkpoint(&mut cp, "upload payload failed intake validation");
                    return if persist(env, &cp).await {
                        MessageDisposition::Ack
                    } else {
                        MessageDisposition::RetryAfter(5)
                    };
                }
            };
            let parsed = match crate::parse::parse_upload(payload.as_bytes(), &estimate) {
                Ok(parsed) => parsed,
                Err(_) => {
                    fail_checkpoint(&mut cp, "shared NZB parser rejected payload");
                    return if persist(env, &cp).await {
                        MessageDisposition::Ack
                    } else {
                        MessageDisposition::RetryAfter(5)
                    };
                }
            };
            if crate::parse::probe_batches(&parsed).is_empty() {
                fail_checkpoint(&mut cp, "NZB has no segments to probe");
                return if persist(env, &cp).await {
                    MessageDisposition::Ack
                } else {
                    MessageDisposition::RetryAfter(5)
                };
            }
            advance_checkpoint(&mut cp, false);
            if !persist(env, &cp).await {
                return MessageDisposition::RetryAfter(5);
            }
            retry_with_checkpoint(env, &mut cp, "pool probe runtime unavailable").await
        }
        JobState::Searching => {
            retry_with_checkpoint(env, &mut cp, "indexer search runtime unavailable").await
        }
        JobState::Parsing | JobState::Probing => {
            retry_with_checkpoint(env, &mut cp, "pool probe runtime unavailable").await
        }
        _ => retry_with_checkpoint(env, &mut cp, "ingest runtime boundary unavailable").await,
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
            retry_raw(&raw, 5);
            return;
        }
    };
    let msg = match serde_json::from_value::<QueueMsg>(value) {
        Ok(msg) => msg,
        Err(_) => {
            retry_raw(&raw, 5);
            return;
        }
    };
    let disposition = process_message(env, &msg).await;
    match disposition {
        MessageDisposition::Ack => raw.ack(),
        MessageDisposition::RetryAfter(seconds) => retry_raw(&raw, seconds),
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
    use crate::jobs::JobState;

    fn search_message() -> crate::queue::QueueMsg {
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
    fn first_checkpoint_preserves_queue_identity_and_starts_queued() {
        let msg = search_message();
        let cp = initial_checkpoint(&msg).unwrap();
        assert_eq!(cp.job_id, msg.job_id);
        assert_eq!(cp.user_id, msg.user_id);
        assert_eq!(cp.idempotency_key, msg.idempotency_key);
        assert_eq!(cp.source, msg.source);
        assert_eq!(cp.state, JobState::Queued);
        assert_eq!(cp.attempt, 1);
        assert!(cp.probe_tally.is_none());
    }

    #[test]
    fn replay_acknowledges_only_terminal_checkpoints() {
        let mut cp = initial_checkpoint(&search_message()).unwrap();
        assert!(!is_terminal(cp.state));

        cp.state = JobState::Done;
        assert!(is_terminal(cp.state));
        cp.state = JobState::Failed;
        assert!(is_terminal(cp.state));
    }

    #[test]
    fn retry_budget_uses_payload_lag_backoff_then_persists_terminal_failure() {
        let mut cp = initial_checkpoint(&search_message()).unwrap();
        let expected_delays = [5, 15, 30, 60];

        for delay in expected_delays {
            assert_eq!(
                retry_or_fail(&mut cp, "runtime unavailable"),
                MessageDisposition::RetryAfter(delay)
            );
            assert_eq!(cp.state, JobState::Queued);
        }
        assert_eq!(cp.attempt, MAX_ATTEMPTS);
        assert_eq!(
            retry_or_fail(&mut cp, "runtime unavailable"),
            MessageDisposition::Ack
        );
        assert_eq!(cp.state, JobState::Failed);
        assert_eq!(cp.failure_reason.as_deref(), Some("runtime unavailable"));
    }

    #[test]
    fn checkpoint_identity_must_match_the_redelivered_message() {
        let msg = search_message();
        let cp = initial_checkpoint(&msg).unwrap();
        assert!(checkpoint_matches(&msg, &cp));

        let mut other = msg.clone();
        other.job_id = "job_other".into();
        assert!(!checkpoint_matches(&other, &cp));
    }
}
