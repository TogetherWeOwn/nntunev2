//! The outer transcode-front -> Durable Object hop is bounded through
//! response completion (headers AND capped body drain, one budget).
//!
//! ONE total budget spans the object reply headers plus the capped
//! body read. `settle_do_reply` races the fetch, then the drain,
//! against the SAME deadline, polling the deadline first (an
//! already-fired deadline aborts even a ready fetch). Partial
//! progress never re-arms the budget: a dribbling body cannot reset
//! its deadline. The drain is the existing capped accumulation
//! (exactly-at-cap admitted, the crossing chunk aborts with the
//! excess never retained, no truncated prefix returned). Expiry,
//! transport errors and over-cap bodies all settle as the EXISTING
//! `transcode_unavailable_fault()` (503) — no new fault code. A
//! reply that completes inside the budget relays VERBATIM for every
//! status: the front never classifies the object's verdict.
//!
//! Futures are driven by a bounded manual poll loop (noop waker),
//! so settlement is a poll-budget assertion, not a flake. The wasm
//! wiring pins at the bottom prove the front route actually settles
//! through this seam under the 600 s knob.
//!
//! Run only this file:
//!
//! ```sh
//! python3 scripts/cargo_cache.py run -- test \
//!     --manifest-path workers/transcode/Cargo.toml \
//!     --test do_reply_deadline_acceptance
//! ```

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::stream::{self, Stream};
use nntune_transcode::contract::{settle_do_reply, transcode_unavailable_fault, MAX_RESULT_BYTES};

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");

fn normalized_runtime() -> String {
    RUNTIME_RS.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn drive<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    for _ in 0..100_000 {
        if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
    }
    panic!("future did not settle within 100_000 polls: local settlement is the contract");
}

fn no_deadline() -> std::future::Pending<()> {
    std::future::pending()
}

fn expired_deadline() -> std::future::Ready<()> {
    std::future::ready(())
}

struct FiresAfterPolls {
    remaining: u32,
}

impl Future for FiresAfterPolls {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.remaining == 0 {
            return Poll::Ready(());
        }
        self.remaining -= 1;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// One chunk, then silence forever: the dribbling body that must not
/// re-arm its deadline.
struct Dribble {
    yielded: bool,
}

impl Stream for Dribble {
    type Item = Result<Vec<u8>, ()>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.yielded {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.yielded = true;
        Poll::Ready(Some(Ok(vec![1u8; 8])))
    }
}

#[test]
fn completed_reply_relays_verbatim_for_every_status() {
    let chunks = vec![
        Ok(b"hello".to_vec()),
        Ok(b" ".to_vec()),
        Ok(b"world".to_vec()),
    ];
    let fetch = std::future::ready(Ok((200u16, stream::iter(chunks))));
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Ok((200u16, b"hello world".to_vec()))
    );
    // Non-200 verdicts relay untouched: bounding the hop must not
    // start classifying the object's answer.
    let chunks = vec![Ok(br#"{"error":"scratch_missing"}"#.to_vec())];
    let fetch = std::future::ready(Ok((404u16, stream::iter(chunks))));
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Ok((404u16, br#"{"error":"scratch_missing"}"#.to_vec()))
    );
}

#[test]
fn expired_deadline_aborts_even_a_ready_fetch() {
    let chunks = vec![Ok(b"done".to_vec())];
    let fetch = std::future::ready(Ok((200u16, stream::iter(chunks))));
    assert_eq!(
        drive(settle_do_reply(fetch, expired_deadline())),
        Err(transcode_unavailable_fault())
    );
}

#[test]
fn fetch_transport_error_settles_as_unavailable() {
    let fetch = std::future::ready(Err::<
        (u16, stream::Iter<std::vec::IntoIter<Result<Vec<u8>, ()>>>),
        (),
    >(()));
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Err(transcode_unavailable_fault())
    );
}

#[test]
fn chunk_error_aborts_without_a_partial_reply() {
    let chunks = vec![Ok(b"prefix".to_vec()), Err(())];
    let fetch = std::future::ready(Ok((200u16, stream::iter(chunks))));
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Err(transcode_unavailable_fault())
    );
}

#[test]
fn over_cap_body_aborts_and_exact_cap_relays() {
    let full = vec![7u8; MAX_RESULT_BYTES];
    let chunks = vec![
        Ok(full[..MAX_RESULT_BYTES - 1].to_vec()),
        Ok(vec![8u8, 9u8]),
    ];
    let fetch = std::future::ready(Ok((200u16, stream::iter(chunks))));
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Err(transcode_unavailable_fault())
    );
    let chunks = vec![Ok(vec![7u8; MAX_RESULT_BYTES - 1]), Ok(vec![8u8])];
    let fetch = std::future::ready(Ok((200u16, stream::iter(chunks))));
    let mut expected = vec![7u8; MAX_RESULT_BYTES - 1];
    expected.push(8u8);
    assert_eq!(
        drive(settle_do_reply(fetch, no_deadline())),
        Ok((200u16, expected))
    );
}

#[test]
fn deadline_firing_mid_drain_aborts_the_dribble() {
    let fetch = std::future::ready(Ok((200u16, Dribble { yielded: false })));
    assert_eq!(
        drive(settle_do_reply(fetch, FiresAfterPolls { remaining: 3 })),
        Err(transcode_unavailable_fault())
    );
}

#[test]
fn front_route_settles_through_the_seam_under_the_600s_knob() {
    let text = normalized_runtime();
    assert!(
        text.contains("const DO_TRANSCODE_DEADLINE_MS: u64 = 600_000;"),
        "the front-to-DO cutoff must stay a single named knob at 600 s"
    );
    assert!(
        text.contains("Delay::from(Duration::from_millis(DO_TRANSCODE_DEADLINE_MS))"),
        "the front fetch must arm its deadline from the knob"
    );
    assert!(
        text.contains("settle_do_reply(fetch, deadline)"),
        "the front fetch must settle headers plus capped body through settle_do_reply"
    );
    assert!(
        text.contains("Ok((status, bytes)) => json_response(status, bytes)"),
        "a completed object reply must relay verbatim, never classified"
    );
}
