//! Short-read regression tests for `collect_body`.
//!
//! The runtime byte-stream adapter maps a client abort to a clean
//! end-of-stream below the collection seam, so an aborted upload is only
//! detectable at this seam when a `Content-Length` was declared: a stream
//! that ends after fewer bytes than declared is a transport failure, never
//! a complete (possibly empty) body. With no declared length (chunked), an
//! abort that the adapter hides as end-of-stream remains undetectable — a
//! runtime limit, pinned here only for the declared-length cases.

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

use nntune_api::handlers;
use nntune_api::worker_entry::{
    collect_body, dispatch_collected, BodyChunkStream, BodyReadError, BodySource, CollectedBody,
};

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    let mut future = Box::pin(future);
    for _ in 0..10_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
    panic!("deterministic fake unexpectedly awaited external work");
}

struct FakeStream {
    chunks: VecDeque<Vec<u8>>,
}

impl FakeStream {
    fn of_chunks(chunks: Vec<&[u8]>) -> Self {
        Self {
            chunks: chunks.into_iter().map(<[u8]>::to_vec).collect(),
        }
    }

    fn empty() -> Self {
        Self::of_chunks(Vec::new())
    }
}

impl BodyChunkStream for FakeStream {
    fn poll_next_chunk(
        &mut self,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Vec<u8>, BodyReadError>>> {
        match self.chunks.pop_front() {
            Some(bytes) => Poll::Ready(Some(Ok(bytes))),
            None => Poll::Ready(None),
        }
    }
}

fn collect(declared: Option<usize>, stream: FakeStream) -> CollectedBody {
    block_on(collect_body(declared, BodySource::Stream(stream)))
}

#[test]
fn declared_length_with_zero_bytes_delivered_is_transport_failure() {
    assert_eq!(
        collect(Some(120), FakeStream::empty()),
        CollectedBody::TransportFailure,
        "an abort before any bytes must not become a dispatchable empty body \
         when a Content-Length was declared"
    );
}

#[test]
fn declared_length_with_short_prefix_is_transport_failure() {
    assert_eq!(
        collect(Some(120), FakeStream::of_chunks(vec![b"{\"ema"])),
        CollectedBody::TransportFailure,
        "a stream ending short of the declared length drops the prefix whole"
    );
}

#[test]
fn declared_length_with_exact_bytes_is_complete() {
    let body = br#"{"email":"a@b.example"}"#;
    assert_eq!(
        collect(Some(body.len()), FakeStream::of_chunks(vec![body])),
        CollectedBody::Complete(body.to_vec())
    );
}

#[test]
fn declared_zero_with_empty_stream_is_complete_empty() {
    assert_eq!(
        collect(Some(0), FakeStream::empty()),
        CollectedBody::Complete(Vec::new()),
        "an explicitly sent empty body stays valid"
    );
}

#[test]
fn no_declared_length_with_empty_stream_is_complete_empty() {
    assert_eq!(
        collect(None, FakeStream::empty()),
        CollectedBody::Complete(Vec::new()),
        "a chunked empty body has no declared length to fall short of"
    );
}

#[test]
fn short_read_dispatches_nothing_through_the_gate() {
    let collected = collect(Some(120), FakeStream::empty());
    let calls = Rc::new(Cell::new(0usize));
    let handler_calls = Rc::clone(&calls);
    let response = block_on(dispatch_collected(
        collected,
        handlers::Request {
            method: "POST".to_string(),
            path: "/v1/vault/test".to_string(),
            cookie: None,
            origin: None,
            authorization: None,
            body: b"template".to_vec(),
        },
        move |_request| async move {
            handler_calls.set(handler_calls.get() + 1);
            handlers::Response {
                status: 200,
                body: "{}".to_string(),
                set_cookie: None,
                retry_after: None,
            }
        },
    ));
    assert_eq!(calls.get(), 0, "a short read must never invoke the handler");
    assert_eq!(response.status, 400);
    assert_eq!(
        response.body,
        r#"{"code":"invalid_request","message":"invalid request","retryable":false}"#
    );
    assert_eq!(response.set_cookie, None);
}
