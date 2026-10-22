//! Acceptance: `validate_library_write` distinctness and attribution.
//!
//! Protected acceptance suite — the Builder implements against this file
//! and must not edit it; a suspect test comes back to the test author.
//!
//! Contract under test (from the card spec, synthetic values only, no DB):
//! - a `LibraryWrite` whose `recording_mbids` contains the same recording
//!   MBID more than once is rejected — including 10,000 copies of one
//!   MBID, which the count cap and per-item shape checks alone accept;
//! - a `LibraryWrite` with a non-empty `recording_mbids` and
//!   `release_mbid: None` (orphan recordings, attributable to no release)
//!   is rejected;
//! - empty recording lists (with or without a release) and valid,
//!   distinct, release-attributed writes are still accepted;
//! - every pre-existing shape/count/key rejection is unchanged, message
//!   included.
//!
//! New-rejection cases are otherwise fully valid, so the new rule is the
//! sole cause of rejection, and they assert only `is_err()` — the exact
//! wording of the new error strings is the Builder's choice. Existing
//! rejections pin their current exact messages. Cases that violate two
//! rules at once never pin which rule fires first.

use nntune_ingest::library::validate_library_write;
use nntune_ingest::queue::LibraryWrite;

const RELEASE_MBID: &str = "f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f";

/// Distinct valid MBID for index `i`: fixed prefix, zero-padded hex
/// counter in the final group. Every value is a canonical 8-4-4-4-12 hex
/// MBID, so shape checks alone accept any list built from these.
fn mbid_for(i: u32) -> String {
    format!("aaaaaaaa-1111-4111-8111-{i:012x}")
}

/// `n` pairwise-distinct valid recording MBIDs, in index order.
fn distinct_recordings(n: u32) -> Vec<String> {
    (0..n).map(mbid_for).collect()
}

/// A fully valid attributed write carrying the given recordings. Every
/// other field satisfies the pre-existing checks, so mutating one field
/// in a test isolates exactly one rejection cause.
fn attributed_write(recording_mbids: Vec<String>) -> LibraryWrite {
    LibraryWrite {
        job_id: "job_0123456789abcdef".into(),
        user_id: "user_01".into(),
        idempotency_key: format!("ingest:user_01:{}:upload", "a".repeat(64)),
        release_mbid: Some(RELEASE_MBID.into()),
        recording_mbids,
    }
}

/// The same valid write with no release: the unmatched/orphan shape.
fn orphan_write(recording_mbids: Vec<String>) -> LibraryWrite {
    let mut write = attributed_write(recording_mbids);
    write.release_mbid = None;
    write
}

// ---------------------------------------------------------------------------
// Duplicate recording MBIDs are rejected (RED on the pre-fix validator)
// ---------------------------------------------------------------------------

#[test]
fn duplicate_adjacent_recordings_rejected() {
    let write = attributed_write(vec![mbid_for(1), mbid_for(1)]);
    assert!(
        validate_library_write(&write).is_err(),
        "the same recording MBID twice must not validate"
    );
}

#[test]
fn duplicate_non_adjacent_recordings_rejected() {
    // The repeat is separated by other recordings: an adjacent-only scan
    // over the list as given must not be enough to pass this suite.
    let write = attributed_write(vec![mbid_for(1), mbid_for(2), mbid_for(3), mbid_for(1)]);
    assert!(validate_library_write(&write).is_err());
}

#[test]
fn duplicate_of_last_recording_rejected() {
    let mut recordings = distinct_recordings(100);
    recordings.push(mbid_for(99));
    let write = attributed_write(recordings);
    assert!(validate_library_write(&write).is_err());
}

#[test]
fn duplicate_buried_in_many_distinct_recordings_rejected() {
    let mut recordings = distinct_recordings(500);
    recordings.insert(250, mbid_for(7));
    let write = attributed_write(recordings);
    assert!(validate_library_write(&write).is_err());
}

#[test]
fn triple_recording_rejected() {
    let write = attributed_write(vec![mbid_for(9), mbid_for(9), mbid_for(9)]);
    assert!(validate_library_write(&write).is_err());
}

#[test]
fn ten_thousand_copies_of_one_recording_rejected() {
    // The headline failure scenario: exactly at the 10k count cap and
    // every element individually shape-valid, so only distinctness can
    // reject this write.
    let write = attributed_write(vec![mbid_for(42); 10_000]);
    assert_eq!(write.recording_mbids.len(), 10_000);
    assert!(
        validate_library_write(&write).is_err(),
        "10k copies of one recording must not validate as 10k tracks"
    );
}

#[test]
fn duplicate_at_exact_cap_length_rejected() {
    // 9,999 distinct recordings plus one repeat of the first: length is
    // exactly the cap, so this is a distinctness rejection, not a count
    // rejection.
    let mut recordings = distinct_recordings(9_999);
    recordings.push(mbid_for(0));
    assert_eq!(recordings.len(), 10_000);
    let write = attributed_write(recordings);
    assert!(validate_library_write(&write).is_err());
}

// ---------------------------------------------------------------------------
// Orphan recordings (non-empty list, no release) are rejected (RED)
// ---------------------------------------------------------------------------

#[test]
fn single_orphan_recording_rejected() {
    let write = orphan_write(vec![mbid_for(1)]);
    assert!(
        validate_library_write(&write).is_err(),
        "a recording with no release to attribute it to must not validate"
    );
}

#[test]
fn multiple_distinct_orphan_recordings_rejected() {
    let write = orphan_write(distinct_recordings(3));
    assert!(validate_library_write(&write).is_err());
}

#[test]
fn orphan_recordings_at_exact_cap_length_rejected() {
    // 10,000 distinct recordings and no release: length is exactly the
    // cap, so only the attribution rule can reject this write.
    let write = orphan_write(distinct_recordings(10_000));
    assert_eq!(write.recording_mbids.len(), 10_000);
    assert!(validate_library_write(&write).is_err());
}

// ---------------------------------------------------------------------------
// Still accepted: empty lists and valid attributed writes
// ---------------------------------------------------------------------------

#[test]
fn empty_recordings_without_release_accepted() {
    // The unmatched write: no release, no recordings.
    let write = orphan_write(vec![]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn empty_recordings_with_release_accepted() {
    let write = attributed_write(vec![]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn single_attributed_recording_accepted() {
    let write = attributed_write(vec![mbid_for(1)]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn distinct_attributed_recordings_accepted_in_any_order() {
    // Distinctness must not become an ordering requirement: an unsorted
    // distinct list is a valid write.
    let write = attributed_write(vec![mbid_for(3), mbid_for(1), mbid_for(2)]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn distinct_recordings_at_exact_cap_accepted() {
    let write = attributed_write(distinct_recordings(10_000));
    assert_eq!(write.recording_mbids.len(), 10_000);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn distinct_recordings_just_under_cap_accepted() {
    let write = attributed_write(distinct_recordings(9_999));
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn recording_matching_release_mbid_accepted() {
    // Distinctness is within the recording list; a recording MBID equal
    // to the release MBID is pre-existing accepted behaviour.
    let write = attributed_write(vec![RELEASE_MBID.into()]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn uppercase_recording_mbid_still_accepted() {
    // Uppercase hex is shape-valid today; distinctness work must not
    // tighten the per-item shape check.
    let write = attributed_write(vec!["AAAAAAAA-1111-4111-8111-000000000001".into()]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

#[test]
fn near_identical_distinct_recordings_accepted() {
    // Differing only in the final hex digit: distinct, not duplicates.
    let write = attributed_write(vec![
        "aaaaaaaa-1111-4111-8111-000000000001".into(),
        "aaaaaaaa-1111-4111-8111-000000000002".into(),
    ]);
    assert_eq!(validate_library_write(&write), Ok(()));
}

// ---------------------------------------------------------------------------
// Pre-existing rejections unchanged (exact messages)
// ---------------------------------------------------------------------------

#[test]
fn existing_job_id_rejection_unchanged() {
    for bad in ["", "j".repeat(65).as_str()] {
        let mut write = attributed_write(vec![mbid_for(1)]);
        write.job_id = bad.into();
        assert_eq!(
            validate_library_write(&write),
            Err("job id must be 1..=64 url-safe chars"),
            "job_id {bad:?}"
        );
    }
}

#[test]
fn existing_user_id_rejection_unchanged() {
    for bad in ["", "u".repeat(129).as_str()] {
        let mut write = attributed_write(vec![mbid_for(1)]);
        write.user_id = bad.into();
        assert_eq!(
            validate_library_write(&write),
            Err("user id must be 1..=128 chars")
        );
    }
}

#[test]
fn existing_idempotency_key_rejection_unchanged() {
    for bad in ["", "not-our-key", "ingest:only-one-separator"] {
        let mut write = attributed_write(vec![mbid_for(1)]);
        write.idempotency_key = bad.into();
        assert_eq!(
            validate_library_write(&write),
            Err("idempotency key must be an ingest key"),
            "idempotency_key {bad:?}"
        );
    }
}

#[test]
fn existing_release_mbid_shape_rejection_unchanged() {
    let mut write = attributed_write(vec![mbid_for(1)]);
    write.release_mbid = Some("not-an-mbid".into());
    assert_eq!(
        validate_library_write(&write),
        Err("release mbid must be canonical 8-4-4-4-12 hex")
    );
}

#[test]
fn malformed_release_mbid_rejected_even_with_no_recordings() {
    // The attribution rule must not bypass the release shape check: a
    // present-but-malformed release MBID is rejected on shape.
    let mut write = attributed_write(vec![]);
    write.release_mbid = Some("not-an-mbid".into());
    assert_eq!(
        validate_library_write(&write),
        Err("release mbid must be canonical 8-4-4-4-12 hex")
    );
}

#[test]
fn existing_recording_shape_rejection_unchanged() {
    for bad in [
        "zzz",
        "",
        "not-an-mbid",
        "zzzzzzzz-1111-4111-8111-111111111111",
        "aaaaaaaa111141118111111111111111",
        "aaaaaaaa-1111-4111-8111-11111111111",
    ] {
        let write = attributed_write(vec![bad.into()]);
        assert_eq!(
            validate_library_write(&write),
            Err("recording mbids must be canonical 8-4-4-4-12 hex"),
            "recording {bad:?}"
        );
    }
}

#[test]
fn existing_count_cap_rejection_unchanged() {
    // 10,001 pairwise-distinct, attributed, shape-valid recordings: the
    // count cap is the sole rejection cause.
    let write = attributed_write(distinct_recordings(10_001));
    assert_eq!(write.recording_mbids.len(), 10_001);
    assert_eq!(
        validate_library_write(&write),
        Err("too many recording mbids")
    );
}
