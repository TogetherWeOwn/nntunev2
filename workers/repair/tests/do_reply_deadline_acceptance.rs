//! The outer repair-front -> Durable Object hop is bounded through
//! response completion (headers AND capped body drain, one budget).
//!
//! PROTECTED acceptance spec: the implementation is built against
//! this file; this file is never edited to match an implementation.
//!
//! Gap under test (`workers/repair/src/runtime.rs`, the front
//! Worker's `fetch`): the route awaits
//! `stub.fetch_with_request(request)` for the Durable Object reply
//! headers and then `read_response_capped(&mut response,
//! MAX_RESULT_BYTES)` for the body, with no deadline on either
//! await. A hung or dribbling object therefore pends the ingest
//! service-binding call indefinitely. The already-bounded Container
//! fetches inside the object are a different, inner hop.
//!
//! Contract the implementation must provide (exact public surface):
//!
//! ```rust,ignore
//! // in nntune_repair::contract (native-testable; runtime is wasm-only),
//! // next to push_chunk_capped / repair_unavailable_fault
//! pub async fn settle_do_reply<F, S, D>(
//!     fetch: F, deadline: D,
//! ) -> Result<(u16, Vec<u8>), ApiFault>
//! where
//!     F: std::future::Future<Output = Result<(u16, S), ()>> + Unpin,
//!     S: futures_util::Stream<Item = Result<Vec<u8>, ()>> + Unpin,
//!     D: std::future::Future<Output = ()> + Unpin;
//!
//! // in workers/repair/src/runtime.rs, with the other *_MS knobs
//! const DO_REPAIR_DEADLINE_MS: u64 = /* finite, documented */;
//! ```
//!
//! Semantics pinned below:
//!
//! - ONE total budget spans the object reply headers plus the
//!   capped body read. `settle_do_reply` races the fetch `F`, then
//!   the drain of `S`, against the SAME `deadline`, polling the
//!   deadline first (the codebase race shape: an already-fired
//!   deadline aborts even a ready fetch). Partial progress —
//!   headers landed, chunks collected — never re-arms the budget:
//!   a dribbling body cannot reset its deadline.
//! - The drain is the existing capped accumulation
//!   (`push_chunk_capped` against `MAX_RESULT_BYTES`,
//!   check-before-extend): exactly-at-cap is admitted, the chunk
//!   that would cross the cap aborts without the excess ever being
//!   retained, and no truncated prefix is returned as a reply.
//! - Expiry (either stage), a fetch transport error, a chunk-level
//!   transport error, and an over-cap body all settle as the
//!   EXISTING `repair_unavailable_fault()` (503
//!   `repair_unavailable`) — the same verdict the front route
//!   already returns for an unreadable object reply. No new fault
//!   code (`do_timeout`, `deadline_exceeded`, ...) may appear.
//! - A reply that completes inside the budget is relayed VERBATIM:
//!   `(status, bytes)` unchanged for every status (200 result
//!   documents, 404 `scratch_missing`, 400, ...). The front route
//!   never classifies the object's verdict; bounding the hop must
//!   not start doing so. After an expiry, a later legitimate call
//!   relays normally: expiry manufactures no terminal outcome and
//!   writes nothing (this seam is read-only; the object's
//!   idempotency and result-marker-last behavior are untouched).
//! - The deadline future is owned by the settlement and dropped on
//!   EVERY completion path: no leaked, forgotten, or spawned
//!   timers (`mem::forget`, `Box::leak`, `spawn_local` stay banned
//!   in `runtime.rs` and must not appear in the seam).
//! - Production wiring: the front `fetch` arms
//!   `Delay::from(Duration::from_millis(DO_REPAIR_DEADLINE_MS))`
//!   BEFORE `fetch_with_request(` is issued (slow headers count
//!   against the same total; the deadline-first race polls — and
//!   thereby registers — the timer from the start) and settles
//!   through `settle_do_reply`. Adapting a Workers `Response` to
//!   the seam's `(u16, stream)` fetch output happens inside that
//!   fetch future and preserves today's body handling: a bodyless
//!   reply reads as empty, an already-materialised reply is
//!   length-checked against `MAX_RESULT_BYTES` before any clone,
//!   a streamed reply yields `Vec<u8>` chunks with errors mapped
//!   to `()`.
//! - `DO_REPAIR_DEADLINE_MS` is finite and DOCUMENTED at its
//!   declaration against the child round it must outlast: one
//!   object run is manifest/result reads + health + input staging
//!   + the Container repair (`CONTAINER_REPAIR_DEADLINE_MS` =
//!     150 s, which covers only the synchronous par2 call, NOT
//!     staging) + output relay + cleanup, so the budget strictly
//!     exceeds 150_000 with documented headroom for those stages and
//!     the enclosing invocation; it is never the 10 s Container base
//!     and never an unbounded placeholder (< 1 h). The exact value
//!     is the implementer's documented choice; the relations are
//!     pinned. Ingest's outer budget outlasting this one is pinned
//!     in the ingest acceptance suite.
//!
//! What this does NOT prove: dropping a Rust future locally does
//! not prove the Durable Object or its Container cancelled the
//! underlying round, and no real object or Container is exercised.
//! A timed-out front reply may therefore race a round that still
//! completes and writes its result marker; the existing
//! plan/idempotency rules (replay on the same manifest digest)
//! are what make the later retry safe, and they are unchanged.
//!
//! Deterministic time: tests never sleep and need no async
//! runtime. Fakes are ready/pending futures, pending/chunked
//! streams, and poll-counted deadlines, driven by a bounded manual
//! poll loop, so "settles locally" is a poll-budget assertion.
//!
//! Why the seam lives in `contract`: `runtime` compiles only for
//! `wasm32`, so a native integration target cannot call it (same
//! constraint as `container_fetch_deadline`). The generic seam in
//! the pure contract module is natively executable, and the
//! wiring pins below prove the wasm route actually settles
//! through it.
//!
//! Run only this file:
//!
//! ```sh
//! python3 scripts/cargo_cache.py run -- test \
//!     --manifest-path workers/repair/Cargo.toml \
//!     --test do_reply_deadline_acceptance
//! ```

use std::cell::Cell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use nntune_repair::contract::{
    push_chunk_capped, repair_unavailable_fault, settle_do_reply, ApiFault, MAX_RESULT_BYTES,
};

// ---------------------------------------------------------------------------
// Deterministic driving: a bounded manual poll loop (settlement =
// Ready within the poll budget; a hang is a failure, not a flake).
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

/// Steady progress, `stall_polls` pending polls before each chunk:
/// only a TOTAL deadline (never re-armed by progress) aborts it.
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

const RESULT_BODY: &[u8] = br#"{"status":"repaired","exit_code":0,"file_count":3}"#;

// ---------------------------------------------------------------------------
// Controls: existing contract behavior the seam composes with.
// ---------------------------------------------------------------------------

#[test]
fn contract_controls_are_unchanged() {
    assert_eq!(
        repair_unavailable_fault(),
        ApiFault::new(503, "repair_unavailable"),
        "the front route's retryable verdict must stay 503 repair_unavailable"
    );
    assert_eq!(MAX_RESULT_BYTES, 1 << 20);
    // Check-before-extend, exactly-at-cap admissible.
    let mut body = vec![0u8; MAX_RESULT_BYTES - 3];
    assert!(push_chunk_capped(&mut body, &[1, 2, 3], MAX_RESULT_BYTES));
    assert_eq!(body.len(), MAX_RESULT_BYTES);
    assert!(!push_chunk_capped(&mut body, &[9], MAX_RESULT_BYTES));
    assert_eq!(
        body.len(),
        MAX_RESULT_BYTES,
        "excess must never be retained"
    );
}

// ---------------------------------------------------------------------------
// Fast paths through the seam: verbatim relay, unchanged.
// ---------------------------------------------------------------------------

#[test]
fn fast_reply_relays_status_and_bytes_verbatim() {
    let out = drive(settle_do_reply(
        ready_fetch(
            200,
            vec![
                Ok(RESULT_BODY[..20].to_vec()),
                Ok(Vec::new()),
                Ok(RESULT_BODY[20..].to_vec()),
            ],
        ),
        no_deadline(),
    ));
    assert_eq!(out, Ok((200, RESULT_BODY.to_vec())));
}

#[test]
fn non_200_replies_relay_verbatim_not_remapped() {
    // The front route relays the object's verdict; only an
    // unreadable/expired reply becomes repair_unavailable.
    for (status, body) in [
        (404u16, &br#"{"error":"scratch_missing"}"#[..]),
        (400, &br#"{"error":"invalid_request"}"#[..]),
        (409, &br#"{"error":"repair_manifest_conflict"}"#[..]),
        (503, &br#"{"error":"scratch_unavailable"}"#[..]),
    ] {
        let out = drive(settle_do_reply(
            ready_fetch(status, vec![Ok(body.to_vec())]),
            no_deadline(),
        ));
        assert_eq!(
            out,
            Ok((status, body.to_vec())),
            "status {status} must relay verbatim, not collapse to 503"
        );
    }
    // A bodyless reply reads as (status, empty), as today.
    let out = drive(settle_do_reply(ready_fetch(200, vec![]), no_deadline()));
    assert_eq!(out, Ok((200, Vec::new())));
    // A fetch-level transport error is the existing 503.
    let out = drive(settle_do_reply(
        std::future::ready(Err::<(u16, ChunkStream), ()>(())),
        no_deadline(),
    ));
    assert_eq!(out, Err(repair_unavailable_fault()));
}

#[test]
fn body_completing_before_the_deadline_succeeds() {
    let body = DribblingBody::new(
        3,
        vec![RESULT_BODY[..20].to_vec(), RESULT_BODY[20..].to_vec()],
    );
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((200, Box::pin(body))));
    let out = drive(settle_do_reply(fetch, FiresAfterPolls { remaining: 500 }));
    assert_eq!(out, Ok((200, RESULT_BODY.to_vec())));
}

#[test]
fn exact_cap_reply_is_admitted_through_the_seam() {
    let mut big = RESULT_BODY.to_vec();
    big.resize(MAX_RESULT_BYTES, b' ');
    let mid = big.len() / 2;
    let chunks = vec![Ok(big[..mid].to_vec()), Ok(big[mid..].to_vec())];
    let out = drive(settle_do_reply(ready_fetch(200, chunks), no_deadline()));
    assert_eq!(
        out.map(|(status, bytes)| (status, bytes.len())),
        Ok((200, MAX_RESULT_BYTES)),
        "a reply at exactly MAX_RESULT_BYTES across chunks must be admitted"
    );
}

#[test]
fn cap_plus_one_reply_is_repair_unavailable() {
    let mut big = RESULT_BODY.to_vec();
    big.resize(MAX_RESULT_BYTES + 1, b' ');
    let mid = big.len() / 2;
    let chunks = vec![Ok(big[..mid].to_vec()), Ok(big[mid..].to_vec())];
    let out = drive(settle_do_reply(ready_fetch(200, chunks), no_deadline()));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "the chunk crossing MAX_RESULT_BYTES must abort as repair_unavailable, excess never retained"
    );
}

#[test]
fn chunk_transport_error_after_a_complete_prefix_is_repair_unavailable() {
    let out = drive(settle_do_reply(
        ready_fetch(200, vec![Ok(RESULT_BODY.to_vec()), Err(())]),
        no_deadline(),
    ));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "a chunk error after a complete prefix must fail whole, never relay the prefix"
    );
}

// ---------------------------------------------------------------------------
// The deadline itself: settlement for stalled headers and stalled /
// dribbling bodies. RED on the unbounded route this suite was
// written against (no seam exists and nothing would return).
// ---------------------------------------------------------------------------

#[test]
fn stalled_headers_abort_as_repair_unavailable_at_the_deadline() {
    let out = drive(settle_do_reply(hanging_fetch(), expired_deadline()));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "object reply headers that never arrive must settle as 503 repair_unavailable at expiry"
    );
    let out = drive(settle_do_reply(
        hanging_fetch(),
        FiresAfterPolls { remaining: 2 },
    ));
    assert_eq!(out, Err(repair_unavailable_fault()));
}

#[test]
fn fired_deadline_wins_over_simultaneously_ready_headers() {
    let out = drive(settle_do_reply(
        ready_fetch(200, vec![Ok(RESULT_BODY.to_vec())]),
        expired_deadline(),
    ));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "deadline-first polling: an already-fired deadline aborts even a ready reply"
    );
}

#[test]
fn stalled_body_aborts_as_repair_unavailable_at_the_deadline() {
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(stream::pending()),
    )));
    let out = drive(settle_do_reply(fetch, expired_deadline()));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "a body that never yields must settle as repair_unavailable at expiry, not pend forever"
    );
}

#[test]
fn dribbling_body_cannot_reset_its_deadline() {
    // One complete-looking chunk lands, then the body hangs: the
    // prefix must not be relayed once the deadline fires.
    let body: ChunkStream =
        Box::pin(stream::iter(vec![Ok(RESULT_BODY.to_vec())]).chain(stream::pending()));
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((200, body)));
    let out = drive(settle_do_reply(fetch, FiresAfterPolls { remaining: 2 }));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "a trickle-then-hang body aborts whole at the SAME deadline"
    );

    // Steady dribble that would complete in ~40 polls, deadline
    // firing early: progress must not extend the budget.
    let chunks = vec![
        RESULT_BODY[..12].to_vec(),
        RESULT_BODY[12..24].to_vec(),
        RESULT_BODY[24..36].to_vec(),
        RESULT_BODY[36..].to_vec(),
    ];
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(DribblingBody::new(10, chunks.clone())),
    )));
    let out = drive(settle_do_reply(fetch, FiresAfterPolls { remaining: 3 }));
    assert_eq!(
        out,
        Err(repair_unavailable_fault()),
        "a dribbling body must not extend its own deadline by making progress"
    );
    // Control: the identical dribble inside the budget relays, so
    // only the deadline flipped the verdict.
    let fetch = std::future::ready(Ok::<(u16, ChunkStream), ()>((
        200,
        Box::pin(DribblingBody::new(10, chunks)),
    )));
    let out = drive(settle_do_reply(fetch, no_deadline()));
    assert_eq!(out, Ok((200, RESULT_BODY.to_vec())));
}

#[test]
fn expiry_then_a_legitimate_retry_succeeds() {
    let expired = drive(settle_do_reply(hanging_fetch(), expired_deadline()));
    assert_eq!(expired, Err(repair_unavailable_fault()));
    // A later legitimate call relays normally — including a
    // non-200 verdict — so expiry manufactured no terminal outcome.
    let retried = drive(settle_do_reply(
        ready_fetch(200, vec![Ok(RESULT_BODY.to_vec())]),
        no_deadline(),
    ));
    assert_eq!(retried, Ok((200, RESULT_BODY.to_vec())));
    let missing = drive(settle_do_reply(
        ready_fetch(404, vec![Ok(br#"{"error":"scratch_missing"}"#.to_vec())]),
        no_deadline(),
    ));
    assert_eq!(
        missing,
        Ok((404, br#"{"error":"scratch_missing"}"#.to_vec()))
    );
}

#[test]
fn deadline_future_is_dropped_on_success_and_on_expiry() {
    let dropped = Rc::new(Cell::new(false));
    let out = drive(settle_do_reply(
        ready_fetch(200, vec![Ok(RESULT_BODY.to_vec())]),
        DropProbeDeadline {
            dropped: dropped.clone(),
            fire: false,
        },
    ));
    assert_eq!(out, Ok((200, RESULT_BODY.to_vec())));
    assert!(
        dropped.get(),
        "the deadline future must be dropped after a successful settlement"
    );
    let dropped = Rc::new(Cell::new(false));
    let out = drive(settle_do_reply(
        hanging_fetch(),
        DropProbeDeadline {
            dropped: dropped.clone(),
            fire: true,
        },
    ));
    assert_eq!(out, Err(repair_unavailable_fault()));
    assert!(
        dropped.get(),
        "the deadline future must be dropped after an expired settlement"
    );
}

// ---------------------------------------------------------------------------
// Production literal relations, seam-composition pins, and wasm
// wiring pins (runtime.rs as text, per container_fetch_deadline).
// ---------------------------------------------------------------------------

const RUNTIME_RS: &str = include_str!("../src/runtime.rs");
const CONTRACT_RS: &str = include_str!("../src/contract.rs");

fn contract_production_source() -> &'static str {
    CONTRACT_RS
        .split_once("#[cfg(test)]")
        .map(|(head, _)| head)
        .unwrap_or(CONTRACT_RS)
}

fn fn_slice<'a>(source: &'a str, start: &str, next: &str) -> &'a str {
    let after = source
        .split_once(start)
        .unwrap_or_else(|| panic!("{start} must exist"))
        .1;
    after
        .split_once(next)
        .unwrap_or_else(|| panic!("{next} must bound {start}"))
        .0
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
fn do_deadline_constant_is_finite_and_outlasts_the_container_repair_budget() {
    let do_ms = const_value_ms(RUNTIME_RS, "DO_REPAIR_DEADLINE_MS");
    let container_repair_ms = const_value_ms(RUNTIME_RS, "CONTAINER_REPAIR_DEADLINE_MS");
    assert_eq!(
        container_repair_ms, 150_000,
        "pin premise: the Container repair deadline this hop must outlast is 150_000 ms"
    );
    assert!(
        do_ms > container_repair_ms,
        "DO_REPAIR_DEADLINE_MS ({do_ms}) must strictly exceed the Container's 150 s \
         repair budget: one object run also stages inputs and relays outputs, which \
         the 150 s does not include"
    );
    assert_ne!(
        do_ms, 10_000,
        "the front hop must not copy the 10 s Container base budget"
    );
    assert!(
        do_ms < 3_600_000,
        "DO_REPAIR_DEADLINE_MS ({do_ms}) must be a finite budget under one hour"
    );
}

#[test]
fn seam_composes_the_existing_capped_accumulator_and_fault() {
    let production = contract_production_source();
    // The seam's own body: from its declaration to the next
    // top-level pub fn (textual placement in the file is the
    // implementer's choice).
    let after = production
        .split_once("pub async fn settle_do_reply")
        .expect("contract.rs must declare pub async fn settle_do_reply")
        .1;
    let end = ["\npub fn ", "\npub async fn ", "\npub const "]
        .iter()
        .filter_map(|marker| after.find(marker))
        .min()
        .unwrap_or(after.len());
    let seam = &after[..end];
    assert!(
        seam.contains("push_chunk_capped("),
        "settle_do_reply's own body must call push_chunk_capped (check-before-extend), not a second accumulator"
    );
    assert!(
        seam.contains("MAX_RESULT_BYTES"),
        "settle_do_reply must bound its drain by MAX_RESULT_BYTES"
    );
    assert!(
        seam.contains("repair_unavailable_fault()"),
        "settle_do_reply must map expiry/transport/oversize to the existing repair_unavailable_fault()"
    );
    for banned in ["mem::forget", "Box::leak", "spawn_local"] {
        assert!(
            !production.contains(banned),
            "contract.rs must not {banned}: the deadline is an owned local future"
        );
    }
}

#[test]
fn front_route_arms_the_deadline_before_dispatch_and_settles_through_the_seam() {
    // The front route runs from its fetch handler to the
    // RepairContainer declaration below it in runtime.rs.
    let front = fn_slice(
        RUNTIME_RS,
        "async fn fetch(mut req: Request, env: Env, _ctx: Context)",
        "pub struct RepairContainer",
    );
    let armed_at = front
        .find("DO_REPAIR_DEADLINE_MS")
        .expect("the front fetch must arm its budget from DO_REPAIR_DEADLINE_MS");
    let dispatch_at = front
        .find("fetch_with_request(")
        .expect("the front fetch must still dispatch through the object stub");
    assert!(
        armed_at < dispatch_at,
        "the deadline must be armed before the object call is issued so slow headers count against it"
    );
    assert!(
        front.contains("Delay::from"),
        "the front fetch must arm a cancellable worker Delay (the house deadline shape)"
    );
    assert!(
        front.contains("Duration::from_millis(DO_REPAIR_DEADLINE_MS)"),
        "the timer must arm from the named constant in millis, not an inline copy"
    );
    assert!(
        front.contains("settle_do_reply("),
        "the front fetch must settle through settle_do_reply (the seam proven above), not a second inline race"
    );
    assert!(
        front.contains("MAX_RESULT_BYTES"),
        "the response adaptation must stay bounded by MAX_RESULT_BYTES (incl. the materialised-reply length check)"
    );
    assert!(
        !front.contains("read_response_capped(&mut response, MAX_RESULT_BYTES).await"),
        "the bare unbounded body read must not remain on the front route"
    );
}

#[test]
fn no_new_fault_code_and_no_leaked_timers_on_the_front_hop() {
    // (`"repair_timeout"` is an existing Container verdict code
    // relayed by leak_code elsewhere in the file; the front-slice
    // pin below keeps it out of the front route itself.)
    for code_name in [
        "\"do_timeout\"",
        "\"front_timeout\"",
        "\"deadline_exceeded\"",
        "\"do_deadline\"",
    ] {
        assert!(
            !RUNTIME_RS.contains(code_name),
            "runtime.rs must not introduce the new front-hop fault code {code_name}"
        );
    }
    let front = fn_slice(
        RUNTIME_RS,
        "async fn fetch(mut req: Request, env: Env, _ctx: Context)",
        "pub struct RepairContainer",
    );
    assert!(
        !front.contains("\"repair_timeout\""),
        "the front route's expiry verdict stays repair_unavailable, not repair_timeout"
    );
    for banned in ["mem::forget", "Box::leak", "spawn_local"] {
        assert!(
            !RUNTIME_RS.contains(banned),
            "runtime.rs must not {banned}: deadline futures are owned locals dropped on every exit"
        );
    }
}
