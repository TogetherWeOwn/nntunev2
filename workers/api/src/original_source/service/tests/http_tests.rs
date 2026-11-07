use std::collections::VecDeque;

use super::*;
use crate::original_source::service::http::{self, Body, Head, Reply};

struct Chunks {
    chunks: VecDeque<Result<Vec<u8>, SourceServiceError>>,
    calls: Rc<Cell<usize>>,
    dropped: DropFlag,
    pending: bool,
    hook: Option<(Rc<Cell<u64>>, u64)>,
}

impl Chunks {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            chunks: [Ok(bytes)].into(),
            calls: Default::default(),
            dropped: DropFlag(Default::default()),
            pending: false,
            hook: None,
        }
    }
}

impl Body for Chunks {
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, SourceServiceError> {
        self.calls.set(self.calls.get() + 1);
        if let Some((clock, value)) = &self.hook {
            clock.set(*value);
        }
        if self.pending {
            never().await;
        }
        let next = self.chunks.pop_front().transpose()?;
        // The native fake follows the raw-copy contract too.
        if next.as_ref().is_some_and(|chunk| chunk.len() > remaining) {
            return Err(SourceServiceError::Invalid);
        }
        Ok(next)
    }
}

fn head(req: &Req) -> Head<'_> {
    Head {
        method: req.method,
        path: req.path,
        authorization: req.auth.as_deref(),
        content_type: Some("application/json"),
        content_length: None,
        content_encoding: None,
        browser_context: req.browser,
    }
}

async fn send(f: &Factory, req: &Req, head: Head<'_>, chunks: Chunks) -> Reply {
    http::handle(
        f,
        &Keys,
        req.key,
        head,
        chunks,
        || f.clock.get(),
        never(),
        never(),
    )
    .await
}

fn unavailable(reply: Reply) {
    assert_eq!(reply.status, 503);
    assert_eq!(reply.content_type, "application/octet-stream");
    assert_eq!(reply.cache_control, "no-store");
    assert!(reply.source_job_id.is_none());
    assert!(reply.bytes.is_empty());
}

#[tokio::test]
async fn success_is_exact_job_payload_and_never_a_json_or_cache_reply() {
    let f = Factory::new();
    let req = Req::valid();
    let mut chunks = Chunks::new(req.body.clone());
    let split = req.body.len() / 2;
    chunks.chunks = [
        Ok(req.body[..split].to_vec()),
        Ok(req.body[split..].to_vec()),
    ]
    .into();
    let length = req.body.len().to_string();
    let mut h = head(&req);
    h.content_length = Some(&length);
    let reply = send(&f, &req, h, chunks).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type, "application/octet-stream");
    assert_eq!(reply.cache_control, "no-store");
    assert_eq!(reply.source_job_id.as_deref(), Some(JOB));
    assert_eq!(reply.bytes, source().encode().unwrap());
    assert_eq!(f.connects.get(), 1);
    assert!(f.dropped_repo.get());
}

#[tokio::test]
async fn malformed_heads_never_read_body_or_connect() {
    let f = Factory::new();
    let req = Req::valid();
    for case in 0..10 {
        let mut h = head(&req);
        match case {
            0 => h.method = "GET",
            1 => h.path = "/public/original-source",
            2 => h.browser_context = true,
            3 => h.content_type = None,
            4 => h.content_type = Some("text/plain"),
            5 => h.content_encoding = Some("gzip"),
            6 => h.authorization = None,
            7 => h.authorization = Some(""),
            8 => h.content_length = Some("513"),
            _ => h.content_length = Some("0"),
        }
        let chunks = Chunks::new(req.body.clone());
        let calls = chunks.calls.clone();
        let dropped = chunks.dropped.0.clone();
        unavailable(send(&f, &req, h, chunks).await);
        assert_eq!(calls.get(), 0);
        assert!(dropped.get());
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn invalid_lengths_and_oversized_auth_never_read_body() {
    let f = Factory::new();
    let req = Req::valid();
    for length in ["", "-1", "+1", " 1", "1 ", "1, 1", "99999999999", "NaN"] {
        let mut h = head(&req);
        h.content_length = Some(length);
        let chunks = Chunks::new(req.body.clone());
        let calls = chunks.calls.clone();
        unavailable(send(&f, &req, h, chunks).await);
        assert_eq!(calls.get(), 0);
    }
    let auth = "a".repeat(257);
    let mut h = head(&req);
    h.authorization = Some(&auth);
    unavailable(send(&f, &req, h, Chunks::new(req.body.clone())).await);
    assert_eq!(f.connects.get(), 0);
}

#[tokio::test]
async fn declared_length_must_equal_actual_body_in_both_directions() {
    for delta in [-1, 1] {
        let f = Factory::new();
        let req = Req::valid();
        let length = (req.body.len() as isize + delta).to_string();
        let mut h = head(&req);
        h.content_length = Some(&length);
        unavailable(send(&f, &req, h, Chunks::new(req.body.clone())).await);
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn exact_cap_with_eof_works_but_one_more_byte_fails() {
    let f = Factory::new();
    let mut bytes = Req::valid().body;
    bytes.resize(512, b' ');
    let req = Req::signed(bytes);
    assert_eq!(
        send(&f, &req, head(&req), Chunks::new(req.body.clone()))
            .await
            .status,
        200
    );
    let f = Factory::new();
    let mut chunks = Chunks::new(req.body.clone());
    chunks.chunks.push_back(Ok(vec![b' ']));
    unavailable(send(&f, &req, head(&req), chunks).await);
    assert_eq!(f.connects.get(), 0);
}

#[tokio::test]
async fn empty_chunks_transport_errors_and_partial_json_never_connect() {
    for case in 0..4 {
        let f = Factory::new();
        let req = Req::valid();
        let mut chunks = Chunks::new(req.body.clone());
        match case {
            0 => chunks.chunks.clear(),
            1 => chunks.chunks.push_back(Ok(Vec::new())),
            2 => chunks.chunks.push_back(Err(SourceServiceError::Store)),
            _ => chunks.chunks = [Ok(req.body[..req.body.len() - 1].to_vec())].into(),
        }
        unavailable(send(&f, &req, head(&req), chunks).await);
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn invalid_signature_and_missing_key_never_connect() {
    for missing_key in [false, true] {
        let f = Factory::new();
        let mut req = Req::valid();
        if missing_key {
            req.key = None;
        } else {
            req.body.push(b' ');
        }
        unavailable(send(&f, &req, head(&req), Chunks::new(req.body.clone())).await);
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn ready_control_prevents_even_body_polling() {
    for cancel in [false, true] {
        let f = Factory::new();
        let req = Req::valid();
        let chunks = Chunks::new(req.body.clone());
        let calls = chunks.calls.clone();
        let control = || async move {
            if !cancel {
                never().await;
            }
        };
        let deadline = async move {
            if cancel {
                never().await;
            }
        };
        unavailable(
            http::handle(
                &f,
                &Keys,
                req.key,
                head(&req),
                chunks,
                || f.clock.get(),
                control(),
                deadline,
            )
            .await,
        );
        assert_eq!(calls.get(), 0);
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn cancellation_and_timer_drop_pending_body_connect_or_query() {
    for stage in 0..3 {
        for cancel in [false, true] {
            let mut f = Factory::new();
            f.pend_connect = stage == 1;
            f.pend_metadata = stage == 2;
            let req = Req::valid();
            let mut chunks = Chunks::new(req.body.clone());
            chunks.pending = stage == 0;
            let dropped = chunks.dropped.0.clone();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let fired = async {
                let _ = rx.await;
            };
            type Control = std::pin::Pin<Box<dyn Future<Output = ()>>>;
            let (cancellation, deadline): (Control, Control) = if cancel {
                (Box::pin(fired), Box::pin(never()))
            } else {
                (Box::pin(never()), Box::pin(fired))
            };
            let mut call = Box::pin(http::handle(
                &f,
                &Keys,
                req.key,
                head(&req),
                chunks,
                || f.clock.get(),
                cancellation,
                deadline,
            ));
            assert!(futures_util::poll!(call.as_mut()).is_pending());
            tx.send(()).unwrap();
            unavailable(call.await);
            assert!(dropped.get());
            if stage == 1 {
                assert!(f.dropped_connect.get());
            }
            if stage == 2 {
                assert!(f.dropped_repo.get());
            }
        }
    }
}

#[tokio::test]
async fn dropping_handler_drops_request_body_and_pending_database_work() {
    for stage in 0..3 {
        let mut f = Factory::new();
        f.pend_connect = stage == 1;
        f.pend_metadata = stage == 2;
        let req = Req::valid();
        let mut chunks = Chunks::new(req.body.clone());
        chunks.pending = stage == 0;
        let dropped = chunks.dropped.0.clone();
        let mut call = Box::pin(http::handle(
            &f,
            &Keys,
            req.key,
            head(&req),
            chunks,
            || f.clock.get(),
            never(),
            never(),
        ));
        assert!(futures_util::poll!(call.as_mut()).is_pending());
        drop(call);
        assert!(dropped.get());
        if stage == 1 {
            assert!(f.dropped_connect.get());
        }
        if stage == 2 {
            assert!(f.dropped_repo.get());
        }
    }
}

#[tokio::test]
async fn expiry_and_rollback_during_body_collection_prevent_connect() {
    for value in [NOW - 1, NOW + 1000] {
        let f = Factory::new();
        let req = Req::valid();
        let mut chunks = Chunks::new(req.body.clone());
        chunks.hook = Some((f.clock.clone(), value));
        unavailable(send(&f, &req, head(&req), chunks).await);
        assert_eq!(f.connects.get(), 0);
    }
}

#[tokio::test]
async fn control_becoming_ready_during_successful_work_wins_the_reply() {
    for cancel in [false, true] {
        let mut f = Factory::new();
        f.hook = Some(("envelope", NOW + 1));
        let req = Req::valid();
        let fired = futures_util::future::poll_fn(|_| {
            if f.clock.get() > NOW {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        });
        let control = async {
            if cancel {
                fired.await;
            } else {
                never().await;
            }
        };
        let deadline = async {
            if cancel {
                never().await;
            }
            futures_util::future::poll_fn(|_| {
                if f.clock.get() > NOW {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
        };
        unavailable(
            http::handle(
                &f,
                &Keys,
                req.key,
                head(&req),
                Chunks::new(req.body.clone()),
                || f.clock.get(),
                control,
                deadline,
            )
            .await,
        );
        assert_eq!(f.connects.get(), 1);
        assert!(f.dropped_repo.get());
    }
}

/// Source-shape guards, not execution of Worker JavaScript or abort propagation.
#[test]
fn worker_reader_acquisition_catches_js_exceptions() {
    let source = include_str!("../worker.rs");
    assert!(source.contains(".call0(stream.as_ref())"));
    assert!(!source.contains(".get_reader()"));
}

#[test]
fn platform_timer_is_not_shadowed_by_the_worker_adapter_module() {
    let source = include_str!("../../service.rs");
    assert!(source.contains("::worker::Delay::from(remaining)"));
}

#[tokio::test]
async fn source_failures_and_authority_loss_never_disclose_job_or_bytes() {
    for stage in ["connect", "metadata", "envelope", "missing"] {
        let mut f = Factory::new();
        if stage == "missing" {
            f.rows = [None, None];
        } else {
            f.hook = Some((stage, NOW + 1000));
        }
        let req = Req::valid();
        unavailable(send(&f, &req, head(&req), Chunks::new(req.body.clone())).await);
        assert_eq!(f.connects.get(), 1);
        assert!(f.dropped_repo.get());
    }
}
