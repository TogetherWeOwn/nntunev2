//! The outer ingest -> repair service hop is bounded through response
//! completion (headers AND capped body drain, one total budget).
//!
//! PROTECTED acceptance spec: the implementation is built against this
//! file; this file is never edited to match an implementation.
//!
//! Gap under test (`workers/ingest/src/repair.rs`, `call_repair`): the
//! call awaits `fetcher.fetch_request(request)` for the reply headers
//! and then `drain_repair_stream(...)` for the capped body, with no
//! deadline on either await. A hung or dribbling repair Worker (or a
//! wedged hop beneath it) therefore pends the queue delivery until
//! some coarse outer timeout. The already-bounded Container fetches
//! inside the repair Worker are a different, inner hop and bound
//! nothing here.
//!
//! Contract the implementation must provide (exact public surface):
//!
//! ```rust,ignore
//! // in nntune_ingest::repair, next to MAX_REPAIR_REPLY_BYTES
//! pub const REPAIR_CALL_DEADLINE_MS: u64 = /* finite, documented */;
//!
//! pub async fn settle_repair_call<F, S, D>(fetch: F, deadline: D) -> RepairCall
//! where
//!     F: std::future::Future<Output = Result<(u16, S), ()>> + Unpin,
//!     S: futures_util::Stream<Item = Result<Vec<u8>, ()>> + Unpin,
//!     D: std::future::Future<Output = ()> + Unpin;
//!
//! // production entry points keep their shapes:
//! pub async fn call_repair(env: &Env, req: &RepairRequest) -> RepairCall;
//! pub async fn call_repair_with_deadline(
//!     env: &Env, req: &RepairRequest, deadline_ms: u64,
//! ) -> RepairCall;
//! ```
//!
//! Semantics pinned below:
//!
//! - ONE total budget spans reply headers plus the capped body drain.
//!   `settle_repair_call` races the fetch `F`, then the drain of `S`,
//!   against the SAME `deadline` (the existing generic race in
//!   `pool_deadline::race_against_deadline` is the house shape:
//!   deadline polled first, so an already-fired deadline aborts even
//!   a ready fetch). Partial progress — headers landed, chunks
//!   collected — never re-arms the budget: a dribbling body cannot
//!   reset its deadline.
//! - Expiry (either stage), a fetch transport error, a chunk-level
//!   transport error, and the chunk that would cross
//!   `MAX_REPAIR_REPLY_BYTES` all settle as the EXISTING
//!   `RepairCall::Retry`: no new `RepairCall` variant and never a
//!   truncated prefix handed to the classifier. A body complete
//!   before expiry classifies exactly as today via
//!   `classify_repair_reply` (only a well-formed 200 `"repaired"`
//!   document is terminal success; an at-cap 65,536-byte reply is
//!   admitted, 65,537 is not). After an expiry, a later legitimate
//!   call succeeds normally: expiry manufactures no terminal outcome
//!   and poisons nothing.
//! - The deadline future is owned by the settlement and dropped on
//!   EVERY completion path (success, transport failure, expiry): no
//!   leaked, forgotten, or spawned timers (`mem::forget`,
//!   `Box::leak`, `spawn_local` are banned in `repair.rs`).
//! - Production wiring mirrors the pool-stub shape:
//!   `call_repair` delegates as
//!   `call_repair_with_deadline(env, req, REPAIR_CALL_DEADLINE_MS)`,
//!   and `call_repair_with_deadline` arms
//!   `Delay::from(Duration::from_millis(deadline_ms))` BEFORE the
//!   `fetch_request(` it issues (slow headers count against the same
//!   total; `Delay` registers its timer on first poll, and the
//!   deadline-first race polls it from the start), then settles
//!   through `settle_repair_call` (adapting the http body stream's
//!   chunks to `Vec<u8>` and its errors to `()` is expected).
//! - `REPAIR_CALL_DEADLINE_MS` is finite and DOCUMENTED at its
//!   declaration against the child bounds it must outlast: it
//!   strictly exceeds the repair front's own durable-object budget
//!   (`DO_REPAIR_DEADLINE_MS` in `workers/repair/src/runtime.rs`,
//!   itself above the Container's 150 s repair deadline, which does
//!   NOT include input staging or output relay), so the inner hop's
//!   `repair_unavailable` verdict can arrive instead of this timer
//!   firing first; it is never the 10 s pool/Container budget and
//!   never an unbounded placeholder (< 1 h). The exact value is the
//!   implementer's documented choice; the relations are pinned.
//!
//! What this does NOT prove: dropping a Rust future locally does not
//! prove the remote Worker, Durable Object, or Container cancelled
//! or terminated the underlying work, and no real service binding is
//! exercised. Idempotency and result-marker-last live in the repair
//! Worker and are untouched by this hop settling early; a retry
//! replays against those existing guarantees.
//!
//! Deterministic time: tests never sleep. Fakes are ready/pending
//! futures, pending/chunked streams, and poll-counted deadlines, all
//! driven by a bounded manual poll loop, so "settles locally" is
//! asserted with a poll budget instead of a wall clock.
//!
//! Run only this file:
//!
//! ```sh
//! python3 scripts/cargo_cache.py run -- test \
//!     --manifest-path workers/ingest/Cargo.toml \
//!     --test repair_call_deadline_acceptance
//! ```

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use nntune_ingest::repair::{
    classify_repair_reply, settle_repair_call, RepairCall, REPAIR_CALL_DEADLINE_MS,
};

/// Reply cap under test: `MAX_REPAIR_REPLY_BYTES == 64 << 10`.
const CAP_BYTES: usize = 64 << 10;

// ---------------------------------------------------------------------------
// Deterministic driving: a bounded manual poll loop (settlement = Ready
// within the poll budget; a hang is a failure, not a timeout flake).
// ---------------------------------------------------------------------------

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

/// Deadline that never fires: the pre-deadline behavior controls.
fn no_deadline() -> std::future::Pending<()> {
    std::future::pending()
}

/// Deadline that has already fired.
fn expired_deadline() -> std::future::Ready<()> {
    std::future::ready(())
}

/// A deadline that fires after `remaining` pending polls, waking
/// itself each time so polling continues without a wall clock.
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

/// A deadline that records whether it was dropped, so "released on
/// every completion path" is observable: leaked/forgotten timers
/// never set the flag.
struct DropProbeDeadline {
    dropped: Rc<Cell<bool>>,
    fire: bool,
}

impl Future for DropProbeDeadline {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.fire {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for DropProbeDeadline {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

// ---------------------------------------------------------------------------
// Scripted fetch / body fakes.
// ---------------------------------------------------------------------------

type ChunkStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, ()>>>>;
type FetchOut = Result<(u16, ChunkStream), ()>;

fn body_of(chunks: Vec<Result<Vec<u8>, ()>>) -> ChunkStream {
    Box::pin(stream::iter(chunks))
}

fn ready_fetch(status: u16, chunks: Vec<Result<Vec<u8>, ()>>) -> std::future::Ready<FetchOut> {
    std::future::ready(Ok((status, body_of(chunks))))
}

fn hanging_fetch() -> std::future::Pending<FetchOut> {
    std::future::pending()
}

/// A body that stalls `stall_polls` polls before EACH chunk (waking
/// itself each time): it makes steady progress, so only a TOTAL
/// deadline (never re-armed by progress) can abort it mid-body.
struct DribblingBody {
    stall_polls: u32,
    stall_left: u32,
    chunks: VecDeque<Vec<u8>>,
}

impl DribblingBody {
    fn new(stall_polls: u32, chunks: Vec<Vec<u8>>) -> Self {
        Self {
            stall_polls,
            stall_left: stall_polls,
            chunks: chunks.into(),
        }
    }
}

impl Stream for DribblingBody {
    type Item = Result<Vec<u8>, ()>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.chunks.is_empty() {
            return Poll::Ready(None);
        }
        if self.stall_left > 0 {
            self.stall_left -= 1;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.stall_left = self.stall_polls;
        Poll::Ready(self.chunks.pop_front().map(Ok))
    }
}

fn repaired_body_of_len(total_len: usize) -> Vec<u8> {
    const PREFIX: &[u8] = br#"{"status":"repaired","pad":""#;
    const SUFFIX: &[u8] = br#""}"#;
    assert!(total_len >= PREFIX.len() + SUFFIX.len());
    let mut body = Vec::with_capacity(total_len);
    body.extend_from_slice(PREFIX);
    body.extend_from_slice(&vec![b'x'; total_len - PREFIX.len() - SUFFIX.len()]);
    body.extend_from_slice(SUFFIX);
    body
}

fn split2(body: Vec<u8>) -> Vec<Result<Vec<u8>, ()>> {
    let mid = body.len() / 2;
    vec![Ok(body[..mid].to_vec()), Ok(body[mid..].to_vec())]
}

// ---------------------------------------------------------------------------
// Controls: classification behavior that must hold before and after.
// ---------------------------------------------------------------------------

#[test]
fn classifier_controls_are_unchanged() {
    assert_eq!(
        classify_repair_reply(
            200,
            br#"{"status":"repaired","exit_code":0,"file_count":3}"#
        ),
        RepairCall::Repaired
    );
    assert_eq!(
        classify_repair_reply(400, br#"{"error":"invalid_request"}"#),
        RepairCall::Unrepairable
    );
    assert_eq!(
        classify_repair_reply(503, br#"{"error":"repair_unavailable"}"#),
        RepairCall::Retry,
        "the inner hop's expiry verdict must keep classifying Retry at ingest"
    );
    assert_eq!(classify_repair_reply(200, b"not json"), RepairCall::Retry);
}

// ---------------------------------------------------------------------------
// Fast paths through the seam: pre-deadline behavior is unchanged.
// ---------------------------------------------------------------------------

#[test]
fn fast_repaired_reply_settles_repaired() {
    let out = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![
                Ok(br#"{"status":"repaired","#.to_vec()),
                Ok(Vec::new()),
                Ok(br#""exit_code":0,"file_count":3}"#.to_vec()),
            ],
        ),
        no_deadline(),
    ));
    assert_eq!(out, RepairCall::Repaired);
}

#[test]
fn fast_failure_statuses_keep_their_classification() {
    // Terminal and transient verdicts arrive through the seam intact:
    // bounding the hop must not re-map a single status.
    let terminal = drive(settle_repair_call(
        ready_fetch(400, vec![Ok(br#"{"error":"invalid_request"}"#.to_vec())]),
        no_deadline(),
    ));
    assert_eq!(terminal, RepairCall::Unrepairable);
    let unrepairable = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![Ok(
                br#"{"status":"unrepairable","exit_code":6,"file_count":0}"#.to_vec(),
            )],
        ),
        no_deadline(),
    ));
    assert_eq!(unrepairable, RepairCall::Unrepairable);
    let unavailable = drive(settle_repair_call(
        ready_fetch(503, vec![Ok(br#"{"error":"repair_unavailable"}"#.to_vec())]),
        no_deadline(),
    ));
    assert_eq!(unavailable, RepairCall::Retry);
    // A fetch-level transport error is Retry, never a guessed outcome.
    let transport = drive(settle_repair_call(
        std::future::ready(Err::<(u16, ChunkStream), ()>(())),
        no_deadline(),
    ));
    assert_eq!(transport, RepairCall::Retry);
}

#[test]
fn body_completing_before_the_deadline_succeeds() {
    // Steady progress that finishes well inside the budget is not an
    // abort: this pins "total deadline", not "abort on first stall".
    let body = DribblingBody::new(
        3,
        vec![
            br#"{"status":"repaired","#.to_vec(),
            br#""exit_code":0,"file_count":1}"#.to_vec(),
        ],
    );
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((200, Box::pin(body))));
    let out = drive(settle_repair_call(
        fetch,
        FiresAfterPolls { remaining: 500 },
    ));
    assert_eq!(out, RepairCall::Repaired);
}

#[test]
fn exact_cap_reply_is_admitted_through_the_seam() {
    let body = repaired_body_of_len(CAP_BYTES);
    assert_eq!(body.len(), 65_536);
    let out = drive(settle_repair_call(
        ready_fetch(200, split2(body)),
        no_deadline(),
    ));
    assert_eq!(
        out,
        RepairCall::Repaired,
        "a well-formed repaired reply at exactly 65,536 bytes must settle Repaired"
    );
}

#[test]
fn cap_plus_one_reply_retries_without_classifying_a_prefix() {
    let body = repaired_body_of_len(CAP_BYTES + 1);
    assert_eq!(body.len(), 65_537);
    let out = drive(settle_repair_call(
        ready_fetch(200, split2(body)),
        no_deadline(),
    ));
    assert_eq!(
        out,
        RepairCall::Retry,
        "the 65,537th byte must fail closed to Retry: no retained excess, no truncated classification"
    );
    // A complete repaired prefix followed by an over-cap chunk is
    // likewise Retry, never Repaired from the prefix.
    let mut chunks = vec![Ok(
        br#"{"status":"repaired","exit_code":0,"file_count":1}"#.to_vec()
    )];
    chunks.push(Ok(vec![b'x'; CAP_BYTES]));
    let out = drive(settle_repair_call(ready_fetch(200, chunks), no_deadline()));
    assert_eq!(out, RepairCall::Retry);
}

#[test]
fn chunk_transport_error_after_a_complete_prefix_retries() {
    let out = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![
                Ok(br#"{"status":"repaired","exit_code":0,"file_count":1}"#.to_vec()),
                Err(()),
            ],
        ),
        no_deadline(),
    ));
    assert_eq!(
        out,
        RepairCall::Retry,
        "a chunk error after a complete prefix must fail whole, never classify the prefix"
    );
}

// ---------------------------------------------------------------------------
// The deadline itself: settlement for stalled headers and stalled /
// dribbling bodies. RED on the unbounded call this suite was written
// against (there is no settlement seam and nothing would return).
// ---------------------------------------------------------------------------

#[test]
fn stalled_headers_abort_to_retry_at_the_deadline() {
    let out = drive(settle_repair_call(hanging_fetch(), expired_deadline()));
    assert_eq!(
        out,
        RepairCall::Retry,
        "headers that never arrive must settle as Retry when the deadline fires, not pend forever"
    );
    // Settlement tracks the deadline, not the first pending poll:
    // a deadline that fires after a few polls still settles the
    // never-arriving headers as Retry.
    let out = drive(settle_repair_call(
        hanging_fetch(),
        FiresAfterPolls { remaining: 2 },
    ));
    assert_eq!(out, RepairCall::Retry);
}

#[test]
fn fired_deadline_wins_over_simultaneously_ready_headers() {
    // Deadline-first polling (the house race shape): an already-fired
    // deadline aborts even when the reply is ready, so no completed
    // work slips in past the budget.
    let out = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![Ok(
                br#"{"status":"repaired","exit_code":0,"file_count":1}"#.to_vec()
            )],
        ),
        expired_deadline(),
    ));
    assert_eq!(out, RepairCall::Retry);
}

#[test]
fn stalled_body_aborts_to_retry_at_the_deadline() {
    // Headers land, the body never yields: the SAME budget (armed
    // before dispatch) ends the drain, it is not a headers-only bound.
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(stream::pending()),
    )));
    let out = drive(settle_repair_call(fetch, expired_deadline()));
    assert_eq!(
        out,
        RepairCall::Retry,
        "a body that never yields must settle as Retry at expiry, not pend forever"
    );
}

#[test]
fn dribbling_body_cannot_reset_its_deadline() {
    // One chunk lands, then the body hangs forever: collected
    // progress must not be classified once the deadline fires.
    let body: ChunkStream = Box::pin(
        stream::iter(vec![Ok(
            br#"{"status":"repaired","exit_code":0,"file_count":1}"#.to_vec(),
        )])
        .chain(stream::pending()),
    );
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((200, body)));
    let out = drive(settle_repair_call(fetch, FiresAfterPolls { remaining: 2 }));
    assert_eq!(
        out,
        RepairCall::Retry,
        "a trickle-then-hang body aborts whole at the SAME deadline, never a partial verdict"
    );

    // Steady dribble (a chunk every few polls, would complete in
    // ~40 polls): a deadline firing early aborts it anyway.
    // A per-chunk or per-progress re-arm would let this run to
    // Repaired; one total budget does not.
    let chunks = vec![
        br#"{"status":"#.to_vec(),
        br#""repaired","#.to_vec(),
        br#""exit_code":0,"#.to_vec(),
        br#""file_count":1}"#.to_vec(),
    ];
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(DribblingBody::new(10, chunks.clone())),
    )));
    let out = drive(settle_repair_call(fetch, FiresAfterPolls { remaining: 3 }));
    assert_eq!(
        out,
        RepairCall::Retry,
        "a dribbling body must not extend its own deadline by making progress"
    );
    // Control: the identical dribble inside the budget is Repaired,
    // so only the deadline (not the dribble shape) flipped it.
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(DribblingBody::new(10, chunks)),
    )));
    let out = drive(settle_repair_call(fetch, no_deadline()));
    assert_eq!(out, RepairCall::Repaired);
}

#[test]
fn expiry_then_a_legitimate_retry_succeeds() {
    let expired = drive(settle_repair_call(hanging_fetch(), expired_deadline()));
    assert_eq!(expired, RepairCall::Retry);
    // A later legitimate call succeeds normally (and a terminal
    // verdict still reads terminal): expiry manufactured neither
    // success nor a permanently poisoned hop.
    let retried = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![Ok(
                br#"{"status":"repaired","exit_code":0,"file_count":2}"#.to_vec()
            )],
        ),
        no_deadline(),
    ));
    assert_eq!(retried, RepairCall::Repaired);
    let terminal = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![Ok(
                br#"{"status":"unrepairable","exit_code":6,"file_count":0}"#.to_vec(),
            )],
        ),
        no_deadline(),
    ));
    assert_eq!(terminal, RepairCall::Unrepairable);
}

#[test]
fn deadline_future_is_dropped_on_success_and_on_expiry() {
    // Success path: the owned deadline is released, not forgotten.
    let dropped = Rc::new(Cell::new(false));
    let out = drive(settle_repair_call(
        ready_fetch(
            200,
            vec![Ok(
                br#"{"status":"repaired","exit_code":0,"file_count":1}"#.to_vec()
            )],
        ),
        DropProbeDeadline {
            dropped: dropped.clone(),
            fire: false,
        },
    ));
    assert_eq!(out, RepairCall::Repaired);
    assert!(
        dropped.get(),
        "the deadline future must be dropped after a successful settlement"
    );
    // Expiry path: likewise.
    let dropped = Rc::new(Cell::new(false));
    let out = drive(settle_repair_call(
        hanging_fetch(),
        DropProbeDeadline {
            dropped: dropped.clone(),
            fire: true,
        },
    ));
    assert_eq!(out, RepairCall::Retry);
    assert!(
        dropped.get(),
        "the deadline future must be dropped after an expired settlement"
    );
}

// ---------------------------------------------------------------------------
// Production literal relations + wiring pins. `repair.rs` is read up
// to its unit-test module so this file's own literals cannot satisfy
// a pin; the sibling hop's budget is read from its runtime source.
// ---------------------------------------------------------------------------

fn repair_production_source() -> &'static str {
    let source = include_str!("../src/repair.rs");
    source
        .split_once("#[cfg(test)]")
        .map(|(head, _)| head)
        .unwrap_or(source)
}

fn const_value_ms(source: &str, name: &str) -> u64 {
    let needle = format!("const {name}: u64 = ");
    let after = source
        .split_once(&needle)
        .unwrap_or_else(|| panic!("{name} must be declared as `const {name}: u64 = <millis>;`"))
        .1;
    let literal: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .filter(|c| *c != '_')
        .collect();
    literal
        .parse()
        .unwrap_or_else(|_| panic!("{name} must be a plain millisecond literal, got {literal:?}"))
}

#[test]
fn deadline_constant_is_finite_and_outlasts_the_inner_hops() {
    let outer = REPAIR_CALL_DEADLINE_MS;
    // The Rust value and the source literal agree (no computed
    // placeholder elsewhere).
    assert_eq!(
        const_value_ms(repair_production_source(), "REPAIR_CALL_DEADLINE_MS"),
        outer
    );
    let runtime = include_str!("../../repair/src/runtime.rs");
    let container_repair_ms = const_value_ms(runtime, "CONTAINER_REPAIR_DEADLINE_MS");
    let do_ms = const_value_ms(runtime, "DO_REPAIR_DEADLINE_MS");
    assert!(
        outer > do_ms,
        "REPAIR_CALL_DEADLINE_MS ({outer}) must strictly exceed the repair front's \
         DO_REPAIR_DEADLINE_MS ({do_ms}) so the inner repair_unavailable can arrive"
    );
    assert!(
        outer > container_repair_ms,
        "REPAIR_CALL_DEADLINE_MS ({outer}) must exceed the Container's 150 s repair \
         deadline ({container_repair_ms}), which excludes staging and output relay"
    );
    assert_ne!(
        outer, 10_000,
        "the outer hop must not copy the 10 s pool/Container base budget"
    );
    assert!(
        outer < 3_600_000,
        "REPAIR_CALL_DEADLINE_MS ({outer}) must be a finite budget under one hour, not a placeholder"
    );
}

fn fn_slice<'a>(source: &'a str, start: &str, next: &str) -> &'a str {
    let after = source
        .split_once(start)
        .unwrap_or_else(|| panic!("{start} must exist in repair.rs"))
        .1;
    after
        .split_once(next)
        .unwrap_or_else(|| panic!("{next} must bound {start} in repair.rs"))
        .0
}

#[test]
fn production_call_arms_the_deadline_before_dispatch_and_settles_through_the_seam() {
    let source = repair_production_source();
    let default_entry = fn_slice(
        source,
        "pub async fn call_repair(",
        "pub async fn call_repair_with_deadline(",
    );
    assert!(
        default_entry.contains("call_repair_with_deadline(env, req, REPAIR_CALL_DEADLINE_MS)"),
        "call_repair must delegate with the named production default, not a second inline budget"
    );
    // The production source already ends at the unit-test module,
    // and call_repair_with_deadline is the last production function
    // (it is what call_repair delegates to), so its slice runs to
    // the end of the production part.
    let wired = source
        .split_once("pub async fn call_repair_with_deadline(")
        .expect("call_repair_with_deadline must exist in repair.rs")
        .1;
    let armed_at = wired
        .find("Delay::from")
        .expect("call_repair_with_deadline must arm a worker Delay deadline");
    let fetch_at = wired
        .find("fetch_request(")
        .expect("call_repair_with_deadline must still fetch through the service binding");
    assert!(
        armed_at < fetch_at,
        "the deadline must be armed before the fetch is issued so slow headers count against it"
    );
    assert!(
        wired.contains("Duration::from_millis(deadline_ms)"),
        "the timer must arm from the injected deadline_ms (millis, not secs, not an inline copy)"
    );
    assert!(
        wired.contains("settle_repair_call("),
        "the production call must settle through settle_repair_call (the seam proven above), not a second inline race"
    );
    assert!(
        !wired.contains("drain_repair_stream(stream, MAX_REPAIR_REPLY_BYTES).await"),
        "the bare unbounded body drain must not remain on the production path"
    );
}

#[test]
fn timers_are_not_leaked_forgotten_or_spawned_in_repair_rs() {
    let source = repair_production_source();
    for banned in ["mem::forget", "Box::leak", "spawn_local"] {
        assert!(
            !source.contains(banned),
            "repair.rs must not {banned}: the deadline is an owned local future dropped on every exit"
        );
    }
}
