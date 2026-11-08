//! Fleet-shared claim/send/finish ordering. Still not enabled by a user route.
//! No expiring lease: cancellation or failed completion can strand the budget,
//! but cannot let a newer send race an unpersisted provider cooldown. Recovery
//! must prove quiescence and retain the cooldown; it is not an automatic retry.

use std::future::Future;

use crate::{
    handlers::{Store, StoreError},
    listenbrainz::OutboundClaim,
};

use super::{transport::Exchange, ApiRequest, WireError};

pub(crate) async fn exchange(
    store: &impl Store,
    request: &ApiRequest,
) -> Result<Exchange, WireError> {
    coordinate(
        store.claim_listenbrainz_outbound(),
        super::transport::exchange(request),
        |claim, wait| store.finish_listenbrainz_outbound(claim, wait),
    )
    .await
}

async fn coordinate<F: Future<Output = Result<(), StoreError>>>(
    claim: impl Future<Output = Result<Option<OutboundClaim>, StoreError>>,
    send: impl Future<Output = Exchange>,
    finish: impl FnOnce(OutboundClaim, u64) -> F,
) -> Result<Exchange, WireError> {
    let claim = claim
        .await
        .map_err(|_| WireError::Unavailable)?
        .ok_or(WireError::RateLimited)?;
    // `send` is a cold async future: it is never polled on denied/failed claim.
    let reply = send.await;
    // Even a body error, absent head or timeout must finish with its cooldown.
    // A failed write drops/zeroizes the reply rather than leaking success.
    finish(claim, reply.cooldown_s())
        .await
        .map_err(|_| WireError::Unavailable)?;
    Ok(reply)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{
        cell::RefCell,
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    use super::super::transport::ResponseHead;
    use super::*;
    use crate::listenbrainz::DEFAULT_429_WAIT_S;
    use zeroize::Zeroizing;

    fn reply(head: Option<ResponseHead>, body: Result<Vec<u8>, WireError>) -> Exchange {
        Exchange {
            head,
            body: body.map(Zeroizing::new),
        }
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[tokio::test]
    async fn denied_or_failed_claim_never_polls_send_or_finish() {
        for admitted in [Ok(None), Err(StoreError)] {
            let expected = if admitted.is_err() {
                WireError::Unavailable
            } else {
                WireError::RateLimited
            };
            let result = coordinate(
                async { admitted },
                async { panic!("denied claim polled transport") },
                |_, _| async { panic!("denied claim finished") },
            )
            .await;
            assert!(matches!(result, Err(error) if error == expected));
        }
    }

    #[tokio::test]
    async fn cooldown_is_durable_before_reply_is_returned() {
        let events = RefCell::new(Vec::new());
        let events = &events;
        let result = coordinate(
            async {
                events.borrow_mut().push("claim");
                Ok(Some(OutboundClaim(7)))
            },
            async {
                events.borrow_mut().push("send");
                reply(
                    Some(ResponseHead {
                        status: 200,
                        cooldown_s: 120,
                    }),
                    Ok(b"valid".to_vec()),
                )
            },
            |claim, wait| async move {
                assert_eq!(claim.0, 7);
                assert_eq!(wait, 120);
                events.borrow_mut().push("finish");
                Ok(())
            },
        )
        .await
        .unwrap();
        events.borrow_mut().push("returned");
        assert_eq!(*events.borrow(), ["claim", "send", "finish", "returned"]);
        assert_eq!(result.body.unwrap().as_slice(), b"valid");
    }

    #[tokio::test]
    async fn body_errors_keep_the_received_cooldown() {
        for error in [
            WireError::InvalidResponse,
            WireError::Unavailable,
            WireError::Rejected,
            WireError::RateLimited,
        ] {
            let result = coordinate(
                async { Ok(Some(OutboundClaim(1))) },
                async {
                    reply(
                        Some(ResponseHead {
                            status: 429,
                            cooldown_s: 900,
                        }),
                        Err(error),
                    )
                },
                |_, wait| async move {
                    assert_eq!(wait, 900);
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert_eq!(result.body.unwrap_err(), error);
        }
    }

    #[tokio::test]
    async fn absent_head_persists_conservative_transport_failure_delay() {
        let result = coordinate(
            async { Ok(Some(OutboundClaim(1))) },
            async { reply(None, Err(WireError::Unavailable)) },
            |_, wait| async move {
                assert_eq!(wait, DEFAULT_429_WAIT_S);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.body.unwrap_err(), WireError::Unavailable);
    }

    #[tokio::test]
    async fn persistence_failure_never_returns_upstream_success_or_error_body() {
        for body in [
            Ok(b"private upstream body".to_vec()),
            Err(WireError::Rejected),
        ] {
            let result = coordinate(
                async { Ok(Some(OutboundClaim(1))) },
                async {
                    reply(
                        Some(ResponseHead {
                            status: 200,
                            cooldown_s: u64::MAX,
                        }),
                        body,
                    )
                },
                |_, wait| async move {
                    assert_eq!(wait, u64::MAX);
                    Err(StoreError)
                },
            )
            .await;
            assert!(matches!(result, Err(WireError::Unavailable)));
        }
    }

    #[test]
    fn pending_claim_never_polls_transport() {
        let mut operation = Box::pin(coordinate(
            std::future::pending(),
            async { panic!("transport polled before admission") },
            |_, _| async { panic!("finish before admission") },
        ));
        assert!(poll_once(operation.as_mut()).is_pending());
        drop(operation);
    }

    #[test]
    fn cancellation_during_send_never_releases_claim() {
        let mut operation = Box::pin(coordinate(
            async { Ok(Some(OutboundClaim(1))) },
            std::future::pending(),
            |_, _| async { panic!("cancellation cannot release claim") },
        ));
        assert!(poll_once(operation.as_mut()).is_pending());
        drop(operation);
    }

    #[test]
    fn pending_persistence_does_not_return_reply() {
        let mut operation = Box::pin(coordinate(
            async { Ok(Some(OutboundClaim(1))) },
            async {
                reply(
                    Some(ResponseHead {
                        status: 200,
                        cooldown_s: 1,
                    }),
                    Ok(Vec::new()),
                )
            },
            |_, _| std::future::pending(),
        ));
        assert!(poll_once(operation.as_mut()).is_pending());
        drop(operation);
    }
}
