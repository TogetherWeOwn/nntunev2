//! Stalled MusicBrainz fetches abort after a configured deadline.
//!
//! PROTECTED acceptance spec: the implementation is built against this
//! file; this file is never edited to match an implementation.
//!
//! Gap under test (`workers/ingest/src/queue_consumer.rs`, `fetch_mb_body`):
//! the body read enforces the response byte caps but no deadline, so
//! `body.next().await` on a trickling or hung MB response pends until the
//! coarse Queue delivery timeout, holding delivery concurrency. The
//! sibling indexer client (`indexer_fetch::FETCH_TIMEOUT_MS`) already
//! enforces a total deadline on the same untrusted-upstream boundary.
//!
//! Contract the implementation must provide (exact public surface):
//!
//! ```rust,ignore
//! // in nntune_ingest::queue_consumer, next to the MAX_MB_* caps it
//! // composes with
//! pub const MB_FETCH_DEADLINE_MS: u64 = 15_000;
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq)]
//! pub enum MbFetchFailure { Deadline, Oversize, NonUtf8, Transport }
//!
//! impl MbFetchFailure {
//!     pub fn match_failure(self) -> MatchFailure; // every variant: Transient
//! }
//!
//! pub async fn collect_mb_body<S, D>(
//!     body: S,
//!     max_bytes: usize,
//!     deadline: D,
//! ) -> Result<String, MbFetchFailure>
//! where
//!     S: futures_util::Stream<Item = Result<Vec<u8>, ()>> + Unpin,
//!     D: std::future::Future<Output = ()> + Unpin;
//! ```
//!
//! Semantics pinned below:
//!
//! - `MB_FETCH_DEADLINE_MS == 15_000`, exactly the indexer client's total
//!   (`indexer_fetch::FETCH_TIMEOUT_MS`, the reused vault total): one
//!   upstream-read budget across clients, composing with the existing
//!   retry budget instead of inventing a second number.
//! - The deadline is TOTAL for the body read, not per-chunk idle time:
//!   one `deadline` future spans the whole drain. Partial progress (a
//!   chunk already collected) does not re-arm it.
//! - A body still incomplete when `deadline` resolves aborts with
//!   `Err(MbFetchFailure::Deadline)`: never a partial body, never `Ok`.
//! - Every failure classifies `Transient` (retry with backoff), never
//!   `Permanent`: a stalled mirror may answer on redelivery, exactly like
//!   a 5xx, an oversize body, or a non-UTF-8 body today.
//! - Behavior before the deadline is byte-for-byte today's: chunks
//!   accumulate (multi-byte UTF-8 split across chunks decodes), the exact
//!   `max_bytes` fit is admitted, the chunk that would cross `max_bytes`
//!   fails `Oversize`, invalid UTF-8 fails `NonUtf8`, a chunk-level
//!   transport error fails `Transport`, and an empty body is `Ok("")`.
//! - Production wiring: `fetch_mb_body` keeps its
//!   `Result<(u16, String), ()>` shape, arms its deadline from
//!   `MB_FETCH_DEADLINE_MS` BEFORE issuing `fetcher.fetch(...)` (so
//!   slow-headers time counts against the same total), drains through
//!   `collect_mb_body`, and both callers keep mapping `Err(())` to the
//!   existing transient errors. Racing the headers await itself is not
//!   pinned here; the scripted stall is the body read, which is the
//!   observed wedge. (The Worker body yields `Bytes` chunks; adapting
//!   them to the seam's `Vec<u8>` chunks in `fetch_mb_body` is expected.)
//! - `collect_mb_body` lives in `queue_consumer.rs` textually between
//!   `fetch_mb_body` and `fetch_release_match`, keeping the existing cap
//!   predicate (`bytes.len() + chunk.len() > max_bytes`) and a
//!   `return Err(());` fail-closed exit in that span: the merged
//!   cap-literal suite slices exactly that span and must keep passing
//!   unedited.
//!
//! Deterministic time: tests never sleep. An expired deadline is
//! `future::ready(())`, an unexpired one is `future::pending()`, and the
//! poll-counted doubles below prove the abort happens when the deadline
//! fires — not before — without any wall clock.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/ingest/Cargo.toml --test mb_fetch_deadline
//! ```

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use nntune_ingest::indexer_fetch::FETCH_TIMEOUT_MS;
use nntune_ingest::matching::MatchFailure;
use nntune_ingest::queue_consumer::{collect_mb_body, MbFetchFailure, MB_FETCH_DEADLINE_MS};
use nntune_ingest::retry::{JobError, ERR_MATCH_FAILED};

/// Deadline that never fires: the pre-deadline behavior controls.
fn no_deadline() -> std::future::Pending<()> {
    std::future::pending()
}

/// Deadline that has already fired.
fn expired_deadline() -> std::future::Ready<()> {
    std::future::ready(())
}

/// A boxed chunk stream so every scripted body shares one type.
type ChunkStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, ()>>>>;

fn body_of(chunks: Vec<Result<Vec<u8>, ()>>) -> ChunkStream {
    Box::pin(stream::iter(chunks))
}

/// A body that never yields a chunk and never ends.
fn hanging_body() -> ChunkStream {
    Box::pin(stream::pending())
}

/// A deadline that fires after `polls` pending polls, waking itself each
/// time so the executor keeps re-polling without a wall clock.
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

/// A body whose single chunk arrives only after `stall_polls` pending
/// polls (waking itself each time), then ends.
struct StallingBody {
    stall_polls: u32,
    chunk: Option<Vec<u8>>,
}

impl Stream for StallingBody {
    type Item = Result<Vec<u8>, ()>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.stall_polls > 0 {
            self.stall_polls -= 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(self.chunk.take().map(Ok))
    }
}

#[test]
fn deadline_constant_is_15s_matching_the_indexer_total() {
    assert_eq!(
        MB_FETCH_DEADLINE_MS, 15_000,
        "the MB fetch deadline must stay exactly 15_000 ms"
    );
    assert_eq!(
        MB_FETCH_DEADLINE_MS, FETCH_TIMEOUT_MS,
        "the MB deadline reuses the indexer client's total, not a second budget"
    );
}

#[tokio::test]
async fn hanging_body_aborts_at_the_deadline() {
    // The wedge from the failure scenario: headers arrived, the body
    // never yields. With no deadline this await never returns; the
    // deadline must abort it instead.
    let result = collect_mb_body(hanging_body(), 64 * 1024, expired_deadline()).await;
    assert_eq!(
        result,
        Err(MbFetchFailure::Deadline),
        "a stalled body must abort at the deadline, not pend forever"
    );
}

#[tokio::test]
async fn trickling_body_aborts_without_a_partial_body() {
    // One chunk lands, then the body hangs: collected progress must not
    // be returned as a truncated body once the deadline fires.
    let body: ChunkStream =
        Box::pin(stream::iter(vec![Ok(b"{\"partial\":".to_vec())]).chain(stream::pending()));
    let result = collect_mb_body(body, 64 * 1024, expired_deadline()).await;
    assert_eq!(
        result,
        Err(MbFetchFailure::Deadline),
        "a trickle-then-hang body aborts whole, never a partial body"
    );
}

#[tokio::test]
async fn abort_waits_for_the_deadline_not_the_first_stall() {
    // The body stalls for a few polls but completes before the deadline
    // fires: no abort. This pins "total deadline", not "abort on the
    // first pending chunk".
    let body = StallingBody {
        stall_polls: 3,
        chunk: Some(b"complete".to_vec()),
    };
    let result = collect_mb_body(Box::pin(body), 64, FiresAfterPolls { remaining: 50 }).await;
    assert_eq!(
        result.as_deref(),
        Ok("complete"),
        "a body that completes before the deadline must succeed"
    );

    // The same stall outliving the deadline aborts: the deadline firing,
    // not the stall alone, is what ends the read.
    let body = StallingBody {
        stall_polls: 50,
        chunk: Some(b"too late".to_vec()),
    };
    let result = collect_mb_body(Box::pin(body), 64, FiresAfterPolls { remaining: 3 }).await;
    assert_eq!(
        result,
        Err(MbFetchFailure::Deadline),
        "a body still incomplete when the deadline fires must abort"
    );
}

#[test]
fn every_failure_classifies_transient_never_permanent() {
    for failure in [
        MbFetchFailure::Deadline,
        MbFetchFailure::Oversize,
        MbFetchFailure::NonUtf8,
        MbFetchFailure::Transport,
    ] {
        assert_eq!(
            failure.match_failure(),
            MatchFailure::Transient,
            "{failure:?} must stay retryable, never Permanent"
        );
    }
    // The search route's existing transient shape for a failed fetch is
    // part of the classification contract (unchanged by the deadline).
    let err = JobError::transient(ERR_MATCH_FAILED, "musicbrainz lookup unavailable");
    assert_eq!(err.code, "match_failed");
    assert!(err.transient, "an aborted MB fetch must stay retryable");
    assert!(!err.made_progress, "an aborted fetch advances nothing");
}

#[tokio::test]
async fn fast_body_is_unchanged() {
    let body = body_of(vec![
        Ok(b"{\"id\":".to_vec()),
        Ok(Vec::new()), // an empty chunk is not end-of-body
        Ok(b"\"abc\"}".to_vec()),
    ]);
    let result = collect_mb_body(body, 64 * 1024, no_deadline()).await;
    assert_eq!(result.as_deref(), Ok("{\"id\":\"abc\"}"));
}

#[tokio::test]
async fn empty_body_is_unchanged() {
    let result = collect_mb_body(body_of(Vec::new()), 64 * 1024, no_deadline()).await;
    assert_eq!(
        result.as_deref(),
        Ok(""),
        "an empty body still reaches the parse step as today"
    );
}

#[tokio::test]
async fn oversize_is_unchanged_exact_fit_admitted() {
    // Exact fit on the running total is admitted (strict `>` today).
    let exact = body_of(vec![Ok(vec![b'x'; 5]), Ok(vec![b'y'; 3])]);
    assert_eq!(
        collect_mb_body(exact, 8, no_deadline()).await.as_deref(),
        Ok("xxxxxyyy")
    );
    // The chunk that would cross the cap fails, with no partial body.
    let crossing = body_of(vec![Ok(vec![b'x'; 5]), Ok(vec![b'y'; 4])]);
    assert_eq!(
        collect_mb_body(crossing, 8, no_deadline()).await,
        Err(MbFetchFailure::Oversize)
    );
    // A single chunk past the cap fails the same way.
    let huge = body_of(vec![Ok(vec![b'x'; 9])]);
    assert_eq!(
        collect_mb_body(huge, 8, no_deadline()).await,
        Err(MbFetchFailure::Oversize)
    );
}

#[tokio::test]
async fn non_utf8_is_unchanged_and_split_sequences_decode() {
    // Invalid UTF-8 fails closed exactly as today.
    let invalid = body_of(vec![Ok(vec![0xff, 0xfe])]);
    assert_eq!(
        collect_mb_body(invalid, 64, no_deadline()).await,
        Err(MbFetchFailure::NonUtf8)
    );
    // Bytes accumulate before decoding: a multi-byte character split
    // across chunks ("é" = 0xC3 0xA9) is valid, as with today's
    // buffer-then-`String::from_utf8`.
    let split = body_of(vec![Ok(vec![0xc3]), Ok(vec![0xa9])]);
    assert_eq!(
        collect_mb_body(split, 64, no_deadline()).await.as_deref(),
        Ok("é")
    );
}

#[tokio::test]
async fn transport_chunk_error_is_unchanged() {
    let body = body_of(vec![Ok(b"partial".to_vec()), Err(())]);
    assert_eq!(
        collect_mb_body(body, 64, no_deadline()).await,
        Err(MbFetchFailure::Transport),
        "a mid-body transport error fails whole, never a partial body"
    );
}

/// Production source before the unit-test module, so this file's own
/// literals cannot satisfy the wiring pins.
fn production_source() -> &'static str {
    let source = include_str!("../src/queue_consumer.rs");
    source
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Slice of one function body bounded by the next function.
fn fn_body(source: &str, name: &str, next: &str) -> String {
    let after = source
        .split_once(name)
        .unwrap_or_else(|| panic!("{name} must exist"))
        .1;
    after
        .split_once(next)
        .unwrap_or_else(|| panic!("{next} must bound {name}"))
        .0
        .to_string()
}

#[test]
fn production_fetch_arms_the_deadline_before_fetching() {
    let fetcher = fn_body(
        production_source(),
        "async fn fetch_mb_body",
        "async fn fetch_release_match",
    );
    assert!(
        fetcher.contains("Result<(u16, String), ()>"),
        "fetch_mb_body keeps its Result<(u16, String), ()> shape so both callers' classification is untouched"
    );
    assert!(
        fetcher.contains("MB_FETCH_DEADLINE_MS"),
        "fetch_mb_body must arm its deadline from the named MB_FETCH_DEADLINE_MS constant"
    );
    assert!(
        fetcher.contains("collect_mb_body("),
        "fetch_mb_body must drain through collect_mb_body (the seam proven above), not a second inline loop"
    );
    let armed_at = fetcher
        .find("MB_FETCH_DEADLINE_MS")
        .expect("deadline constant must appear in fetch_mb_body");
    let fetch_at = fetcher
        .find("fetcher.fetch(")
        .expect("fetch_mb_body must still fetch through the MB_API binding");
    assert!(
        armed_at < fetch_at,
        "the deadline must be armed before fetcher.fetch(...) so slow headers count against the same total"
    );
    // Compatibility with the merged cap-literal suite, which slices this
    // same span: extracting the drain must not move the cap predicate or
    // the fail-closed exit out of it (that suite stays unedited).
    assert!(
        fetcher.contains("if bytes.len() + chunk.len() > max_bytes {"),
        "the cap predicate must stay in the fetch_mb_body..fetch_release_match span (collect_mb_body lives there)"
    );
    assert!(
        fetcher.contains("return Err(());"),
        "the fail-closed Err(()) exit must stay in the fetch_mb_body..fetch_release_match span"
    );
}

#[test]
fn production_callers_keep_the_transient_mapping() {
    let source = production_source();
    let search = fn_body(
        source,
        "async fn fetch_release_match",
        "async fn fetch_release_recordings",
    );
    assert!(
        search.contains(".map_err(|()| unavailable("),
        "the search route must keep mapping a failed (now also aborted) fetch to the transient `unavailable` error"
    );
    let recordings = fn_body(
        source,
        "async fn fetch_release_recordings",
        "async fn resolve_library_write",
    );
    assert!(
        recordings.contains("MatchFailure::Transient"),
        "the recordings route must keep classifying a failed (now also aborted) fetch as Transient"
    );
}
