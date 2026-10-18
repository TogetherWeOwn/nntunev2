//! Play/track id 512-byte cap agreement pins (tests-only).
//!
//! Contract under test:
//!
//! - `workers/stream/src/meter.rs:180-184` enforces track/play id caps with
//!   bare `512` literals inside [`derive_play_idempotency_key`];
//! - `workers/api/src/ledger.rs:71,73` owns the named constants
//!   `MAX_TRACK_ID_LEN` / `MAX_PLAY_ID_LEN = 512`, and the ledger-side
//!   `derive_play_idempotency_key` enforces the same 128/512/512 byte limits.
//!
//! A stream-accepted track/play id must never be rejected at the ledger, and
//! vice versa, so the suite pins both the boundary behaviour (empty and
//! 513-byte ids rejected, exactly-512 accepted, for track AND play
//! independently) and the literal agreement
//! (`512 == MAX_TRACK_ID_LEN == MAX_PLAY_ID_LEN`, importing both crates).
//!
//! Sensitivity (what each pin kills; implementation stays untouched):
//!
//! - raising/lowering either stream-side literal (e.g. 256 or 1024) fails
//!   the boundary tests on that field — the ledger constants still say 512,
//!   so the drift surfaces here instead of silently unbilled plays;
//! - raising/lowering either ledger constant fails
//!   `ledger_caps_are_512_and_agree` even when the stream behaviour is
//!   unchanged, catching a ledger-side drift before it rejects
//!   stream-accepted ids;
//! - splitting the two caps apart (track 512 / play 256 or vice versa) fails
//!   the agreement assert, which requires both to stay 512 together;
//! - accepting empty ids fails `empty_*_rejected`; accepting 513-byte ids
//!   fails `overlong_*_rejected`; rejecting exactly-512 fails
//!   `exactly_512_*_accepted`.
//!
//! Out of scope: `probe.rs` message-id 512 (a different RFC concept),
//! api-side ledger pins (already covered there), any metering behaviour
//! change.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test -p nntune-stream --test id_cap_agreement_acceptance
//! ```

use nntune_api::ledger::{MAX_PLAY_ID_LEN, MAX_TRACK_ID_LEN};
use nntune_stream::meter::derive_play_idempotency_key;

const USER: &str = "user-1";

fn id_of_len(len: usize) -> String {
    "a".repeat(len)
}

#[test]
fn ledger_caps_are_512_and_agree() {
    assert_eq!(
        MAX_TRACK_ID_LEN, 512,
        "ledger MAX_TRACK_ID_LEN must stay exactly 512 bytes"
    );
    assert_eq!(
        MAX_PLAY_ID_LEN, 512,
        "ledger MAX_PLAY_ID_LEN must stay exactly 512 bytes"
    );
    assert_eq!(
        MAX_TRACK_ID_LEN, MAX_PLAY_ID_LEN,
        "ledger track/play caps must agree with each other and the stream 512"
    );
}

#[test]
fn empty_track_id_rejected() {
    assert!(
        derive_play_idempotency_key(USER, "", "play-1").is_err(),
        "empty track id must be rejected"
    );
}

#[test]
fn empty_play_id_rejected() {
    assert!(
        derive_play_idempotency_key(USER, "track-1", "").is_err(),
        "empty play id must be rejected"
    );
}

#[test]
fn overlong_513_byte_track_id_rejected() {
    assert!(
        derive_play_idempotency_key(USER, &id_of_len(513), "play-1").is_err(),
        "513-byte track id must be rejected at the 512-byte cap"
    );
}

#[test]
fn overlong_513_byte_play_id_rejected() {
    assert!(
        derive_play_idempotency_key(USER, "track-1", &id_of_len(513)).is_err(),
        "513-byte play id must be rejected at the 512-byte cap"
    );
}

#[test]
fn exactly_512_byte_track_and_play_ids_accepted() {
    // Each 512-byte field is accepted independently and together: a cap, not
    // a combined budget.
    for (track, play) in [
        (id_of_len(512), "play-1".to_string()),
        ("track-1".to_string(), id_of_len(512)),
        (id_of_len(512), id_of_len(512)),
    ] {
        assert!(
            derive_play_idempotency_key(USER, &track, &play).is_ok(),
            "exactly-512-byte ids (track {}B, play {}B) must be accepted",
            track.len(),
            play.len(),
        );
    }
}
