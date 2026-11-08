//! Hermetic tests for the unwired transport primitive: no provider, Worker
//! runtime, network, database or credential is touched. Futures are driven by
//! a tiny native executor; deadline tests use a short real timer and a
//! parking waker, bounded by a generous watchdog.

use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use super::*;

const CAP: usize = MAX_VALIDATION_BODY_BYTES;
const SHORT_TIMEOUT_MS: u64 = 20;

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Poll to completion, parking between wakes. The watchdog turns a lost wake
/// or a hung future into a failure rather than a stuck CI job.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let start = Instant::now();
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "future did not complete within the watchdog"
        );
        thread::park_timeout(Duration::from_millis(5));
    }
}

/// Poll exactly once on a noop waker.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// Yields scripted items, counts polls, then stays pending forever when
/// `stall` is set (otherwise ends).
struct ScriptedStream {
    items: std::vec::IntoIter<Result<Vec<u8>, ()>>,
    polls: Rc<Cell<usize>>,
    stall: bool,
}

impl ScriptedStream {
    fn new(items: Vec<Result<Vec<u8>, ()>>, stall: bool) -> (Self, Rc<Cell<usize>>) {
        let polls = Rc::new(Cell::new(0));
        (
            Self {
                items: items.into_iter(),
                polls: polls.clone(),
                stall,
            },
            polls,
        )
    }
}

impl Stream for ScriptedStream {
    type Item = Result<Vec<u8>, ()>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.set(self.polls.get() + 1);
        match self.items.next() {
            Some(item) => Poll::Ready(Some(item)),
            None if self.stall => Poll::Pending,
            None => Poll::Ready(None),
        }
    }
}

fn bytes(result: &Result<Zeroizing<Vec<u8>>, WireError>) -> Result<&[u8], WireError> {
    result.as_ref().map(|b| b.as_slice()).map_err(|e| *e)
}

fn collect(
    declared: Option<usize>,
    items: Vec<Result<Vec<u8>, ()>>,
) -> (Result<Zeroizing<Vec<u8>>, WireError>, usize) {
    let (stream, polls) = ScriptedStream::new(items, false);
    let result = block_on(validation_body(declared, stream));
    (result, polls.get())
}

// --- validation_body -----------------------------------------------------

#[test]
fn body_collects_chunks_in_order() {
    let (result, _) = collect(Some(6), vec![Ok(b"ab".to_vec()), Ok(b"cdef".to_vec())]);
    assert_eq!(bytes(&result), Ok(&b"abcdef"[..]));
}

#[test]
fn empty_stream_is_an_empty_success() {
    let (result, _) = collect(None, vec![]);
    assert_eq!(bytes(&result), Ok(&b""[..]));
}

#[test]
fn body_exactly_at_cap_is_accepted() {
    let (result, _) = collect(Some(CAP), vec![Ok(vec![b'x'; CAP])]);
    assert_eq!(bytes(&result).map(<[u8]>::len), Ok(CAP));
    let (result, _) = collect(None, vec![Ok(vec![b'x'; CAP - 1]), Ok(vec![b'y'])]);
    assert_eq!(bytes(&result).map(<[u8]>::len), Ok(CAP));
}

#[test]
fn single_oversized_chunk_is_rejected_and_stops_the_stream() {
    let (stream, polls) = ScriptedStream::new(
        vec![Ok(vec![b'x'; CAP + 1]), Ok(b"never read".to_vec())],
        false,
    );
    let result = block_on(validation_body(None, stream));
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
    assert_eq!(polls.get(), 1, "no chunk is pulled after the cap trips");
}

#[test]
fn cumulative_overflow_is_rejected_and_stops_the_stream() {
    let (stream, polls) = ScriptedStream::new(
        vec![
            Ok(vec![b'x'; CAP - 1]),
            Ok(vec![b'y', b'z']),
            Ok(b"never read".to_vec()),
        ],
        false,
    );
    let result = block_on(validation_body(None, stream));
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
    assert_eq!(polls.get(), 2);
}

#[test]
fn declared_oversize_never_polls_the_stream() {
    let (stream, polls) = ScriptedStream::new(vec![Ok(b"{}".to_vec())], false);
    let result = block_on(validation_body(Some(CAP + 1), stream));
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
    assert_eq!(polls.get(), 0);
    let (stream, polls) = ScriptedStream::new(vec![], false);
    let result = block_on(validation_body(Some(usize::MAX), stream));
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
    assert_eq!(polls.get(), 0);
}

#[test]
fn declared_length_is_only_an_early_hint() {
    // A small declared length does not license more than the cap (for
    // example when the decoded body is larger than the wire length).
    let (result, _) = collect(Some(2), vec![Ok(vec![b'x'; CAP + 1])]);
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
}

#[test]
fn stream_error_after_valid_bytes_is_never_a_success() {
    let (result, polls) = collect(
        Some(2),
        vec![Ok(b"{}".to_vec()), Err(()), Ok(b"never read".to_vec())],
    );
    assert_eq!(bytes(&result), Err(WireError::Unavailable));
    assert_eq!(polls, 2);
    let (result, _) = collect(None, vec![Err(())]);
    assert_eq!(bytes(&result), Err(WireError::Unavailable));
}

#[test]
fn ready_empty_chunks_cannot_starve_the_deadline() {
    // An endless stream of ready empty chunks is cut off after a bounded
    // number of polls instead of spinning until the timer (never) runs.
    let (stream, polls) = ScriptedStream::new(vec![Ok(Vec::new()); CAP + 1000], false);
    let result = block_on(validation_body(None, stream));
    assert_eq!(bytes(&result), Err(WireError::InvalidResponse));
    assert_eq!(polls.get(), CAP + 2);
    // Exactly the allowed number of empty chunks still succeeds.
    let (result, _) = collect(None, vec![Ok(Vec::new()); CAP + 1]);
    assert_eq!(bytes(&result), Ok(&b""[..]));
}

// --- timed_exchange ------------------------------------------------------

fn head(status: u16, cooldown_s: u64) -> ResponseHead {
    ResponseHead { status, cooldown_s }
}

#[test]
fn completed_operation_returns_body_and_head() {
    let cell = Cell::new(None);
    let exchange = block_on(timed_exchange(
        &cell,
        async {
            cell.set(Some(head(200, 1)));
            Ok(Zeroizing::new(b"ok".to_vec()))
        },
        60_000,
    ));
    assert_eq!(exchange.head, Some(head(200, 1)));
    assert_eq!(bytes(&exchange.body), Ok(&b"ok"[..]));
    assert_eq!(exchange.cooldown_s(), 1);
}

#[test]
fn stalled_body_keeps_the_received_head_and_cooldown() {
    let cell = Cell::new(None);
    let (stream, _) = ScriptedStream::new(vec![Ok(b"{\"valid\":".to_vec())], true);
    let exchange = block_on(timed_exchange(
        &cell,
        async {
            cell.set(Some(head(429, 3_600)));
            validation_body(None, stream).await
        },
        SHORT_TIMEOUT_MS,
    ));
    assert_eq!(bytes(&exchange.body), Err(WireError::Unavailable));
    assert_eq!(exchange.head, Some(head(429, 3_600)));
    assert_eq!(exchange.cooldown_s(), 3_600);
}

#[test]
fn pending_body_with_no_bytes_keeps_the_head() {
    let cell = Cell::new(None);
    let exchange = block_on(timed_exchange(
        &cell,
        async {
            cell.set(Some(head(200, 7)));
            std::future::pending::<Result<Zeroizing<Vec<u8>>, WireError>>().await
        },
        SHORT_TIMEOUT_MS,
    ));
    assert_eq!(bytes(&exchange.body), Err(WireError::Unavailable));
    assert_eq!(exchange.cooldown_s(), 7);
}

#[test]
fn failure_after_head_keeps_the_head() {
    let cell = Cell::new(None);
    let exchange = block_on(timed_exchange(
        &cell,
        async {
            cell.set(Some(head(503, u64::MAX)));
            Err(WireError::Unavailable)
        },
        60_000,
    ));
    assert_eq!(bytes(&exchange.body), Err(WireError::Unavailable));
    assert_eq!(exchange.cooldown_s(), u64::MAX);
}

#[test]
fn pre_header_deadline_uses_the_fallback_cooldown() {
    let cell = Cell::new(None);
    let exchange = block_on(timed_exchange(
        &cell,
        std::future::pending::<Result<Zeroizing<Vec<u8>>, WireError>>(),
        SHORT_TIMEOUT_MS,
    ));
    assert_eq!(bytes(&exchange.body), Err(WireError::Unavailable));
    assert!(exchange.head.is_none());
    assert_eq!(exchange.cooldown_s(), listenbrainz::DEFAULT_429_WAIT_S);
}

#[test]
fn pre_header_failure_uses_the_fallback_cooldown() {
    let cell = Cell::new(None);
    let exchange = block_on(timed_exchange(
        &cell,
        async { Err(WireError::Unavailable) },
        60_000,
    ));
    assert!(exchange.head.is_none());
    assert_eq!(exchange.cooldown_s(), listenbrainz::DEFAULT_429_WAIT_S);
}

struct DropFlag(Rc<Cell<usize>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn deadline_drops_the_operation_and_its_guard_before_returning() {
    let drops = Rc::new(Cell::new(0));
    let cell = Cell::new(None);
    let guard = DropFlag(drops.clone());
    let exchange = block_on(timed_exchange(
        &cell,
        async move {
            let _guard = guard;
            std::future::pending::<Result<Zeroizing<Vec<u8>>, WireError>>().await
        },
        SHORT_TIMEOUT_MS,
    ));
    assert_eq!(bytes(&exchange.body), Err(WireError::Unavailable));
    assert_eq!(drops.get(), 1);
}

#[test]
fn dropping_the_timed_exchange_drops_the_operation_guard() {
    let drops = Rc::new(Cell::new(0));
    let cell = Cell::new(None);
    let guard = DropFlag(drops.clone());
    let mut future = Box::pin(timed_exchange(
        &cell,
        async move {
            let _guard = guard;
            std::future::pending::<Result<Zeroizing<Vec<u8>>, WireError>>().await
        },
        600_000,
    ));
    assert!(poll_once(future.as_mut()).is_pending());
    assert_eq!(drops.get(), 0, "guard lives while the fetch is in flight");
    drop(future);
    assert_eq!(drops.get(), 1);
}

// --- ResponseHead / redirect policy -------------------------------------

#[test]
fn redirects_are_manual() {
    assert!(matches!(redirect_policy(), RequestRedirect::Manual));
}

#[test]
fn numeric_headers_produce_relative_cooldowns() {
    let now = 1_000_000;
    assert_eq!(ResponseHead::new(now, 200, &[]), head(200, 1));
    assert_eq!(ResponseHead::new(now, 429, &[]), head(429, 60));
    assert_eq!(
        ResponseHead::new(now, 429, &[("Retry-After", "7")]),
        head(429, 7)
    );
    assert_eq!(
        ResponseHead::new(now, 429, &[("x-ratelimit-reset-in", "30")]),
        head(429, 30)
    );
    assert_eq!(
        ResponseHead::new(now, 200, &[("x-ratelimit-remaining", "0")]),
        head(200, 10)
    );
    assert_eq!(
        ResponseHead::new(
            now,
            200,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1000045"),
            ],
        ),
        head(200, 45)
    );
    // A valid Retry-After is a floor even on success.
    assert_eq!(
        ResponseHead::new(now, 200, &[("retry-after", "120")]),
        head(200, 120)
    );
}

#[test]
fn comma_coalesced_numeric_fields_use_the_largest_wait() {
    let now = 1_000_000;
    let cooldown = |status, headers: &[(&str, &str)]| ResponseHead::new(now, status, headers);
    // Remaining "5, 0" means exhausted; reset-in "2, 30" waits for the 30.
    let exhausted = cooldown(
        200,
        &[
            ("x-ratelimit-remaining", "5, 0"),
            ("x-ratelimit-reset-in", "2, 30"),
        ],
    );
    assert_eq!(exhausted, head(200, 30));
    assert!(exhausted.cooldown_s >= 30);
    assert_eq!(
        cooldown(429, &[("x-ratelimit-reset-in", "2, 30")]),
        head(429, 30)
    );
    assert_eq!(
        cooldown(429, &[("x-ratelimit-reset", "1000010,1000040")]),
        head(429, 40)
    );
}

#[test]
fn coalesced_http_date_retry_after_is_not_split() {
    // Splitting at the comma would leave a fragment that is not obviously
    // invalid; the whole value must stay one unsupported Retry-After.
    assert_eq!(
        ResponseHead::new(
            1_000_000,
            429,
            &[
                ("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT"),
                ("x-ratelimit-reset-in", "2, 30"),
            ],
        ),
        head(429, u64::MAX)
    );
    assert_eq!(
        ResponseHead::new(1_000_000, 200, &[("retry-after", "5, 10")]),
        head(200, u64::MAX)
    );
}

#[test]
fn unsupported_or_malformed_retry_after_fails_closed() {
    let now = 1_000_000;
    for value in [
        "Wed, 21 Oct 2015 07:28:00 GMT",
        "",
        "-1",
        "1.5",
        "abc",
        "99999999999999999999999999",
    ] {
        for status in [200, 429, 503] {
            assert_eq!(
                ResponseHead::new(now, status, &[("retry-after", value)]),
                head(status, u64::MAX),
                "status {status} value {value:?}"
            );
        }
    }
}

#[test]
fn fail_closed_sentinel_is_not_shortened_near_the_clock_limit() {
    let now = u64::MAX - 5;
    assert_eq!(
        ResponseHead::new(
            now,
            429,
            &[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]
        ),
        head(429, u64::MAX)
    );
    // A numeric wait that saturates the absolute time also stays at the
    // sentinel instead of becoming `u64::MAX - now`.
    assert_eq!(
        ResponseHead::new(now, 429, &[("retry-after", "100")]),
        head(429, u64::MAX)
    );
}
