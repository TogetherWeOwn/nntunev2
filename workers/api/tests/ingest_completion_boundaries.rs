#![cfg(not(any(target_arch = "wasm32", feature = "postgres-tests")))]

use std::time::{Duration, UNIX_EPOCH};

use nntune_core::media_envelope::EnvelopeContext;
use nntune_ingest::multipart::session_outcome::CommittedOutcome;

// Compile the production checks without the database caller feature.
#[allow(dead_code)]
#[path = "../src/ingest_session_caller/flac.rs"]
mod flac;

#[test]
fn replay_requires_the_prepared_completion_identities() {
    let mut context = EnvelopeContext {
        identities: [[0; 16], [1; 16], [2; 16], [3; 16], [4; 16], [5; 16]],
        plaintext_len: 1,
        header: [0; 48],
    };
    let outcome = CommittedOutcome {
        map: "06060606-0606-0606-0606-060606060606".into(),
        object: "03030303-0303-0303-0303-030303030303".into(),
        generation: "04040404-0404-0404-0404-040404040404".into(),
        configuration: "05050505-0505-0505-0505-050505050505".into(),
        root_version: "00000000-0000-0000-0000-000000000000".into(),
        media_version: "synthetic-media".into(),
        page_versions: vec![],
    };
    assert!(flac::matches_outcome(&context, [6; 16], &outcome));
    assert!(!flac::matches_outcome(&context, [7; 16], &outcome));
    for index in [0, 3, 4, 5] {
        context.identities[index][0] ^= 1;
        assert!(!flac::matches_outcome(&context, [6; 16], &outcome));
        context.identities[index][0] ^= 1;
    }
}

#[test]
fn readback_deadline_is_the_earlier_live_bound() {
    let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
    assert_eq!(flac::readback_deadline(at(10), 12, at(15)), Some(at(12)));
    assert_eq!(flac::readback_deadline(at(10), 15, at(12)), Some(at(12)));
    assert_eq!(flac::readback_deadline(at(12), 12, at(15)), None);
    assert_eq!(flac::readback_deadline(at(12), 15, at(12)), None);
}
