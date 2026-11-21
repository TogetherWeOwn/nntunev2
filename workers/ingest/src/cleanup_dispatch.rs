//! Inactive producer for committed multipart cleanup claims. No route or
//! consumer calls this adapter yet. Queue acceptance is not a cleanup receipt.

use std::future::Future;

use serde::Serialize;
use worker::{Env, MessageBuilder};

mod postgres;
pub use postgres::{dispatch_scope, CleanupScope, DispatchError};

const MAX_DELAY_SECONDS: u32 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupKind {
    AbsoluteExpiry,
    TerminalReconcile,
}

pub struct CleanupClaim {
    session_id: String,
    kind: CleanupKind,
    claim_id: String,
    delay_seconds: u32,
}

impl CleanupClaim {
    /// Build only from a trusted, committed outbox row, never a request body.
    pub fn from_committed_row(
        session_id: String,
        kind: CleanupKind,
        claim_id: String,
        delay_seconds: i32,
    ) -> Option<Self> {
        let delay_seconds = u32::try_from(delay_seconds).ok()?;
        if delay_seconds > MAX_DELAY_SECONDS
            || (kind == CleanupKind::TerminalReconcile && delay_seconds != 0)
            || !crate::queue::validate_provider_id(&session_id)
            || !crate::queue::validate_provider_id(&claim_id)
        {
            return None;
        }
        Some(Self {
            session_id,
            kind,
            claim_id,
            delay_seconds,
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CleanupMessage {
    UploadSessionCleanup {
        session_id: String,
        kind: CleanupKind,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub enum HandoffError {
    QueueNotConfirmed,
    AckNotConfirmed,
}

/// Use the existing ingest queue. The caller binds `ack` to the trusted
/// environment/library/principal and executes the scoped claim CAS. False or
/// error is unconfirmed; leave retries to the outbox, never resend inline.
pub async fn enqueue_claim<A: Future<Output = Result<bool, ()>>>(
    env: &Env,
    claim: CleanupClaim,
    ack: impl FnOnce(String, CleanupKind, String) -> A,
) -> Result<(), HandoffError> {
    handoff(
        claim,
        |message, delay| async move {
            env.queue(crate::dispatch::INGEST_QUEUE_BINDING)
                .map_err(|_| ())?
                .send(MessageBuilder::new(message).delay_seconds(delay).build())
                .await
                .map_err(|_| ())
        },
        ack,
    )
    .await
}

async fn handoff<S: Future<Output = Result<(), ()>>, A: Future<Output = Result<bool, ()>>>(
    claim: CleanupClaim,
    send: impl FnOnce(CleanupMessage, u32) -> S,
    ack: impl FnOnce(String, CleanupKind, String) -> A,
) -> Result<(), HandoffError> {
    let CleanupClaim {
        session_id,
        kind,
        claim_id,
        delay_seconds,
    } = claim;
    let message = CleanupMessage::UploadSessionCleanup {
        session_id: session_id.clone(),
        kind,
    };
    send(message, delay_seconds)
        .await
        .map_err(|()| HandoffError::QueueNotConfirmed)?;
    match ack(session_id, kind, claim_id).await {
        Ok(true) => Ok(()),
        Ok(false) | Err(()) => Err(HandoffError::AckNotConfirmed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::future::FutureExt as _;
    use std::{cell::Cell, future::ready};

    const SESSION: &str = "00000000-0000-0000-0000-000000000001";
    const CLAIM: &str = "00000000-0000-0000-0000-000000000002";
    const SUCCESSOR: &str = "00000000-0000-0000-0000-000000000003";

    #[test]
    fn confirms_queue_before_ack_and_redelivers_same_opaque_identity() {
        for (sent, acked, claim_id, result) in [
            (
                Err(()),
                Ok(true),
                CLAIM,
                Err(HandoffError::QueueNotConfirmed),
            ),
            (Ok(()), Ok(false), CLAIM, Err(HandoffError::AckNotConfirmed)),
            (Ok(()), Err(()), CLAIM, Err(HandoffError::AckNotConfirmed)),
            (Ok(()), Ok(true), SUCCESSOR, Ok(())),
        ] {
            let kind = CleanupKind::AbsoluteExpiry;
            let claim =
                CleanupClaim::from_committed_row(SESSION.into(), kind, claim_id.into(), 86400)
                    .unwrap();
            let phase = Cell::new(0);
            let send_phase = &phase;
            let actual = handoff(
                claim,
                |message, delay| async move {
                    assert_eq!(send_phase.replace(1), 0);
                    assert_eq!(delay, 86400);
                    let expected = serde_json::json!({
                        "type": "upload_session_cleanup",
                        "session_id": SESSION,
                        "kind": "absolute_expiry",
                    });
                    assert_eq!(serde_json::to_value(message).unwrap(), expected);
                    sent
                },
                |session, ack_kind, id| {
                    assert_eq!(phase.replace(2), 1);
                    assert_eq!(
                        (session.as_str(), ack_kind, id.as_str()),
                        (SESSION, kind, claim_id)
                    );
                    ready(acked)
                },
            )
            .now_or_never();
            assert_eq!(actual, Some(result));
            assert_eq!(phase.get(), if sent.is_ok() { 2 } else { 1 });
        }
    }

    #[test]
    fn refuses_invalid_claims_before_dispatch() {
        let claim = |session: &str, kind, id: &str, delay| {
            CleanupClaim::from_committed_row(session.into(), kind, id.into(), delay)
        };
        use CleanupKind::{AbsoluteExpiry, TerminalReconcile};
        assert!(claim(SESSION, TerminalReconcile, CLAIM, 0).is_some());
        for (session, kind, id, delay) in [
            (SESSION, AbsoluteExpiry, CLAIM, -1),
            (SESSION, AbsoluteExpiry, CLAIM, 86401),
            (SESSION, TerminalReconcile, CLAIM, 1),
            ("invalid", AbsoluteExpiry, CLAIM, 0),
            (SESSION, AbsoluteExpiry, "invalid", 0),
        ] {
            assert!(claim(session, kind, id, delay).is_none());
        }
    }
}
