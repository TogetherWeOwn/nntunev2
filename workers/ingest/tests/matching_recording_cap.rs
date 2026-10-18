//! Recording-cap boundary pins: the cap applies after ordering and
//! deduplication, never before.
//!
//! Tests-only slice (TOG-29833). The matcher contract under test lives in
//! `workers/ingest/src/matching.rs`: [`parse_mb_recordings_response`]
//! filters invalid MBIDs, re-sorts by (medium, track, MBID) without trusting
//! wire order, deduplicates keeping the first sorted position, then caps at
//! [`MAX_RECORDING_MBIDS`] (10,000, mirroring
//! `library::validate_library_write`). These tests pin that order of
//! operations at the boundary with generated synthetic responses only — no
//! MB service, database, or provider connections.
//!
//! Synthetic ID design (independent of the matcher): `mbid_for(i)` is
//! `aaaaaaaa-1111-4111-8111-<i as 12-digit hex>`, so lexicographic MBID
//! order is numeric order. Medium/track assignment is
//! `medium = 1 + i / 5000`, `track = i % 5000 + 1`, so the
//! (medium, track, MBID) sort order is also numeric order: disc 1 holds
//! i = 0..5000, disc 2 holds i = 5000..10000, disc 3 starts at i = 10000.
//! The expected retained set is therefore `(0..k).map(mbid_for)` with no
//! sorting call — constructed without touching the matcher. A track-only
//! sort would interleave discs (disc 1 track 1, disc 2 track 1, ...), so
//! the reversed/shuffled wire orders below fail unless the matcher sorts
//! medium-major.

use nntune_ingest::dispatch::build_search_dispatch;
use nntune_ingest::library::validate_library_write;
use nntune_ingest::matching::{
    library_write_for, parse_mb_recordings_response, MAX_RECORDING_MBIDS,
};

/// Release MBID carried in the synthetic bodies (any valid MBID; the
/// recordings route echoes it and the parser ignores it).
const RELEASE_MBID: &str = "aaaaaaaa-1111-4111-8111-111111111111";

/// Distinct valid MBID for index `i`: fixed prefix, zero-padded hex
/// counter in the final group, so lexicographic order is numeric order.
fn mbid_for(i: u32) -> String {
    format!("aaaaaaaa-1111-4111-8111-{i:012x}")
}

/// Medium assignment: 5000 tracks per disc.
fn medium_for(i: u32) -> i16 {
    (1 + i / 5000) as i16
}

/// Track assignment within the disc (1-based).
fn track_for(i: u32) -> i16 {
    (i % 5000 + 1) as i16
}

/// One wire entry: (mbid, medium_no, track_no).
type WireEntry = (String, i16, i16);

/// The `n` unique synthetic entries in index order.
fn unique_entries(n: u32) -> Vec<WireEntry> {
    (0..n)
        .map(|i| (mbid_for(i), medium_for(i), track_for(i)))
        .collect()
}

/// Deterministic permutation of `0..n`: multiply by a prime coprime to
/// every tested `n` (9999 = 3^2·11·101, 10000 = 2^4·5^4,
/// 10001 = 73·137; 7919 divides none of them), mod `n`.
fn shuffled_order(n: usize) -> Vec<usize> {
    (0..n).map(|k| (k * 7919) % n).collect()
}

/// Render a recordings body from wire entries in the given wire order.
fn body_for(wire: &[WireEntry]) -> String {
    let mut out = String::from("{\"release_mbid\":\"");
    out.push_str(RELEASE_MBID);
    out.push_str("\",\"recordings\":[");
    for (pos, (mbid, medium, track)) in wire.iter().enumerate() {
        if pos > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"mbid\":\"{mbid}\",\"track_no\":{track},\"medium_no\":{medium}}}"
        ));
    }
    out.push_str("]}");
    out
}

/// Expected retained IDs for `n_unique` distinct synthetic IDs, built
/// without calling the matcher: numeric order is the (medium, track,
/// MBID) order by construction, capped at [`MAX_RECORDING_MBIDS`].
fn expected_for(n_unique: u32) -> Vec<String> {
    let kept = (n_unique as usize).min(MAX_RECORDING_MBIDS);
    (0..kept as u32).map(mbid_for).collect()
}

/// Invalid MBID rows: every shape [`validate_mbid`](nntune_ingest::queue::validate_mbid)
/// rejects — wrong text, empty, non-hex, missing hyphens, short group.
/// (Uppercase hex is *valid* and deliberately absent here.)
fn invalid_entries() -> Vec<WireEntry> {
    [
        "not-an-mbid",
        "",
        "zzzzzzzz-1111-4111-8111-111111111111",
        "aaaaaaaa11114111811111111111111",
        "aaaaaaaa-1111-4111-8111-11111111111",
        "too-short",
    ]
    .into_iter()
    .enumerate()
    .map(|(k, bad)| (bad.to_string(), 1, 9000 + k as i16))
    .collect()
}

/// Exact-clone duplicates of the given indices (same MBID *and* same
/// medium/track, so both copies sort adjacently and the second is
/// dropped at the same position without shifting the order).
fn duplicate_entries(indices: &[u32]) -> Vec<WireEntry> {
    indices
        .iter()
        .map(|&i| (mbid_for(i), medium_for(i), track_for(i)))
        .collect()
}

fn parse(body: &str) -> Vec<String> {
    parse_mb_recordings_response(200, body).expect("synthetic body must parse")
}

fn search_msg() -> nntune_ingest::queue::QueueMsg {
    build_search_dispatch("user_01", "animals", "vault_idx_01", &[3000], 25, 0)
        .expect("test dispatch must build")
}

#[test]
fn recording_cap_value_mirrors_library_validation() {
    assert_eq!(
        MAX_RECORDING_MBIDS, 10_000,
        "matcher cap and library validation must agree on 10k"
    );
}

#[test]
fn cap_9999_keeps_every_unique_id_in_order() {
    // 9,999 unique IDs plus duplicates/invalids around them: everything
    // valid survives, in medium/track/MBID order, at exact length.
    let mut wire = unique_entries(9999);
    wire.reverse();
    wire.splice(0..0, invalid_entries());
    wire.extend(duplicate_entries(&[0, 1, 4998]));
    wire.extend(invalid_entries());
    let got = parse(&body_for(&wire));
    assert_eq!(got, expected_for(9999));
    assert_eq!(got.len(), 9999);
}

#[test]
fn cap_10000_keeps_every_unique_id_in_order() {
    // Exactly at the cap: nothing is dropped, duplicates/invalids add no
    // quota consumption.
    let mut wire = unique_entries(10_000);
    wire.reverse();
    wire.splice(0..0, invalid_entries());
    wire.insert(
        wire.len() / 2,
        duplicate_entries(&[4999]).into_iter().next().unwrap(),
    );
    wire.extend(duplicate_entries(&[0, 9999]));
    wire.extend(invalid_entries());
    let got = parse(&body_for(&wire));
    assert_eq!(got, expected_for(10_000));
    assert_eq!(got.len(), 10_000);
}

#[test]
fn cap_10001_truncates_to_first_10000_in_order() {
    // One over the cap: the lexicographically largest ID (i = 10000,
    // disc 3 track 1) is dropped; the retained set is exactly the first
    // 10,000 in medium/track/MBID order.
    let mut wire = unique_entries(10_001);
    wire.reverse();
    // Duplicates/invalids before and straddling the sorted boundary must
    // not consume unique-valid quota: clones of the last retained ID
    // (i = 9999) and the dropped ID (i = 10000).
    wire.splice(0..0, invalid_entries());
    wire.extend(duplicate_entries(&[0, 9999, 10_000]));
    wire.extend(invalid_entries());
    let got = parse(&body_for(&wire));
    let expected = expected_for(10_001);
    assert_eq!(expected.len(), 10_000);
    assert_eq!(got, expected);
    assert_eq!(got.last(), Some(&mbid_for(9999)));
    assert!(
        !got.contains(&mbid_for(10_000)),
        "the 10,001st sorted ID must be the one dropped"
    );
}

#[test]
fn duplicates_and_invalids_never_consume_quota() {
    // Heavy injection: 10,001 unique IDs buried in duplicates of early,
    // middle, boundary (9999) and dropped (10000) IDs plus invalid rows
    // at the start, middle and end of the wire. The retained set is still
    // exactly the first 10,000 unique IDs — not only the same size.
    let order = shuffled_order(10_001);
    let unique = unique_entries(10_001);
    let mut wire: Vec<WireEntry> = order.iter().map(|&k| unique[k].clone()).collect();
    let boundary_dups = duplicate_entries(&[0, 1, 2500, 4999, 5000, 7500, 9998, 9999, 10_000]);
    wire.splice(0..0, invalid_entries());
    wire.splice(wire.len() / 2..wire.len() / 2, boundary_dups);
    wire.extend(invalid_entries());
    wire.extend(duplicate_entries(&[0, 9999]));
    let got = parse(&body_for(&wire));
    assert_eq!(got, expected_for(10_001));
    assert_eq!(got.len(), MAX_RECORDING_MBIDS);
}

#[test]
fn shuffled_wire_order_yields_same_capped_set() {
    // Forward, reversed and deterministically shuffled wire orders of the
    // same 10,001 unique IDs converge on the identical capped set: the
    // matcher sorts medium-major rather than trusting arrival order.
    let unique = unique_entries(10_001);
    let mut reversed = unique.clone();
    reversed.reverse();
    let shuffled: Vec<WireEntry> = shuffled_order(unique.len())
        .iter()
        .map(|&k| unique[k].clone())
        .collect();
    let expected = expected_for(10_001);
    assert_eq!(parse(&body_for(&unique)), expected);
    assert_eq!(parse(&body_for(&reversed)), expected);
    assert_eq!(parse(&body_for(&shuffled)), expected);
}

#[test]
fn mbid_lexicographic_breaks_same_track_ties() {
    // Three IDs sharing one (medium, track): wire arrives in reverse
    // lexicographic order and must still come out lexicographic.
    let mut ids = vec![mbid_for(7), mbid_for(70), mbid_for(700)];
    ids.sort();
    let wire: Vec<WireEntry> = ids.iter().rev().map(|m| (m.clone(), 2, 42)).collect();
    assert_eq!(parse(&body_for(&wire)), ids);
}

#[test]
fn capped_output_feeds_library_validation() {
    // The capped matcher output is accepted end to end: carried into a
    // library write and through validation. One more ID (the dropped
    // 10,001st) tips the write over the validation cap, proving the
    // matcher cap is what keeps oversized listings writable.
    let mut wire = unique_entries(10_001);
    wire.reverse();
    let capped = parse(&body_for(&wire));
    assert_eq!(capped.len(), MAX_RECORDING_MBIDS);
    let msg = search_msg();
    let write = library_write_for(&msg, Some(RELEASE_MBID.to_string()), capped.clone());
    assert_eq!(write.release_mbid.as_deref(), Some(RELEASE_MBID));
    assert_eq!(write.recording_mbids, expected_for(10_001));
    assert!(
        validate_library_write(&write).is_ok(),
        "capped output must validate"
    );
    let mut over = capped;
    over.push(mbid_for(10_000));
    let rejected = library_write_for(&msg, Some(RELEASE_MBID.to_string()), over);
    assert_eq!(
        validate_library_write(&rejected),
        Err("too many recording mbids")
    );
}
