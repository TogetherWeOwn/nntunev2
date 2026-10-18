//! Behavioral agreement between CompletePlay and the actual API ledger.
//!
//! The API crate is workspace-excluded and absent from container build
//! contexts, so a stream path dev-dependency would break container builds.
//! Run a native, test-only Cargo harness in scratch instead. Both production
//! crates are normal path dependencies there; no ledger logic is copied,
//! no database/provider is contacted, and no tracked manifest is changed.
//!
//! Sensitivity: accepting punctuation/Unicode in any identity field fails
//! derivation and CompletePlay assertions. Rewriting identities, rejecting
//! valid ASCII, changing byte caps, or changing billing/replay behavior fails
//! positive controls and the actual CompletePlay -> prepare_ledger_row path.

use std::fs;
use std::process::Command;

#[test]
fn play_ledger_alphabet_contract() {
    let stream = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let api = stream.join("../api").canonicalize().expect("API checkout");
    let scratch = std::env::var_os("PAPERCLIP_RUN_SCRATCH_DIR")
        .or_else(|| std::env::var_os("PAPERCLIP_SCRATCH_DIR"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("play-ledger-alphabet-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("create native contract harness");
    let manifest = format!(
        "[package]\nname = \"play-ledger-alphabet-contract\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
         [workspace]\n\
         [dependencies]\nnntune-stream = {{ path = {stream:?} }}\nnntune-api = {{ path = {api:?} }}\n\
         [[test]]\nname = \"alphabet\"\npath = \"alphabet.rs\"\n"
    );
    fs::write(scratch.join("Cargo.toml"), manifest).expect("write test-only manifest");
    fs::write(scratch.join("alphabet.rs"), CONTRACT_TESTS).expect("write contract tests");
    // Seed the API's resolved versions while Cargo adds stream-only
    // dependencies. The generated lockfile belongs only to scratch.
    fs::copy(api.join("Cargo.lock"), scratch.join("Cargo.lock")).expect("seed API lockfile");
    let cargo = std::env::var_os("CARGO").expect("Cargo test runner");
    let lock = Command::new(&cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(scratch.join("Cargo.toml"))
        .output()
        .expect("resolve native contract harness");
    assert!(
        lock.status.success(),
        "harness setup is not assertion-level RED:\n{}\n{}",
        String::from_utf8_lossy(&lock.stdout),
        String::from_utf8_lossy(&lock.stderr)
    );
    let result = Command::new(cargo)
        .args(["test", "--locked", "--manifest-path"])
        .arg(scratch.join("Cargo.toml"))
        // Do not compete for the outer Cargo invocation's target lock.
        .arg("--target-dir")
        .arg(scratch.join("target"))
        .args(["--test", "alphabet", "--", "--nocapture"])
        .output()
        .expect("run native stream/API contract assertions");
    assert!(
        result.status.success(),
        "native stream/API contract failed (compile errors are not RED):\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

const CONTRACT_TESTS: &str = r#"
use nntune_api::ledger::{
    self, classify_replay, prepare_ledger_row, prepare_play_ledger_row, LedgerError, ReplayOutcome,
};
use nntune_stream::meter::{derive_play_idempotency_key, stream_seconds_for_bytes, CompletePlay};

const USER: &str = "user-1";
const TRACK: &str = "track-9";
const PLAY: &str = "play-7";
const AT: u64 = 1_767_225_600;
const BYTES: u64 = 48_001;
const SECONDS: u64 = 2;

fn identity(field: usize, value: &str) -> [&str; 3] {
    let mut fields = [USER, TRACK, PLAY];
    fields[field] = value;
    fields
}

// Every non-alphabet ASCII character, plus multi-byte examples. Each test
// collects all failures so baseline RED identifies more than the first dot.
fn disallowed_values() -> Vec<String> {
    let mut values: Vec<String> = (0u8..=127)
        .filter(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'-' | b'_'))
        .map(|b| format!("id{}x", char::from(b)))
        .collect();
    values.extend(
        ["track.v1", "a/b", "café", "東京", "a\u{301}", "a💿b"]
            .into_iter()
            .map(str::to_owned),
    );
    values
}

fn assert_derivation_refuses(field: usize, api: bool) {
    let accepted: Vec<_> = disallowed_values()
        .into_iter()
        .filter(|value| {
            let [user, track, play] = identity(field, value);
            if api {
                ledger::derive_play_idempotency_key(user, track, play).is_ok()
            } else {
                derive_play_idempotency_key(user, track, play).is_ok()
            }
        })
        .collect();
    assert!(
        accepted.is_empty(),
        "{} derivation accepted non-persistable identity field {field}: {accepted:?}",
        if api { "API" } else { "stream" }
    );
}

fn assert_complete_play_refuses(field: usize) {
    let emitted: Vec<_> = disallowed_values()
        .into_iter()
        .filter(|value| {
            let [user, track, play] = identity(field, value);
            CompletePlay::new(user, track, play, BYTES).is_ok()
        })
        .collect();
    assert!(
        emitted.is_empty(),
        "CompletePlay emitted non-persistable identity field {field}: {emitted:?}"
    );
}

macro_rules! rejection_tests {
    ($stream:ident, $api:ident, $complete:ident, $field:expr) => {
        #[test]
        fn $stream() {
            assert_derivation_refuses($field, false);
        }
        #[test]
        fn $api() {
            assert_derivation_refuses($field, true);
        }
        #[test]
        fn $complete() {
            assert_complete_play_refuses($field);
        }
    };
}
rejection_tests!(
    stream_rejects_user_alphabet,
    api_rejects_user_alphabet,
    complete_play_rejects_user_alphabet,
    0
);
rejection_tests!(
    stream_rejects_track_alphabet,
    api_rejects_track_alphabet,
    complete_play_rejects_track_alphabet,
    1
);
rejection_tests!(
    stream_rejects_play_alphabet,
    api_rejects_play_alphabet,
    complete_play_rejects_play_alphabet,
    2
);

fn feed_through(user: &str, track: &str, play_id: &str, expected_key: &str) {
    let play = CompletePlay::new(user, track, play_id, BYTES).expect("ordinary CompletePlay");
    assert_eq!(play.idempotency_key, expected_key, "no identity rewriting");
    assert_eq!(play.user_id, user);
    assert_eq!(play.track_id, track);
    assert_eq!(play.bytes, BYTES);
    assert_eq!(
        derive_play_idempotency_key(user, track, play_id).unwrap(),
        expected_key
    );
    assert_eq!(
        ledger::derive_play_idempotency_key(user, track, play_id).unwrap(),
        expected_key
    );
    assert_eq!(stream_seconds_for_bytes(play.bytes), SECONDS);
    let row = prepare_ledger_row(
        &play.user_id,
        &play.track_id,
        &play.idempotency_key,
        play.bytes,
        stream_seconds_for_bytes(play.bytes),
        AT,
    )
    .expect("accepted stream key must prepare an actual API ledger row");
    assert_eq!(row.idempotency_key, expected_key);
    assert_eq!(row.user_id, user);
    assert_eq!(row.track_id, track);
    assert_eq!(
        (row.bytes_served, row.stream_seconds, row.recorded_at),
        (BYTES, SECONDS, AT)
    );
    assert_eq!(row.period_month, "2026-01");
    let retry = prepare_play_ledger_row(user, track, play_id, BYTES, SECONDS, AT).unwrap();
    assert_eq!(row, retry);
    assert_eq!(classify_replay(None, &row), ReplayOutcome::Fresh);
    assert_eq!(
        classify_replay(Some(&row), &retry),
        ReplayOutcome::Duplicate
    );
    let changed = prepare_play_ledger_row(user, track, play_id, 96_000, 2, AT).unwrap();
    assert_eq!(
        classify_replay(Some(&row), &changed),
        ReplayOutcome::Conflict
    );
}

#[test]
fn ascii_handles_and_uuid_keys_feed_the_actual_ledger_unchanged() {
    for (user, track, play, expected) in [
        (USER, TRACK, PLAY, "play:user-1:track-9:play-7"),
        ("A_z-09", "TRACK_v1-2", "Play_7-A", "play:A_z-09:TRACK_v1-2:Play_7-A"),
        ("550e8400-e29b-41d4-a716-446655440000", "c56a4180-65aa-42ec-a945-5fd21dec0538",
         "123e4567-e89b-12d3-a456-426614174000",
         "play:550e8400-e29b-41d4-a716-446655440000:c56a4180-65aa-42ec-a945-5fd21dec0538:123e4567-e89b-12d3-a456-426614174000"),
    ] {
        feed_through(user, track, play, expected);
    }
    for b in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-" {
        let value = format!("{}", char::from(*b));
        for field in 0..3 {
            let [user, track, play] = identity(field, &value);
            feed_through(user, track, play, &format!("play:{user}:{track}:{play}"));
        }
    }
}

#[test]
fn maximum_ascii_fields_feed_the_actual_ledger() {
    let user = "U".repeat(128);
    let track = "T".repeat(512);
    let play = "P".repeat(512);
    feed_through(&user, &track, &play, &format!("play:{user}:{track}:{play}"));
}

#[test]
fn empty_overlong_and_delimiter_fields_still_fail_closed() {
    for (field, limit) in [(0, 128), (1, 512), (2, 512)] {
        for value in [
            String::new(),
            "x".repeat(limit + 1),
            "a:b".into(),
            "a=b".into(),
            "a b".into(),
            "a\0b".into(),
            "a\nb".into(),
            "é".repeat(limit / 2 + 1),
        ] {
            let [user, track, play] = identity(field, &value);
            assert!(
                derive_play_idempotency_key(user, track, play).is_err(),
                "stream field {field}: {value:?}"
            );
            assert_eq!(
                ledger::derive_play_idempotency_key(user, track, play),
                Err(LedgerError::Invalid)
            );
            assert!(CompletePlay::new(user, track, play, BYTES).is_err());
        }
    }
}

#[test]
fn punctuation_and_unicode_keys_remain_unpersistable() {
    // Keep the downstream alphabet narrow; widening ledger/schema instead
    // of rejecting at derivation is not an acceptable fix.
    for value in ["track.v1", "a/b", "café", "東京"] {
        for field in 0..3 {
            let [user, track, play] = identity(field, value);
            let key = format!("play:{user}:{track}:{play}");
            assert_eq!(
                prepare_ledger_row(user, track, &key, BYTES, SECONDS, AT),
                Err(LedgerError::Invalid)
            );
        }
    }
}

#[test]
fn valid_play_accounting_and_identity_binding_are_unchanged() {
    for (bytes, seconds) in [(1, 1), (48_000, 1), (48_001, 2), (96_000, 2)] {
        let play = CompletePlay::new(USER, TRACK, PLAY, bytes).unwrap();
        let row =
            prepare_ledger_row(USER, TRACK, &play.idempotency_key, bytes, seconds, AT).unwrap();
        assert_eq!((row.bytes_served, row.stream_seconds), (bytes, seconds));
        assert_eq!(stream_seconds_for_bytes(play.bytes), seconds);
    }
    let first = CompletePlay::new(USER, TRACK, PLAY, BYTES).unwrap();
    for (user, track, play) in [
        ("user-2", TRACK, PLAY),
        (USER, "track-8", PLAY),
        (USER, TRACK, "play-8"),
    ] {
        let second = CompletePlay::new(user, track, play, BYTES).unwrap();
        assert_ne!(first.idempotency_key, second.idempotency_key);
        prepare_ledger_row(user, track, &second.idempotency_key, BYTES, SECONDS, AT).unwrap();
    }
    assert_eq!(
        prepare_ledger_row("user-2", TRACK, &first.idempotency_key, BYTES, SECONDS, AT),
        Err(LedgerError::Invalid)
    );
    assert_eq!(
        prepare_ledger_row(USER, "track-8", &first.idempotency_key, BYTES, SECONDS, AT),
        Err(LedgerError::Invalid)
    );
    assert_eq!(
        prepare_play_ledger_row(USER, TRACK, PLAY, 0, 0, AT),
        Err(LedgerError::Invalid)
    );
    assert_eq!(
        prepare_play_ledger_row(USER, TRACK, PLAY, BYTES, 1, AT),
        Err(LedgerError::Invalid)
    );
}
"#;
