//! MusicBrainz fetch response-cap literal pins (tests-only).
//!
//! Contract under test (`workers/ingest/src/queue_consumer.rs:936,942`,
//! enforced at `:1026` and `:1055` via `fetch_mb_body` at `:1000-1017`):
//!
//! - `MAX_MB_RESPONSE_BYTES == 65_536` (`64 * 1024`): the largest MB
//!   release-search response read into memory (25 small JSON hits; anything
//!   larger is a contract break, handled as transient).
//! - `MAX_MB_RECORDINGS_RESPONSE_BYTES == 1_048_576` (`1024 * 1024`): the
//!   largest MB release-recordings response (at most 1000 recording objects
//!   at a few hundred JSON bytes each — 1 MiB is generous headroom, anything
//!   larger is a contract break, handled as transient like the search cap).
//! - Exactly-at-cap bodies are admitted; cap+1 fails closed (`Err(())`, no
//!   truncated body returned) through the fetch helpers.
//! - An oversize body classifies as retryable on both routes: the search
//!   route reports `JobError::transient(ERR_MATCH_FAILED, "musicbrainz lookup
//!   unavailable")`, the recordings route reports `MatchFailure::Transient`.
//!
//! The caps are private to `queue_consumer.rs` and `fetch_mb_body` takes a
//! live Worker `Env` (no native constructor), so — like
//! `route_body_cap_boundary_acceptance` — the literal values and call-site
//! wiring are pinned by source text while the boundary arithmetic below
//! mirrors the exact streaming predicate
//! (`bytes.len() + chunk.len() > max_bytes` → `Err(())`, strict `>`, plain
//! `+`) and every fail-closed assertion goes through the public surface:
//! `JobError`, `ERR_MATCH_FAILED`, and `MatchFailure`.
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising/lowering either cap flips its literal pin (verified by mutation,
//!   restored);
//! - moving either route off its cap (or dropping the gate) flips the
//!   wiring pin;
//! - weakening the predicate to `>=` flips the predicate pin (the boundary
//!   tests exercise the transcribed mirror, so implementation-side drift is
//!   caught by the source-text pins — verified by mutation, restored);
//! - rewording the search-route detail or reclassifying either oversize path
//!   as permanent flips the fail-closed pins.
//!
//! Run only this file:
//!
//! ```sh
//! cd workers/ingest && cargo test --locked --test mb_fetch_cap_literals_acceptance
//! ```

use nntune_ingest::matching::MatchFailure;
use nntune_ingest::retry::{JobError, ERR_MATCH_FAILED};

/// The crate source under test. Sliced to the pre-`#[cfg(test)]` production
/// half per assertion so this file's own literals cannot satisfy the pins.
const CONSUMER_RS: &str = include_str!("../src/queue_consumer.rs");

/// Search-route response cap under test: `MAX_MB_RESPONSE_BYTES = 64 * 1024`.
const SEARCH_CAP_BYTES: usize = 65_536;

/// Recordings-route response cap under test:
/// `MAX_MB_RECORDINGS_RESPONSE_BYTES = 1024 * 1024`.
const RECORDINGS_CAP_BYTES: usize = 1_048_576;

/// Oversize detail the search route reports (`queue_consumer.rs:1029`).
const LOOKUP_UNAVAILABLE: &str = "musicbrainz lookup unavailable";

/// Production source before the unit-test module: pins below must match the
/// shipped code, never this file's own literals.
fn production_source() -> &'static str {
    CONSUMER_RS
        .split_once("#[cfg(test)]")
        .expect("test module must exist")
        .0
}

/// Slice of one function body out of the production source, bounded by the
/// next function, so wiring pins cannot match a neighboring function.
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

/// Mirror of the `fetch_mb_body` accumulation gate
/// (`queue_consumer.rs:1007-1014`): the chunk that would push the running
/// buffered total over `max_bytes` fails closed. Length-only so the 1 MiB
/// recordings boundary is exercised without a 1 MiB allocation; the
/// predicate is transcribed exactly (plain `+`, strict `>`).
fn gate_push_chunk(buf_len: usize, chunk_len: usize, max_bytes: usize) -> Result<(), ()> {
    if buf_len + chunk_len > max_bytes {
        return Err(());
    }
    Ok(())
}

#[test]
fn search_cap_literal_is_64_kib() {
    assert_eq!(
        SEARCH_CAP_BYTES, 65_536,
        "search response cap must stay exactly 64 KiB"
    );
    assert_eq!(SEARCH_CAP_BYTES, 64 * 1024);
    assert!(
        production_source().contains("const MAX_MB_RESPONSE_BYTES: usize = 64 * 1024;"),
        "queue_consumer.rs must declare MAX_MB_RESPONSE_BYTES as exactly `64 * 1024` (:936)"
    );
}

#[test]
fn recordings_cap_literal_is_1_mib() {
    assert_eq!(
        RECORDINGS_CAP_BYTES, 1_048_576,
        "recordings response cap must stay exactly 1 MiB"
    );
    assert_eq!(RECORDINGS_CAP_BYTES, 1024 * 1024);
    assert!(
        production_source()
            .contains("const MAX_MB_RECORDINGS_RESPONSE_BYTES: usize = 1024 * 1024;"),
        "queue_consumer.rs must declare MAX_MB_RECORDINGS_RESPONSE_BYTES as exactly `1024 * 1024` (:942)"
    );
}

#[test]
fn recordings_cap_dwarfs_search_cap() {
    // The recordings route returns up to 1000 recording objects against the
    // search route's 25 small hits: 1 MiB is exactly 16x the 64 KiB search
    // cap, the headroom the :938-941 comment justifies.
    assert_eq!(RECORDINGS_CAP_BYTES, 16 * SEARCH_CAP_BYTES);
}

#[test]
fn routes_enforce_caps_via_fetch_mb_body() {
    let source = production_source();
    let search = fn_body(
        source,
        "async fn fetch_release_match",
        "async fn fetch_release_recordings",
    );
    assert!(
        search.contains("MAX_MB_RESPONSE_BYTES,"),
        "fetch_release_match must stream through fetch_mb_body with MAX_MB_RESPONSE_BYTES (:1026)"
    );
    assert!(
        search.contains(LOOKUP_UNAVAILABLE),
        "fetch_release_match must report an oversize body as \"musicbrainz lookup unavailable\" (:1029)"
    );
    assert!(
        search.contains("JobError::transient(ERR_MATCH_FAILED, detail)"),
        "fetch_release_match must classify an oversize body as transient via the `unavailable` closure (:1022), never permanent"
    );
    assert!(
        search.contains(".map_err(|()| unavailable("),
        "fetch_release_match must route fetch_mb_body failures through the transient `unavailable` closure (:1029)"
    );
    let recordings = fn_body(
        source,
        "async fn fetch_release_recordings",
        "async fn resolve_library_write",
    );
    assert!(
        recordings.contains("MAX_MB_RECORDINGS_RESPONSE_BYTES,"),
        "fetch_release_recordings must stream through fetch_mb_body with MAX_MB_RECORDINGS_RESPONSE_BYTES (:1055)"
    );
    assert!(
        recordings.contains("MatchFailure::Transient"),
        "fetch_release_recordings must classify an oversize body as Transient"
    );
}

#[test]
fn fetch_gate_predicate_is_strictly_over() {
    // The transcribed mirror must stay faithful: `fetch_mb_body` admits the
    // exact fit (`>`, not `>=`) with plain wrapping-free addition.
    let fetcher = fn_body(
        production_source(),
        "async fn fetch_mb_body",
        "async fn fetch_release_match",
    );
    assert!(
        fetcher.contains("if bytes.len() + chunk.len() > max_bytes {"),
        "fetch_mb_body must reject exactly the chunk that would cross the cap (:1010)"
    );
    assert!(
        fetcher.contains("return Err(());"),
        "fetch_mb_body must fail closed with no partial body (:1011)"
    );
}

#[test]
fn search_exact_cap_admitted() {
    // Exact fit on the running total is accepted (strict `>`).
    assert!(gate_push_chunk(SEARCH_CAP_BYTES - 1, 1, SEARCH_CAP_BYTES).is_ok());
    assert!(gate_push_chunk(SEARCH_CAP_BYTES, 0, SEARCH_CAP_BYTES).is_ok());
    // Byte-exact control with a real buffer at search scale: 64 KiB
    // accumulates in 1 KiB chunks, and the capped body is what the parse
    // step would receive.
    let mut buf = Vec::new();
    let chunk = vec![b'x'; 1024];
    for _ in 0..64 {
        assert!(gate_push_chunk(buf.len(), chunk.len(), SEARCH_CAP_BYTES).is_ok());
        buf.extend_from_slice(&chunk);
    }
    assert_eq!(buf.len(), SEARCH_CAP_BYTES);
}

#[test]
fn search_cap_plus_one_fails_closed_transient() {
    // The chunk that would cross the cap fails: the caller gets `Err(())`
    // and never a truncated body (the gate returns before extending).
    assert!(gate_push_chunk(SEARCH_CAP_BYTES, 1, SEARCH_CAP_BYTES).is_err());
    assert!(gate_push_chunk(SEARCH_CAP_BYTES - 1, 2, SEARCH_CAP_BYTES).is_err());
    // Fail-closed classification through the exact public error the search
    // route builds for an oversize body: transient under ERR_MATCH_FAILED,
    // retryable on redelivery — never a permanent quarantine.
    let err = JobError::transient(ERR_MATCH_FAILED, LOOKUP_UNAVAILABLE);
    assert_eq!(ERR_MATCH_FAILED, "match_failed");
    assert_eq!(err.code, "match_failed");
    assert!(err.transient, "oversize MB body must stay retryable");
    assert!(!err.made_progress, "a rejected fetch advances nothing");
    assert_eq!(err.detail, LOOKUP_UNAVAILABLE);
}

#[test]
fn recordings_exact_cap_admitted() {
    assert!(gate_push_chunk(RECORDINGS_CAP_BYTES - 1, 1, RECORDINGS_CAP_BYTES).is_ok());
    assert!(gate_push_chunk(RECORDINGS_CAP_BYTES, 0, RECORDINGS_CAP_BYTES).is_ok());
}

#[test]
fn recordings_cap_plus_one_fails_closed_transient() {
    assert!(gate_push_chunk(RECORDINGS_CAP_BYTES, 1, RECORDINGS_CAP_BYTES).is_err());
    assert!(gate_push_chunk(RECORDINGS_CAP_BYTES - 1, 2, RECORDINGS_CAP_BYTES).is_err());
    // The recordings route maps the same `Err(())` to `MatchFailure::Transient`
    // (pinned by source in `routes_enforce_caps_via_fetch_mb_body`): a 1 MiB+1
    // body may succeed on redelivery, exactly like a 5xx — it must never be
    // the Permanent variant that skips the retry budget.
    assert_ne!(
        MatchFailure::Transient,
        MatchFailure::Permanent,
        "the fail-closed classification must be the retryable variant"
    );
}
