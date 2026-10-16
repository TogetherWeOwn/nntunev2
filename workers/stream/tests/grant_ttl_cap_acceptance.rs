//! Acceptance tests (failing-first): play grants must be short-lived.
//!
//! Behavioural half of the contract, written against the *existing*
//! public API (`grant::issue` / `grant::verify` / `play::play`) so it
//! compiles on the current head and fails at runtime where the cap is
//! missing:
//!
//! - a grant is accepted only while `exp - now <= MAX_GRANT_TTL`
//!   (15 minutes = 900 seconds), plus a bounded clock-skew allowance
//!   (60 seconds) for a verifier whose clock lags the issuer's;
//! - a validly-signed grant beyond cap + skew is rejected with a
//!   distinct error code — not `Expired`, `BadSignature` or
//!   `Malformed`, so callers/logs can tell "minted too long" apart
//!   from the pre-existing failure modes;
//! - `now >= exp` stays `Expired` (skew never resurrects a grant);
//! - signature failure still wins over the TTL check (no oracle for
//!   unsigned/tampered tokens), and wrong-track binding is still
//!   rejected on the play path;
//! - the cap and skew values are documented in `workers/stream/README.md`.
//!
//! The exact names of the new constants, error variant and the
//! clamping issuer are pinned in `grant_ttl_contract_acceptance.rs`.
//! Do not edit these tests to make them pass: implement the cap.

use std::net::SocketAddr;
use std::path::PathBuf;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant::{self, GrantError};
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"grant-ttl-acceptance-secret";
const OTHER_SECRET: &[u8] = b"grant-ttl-other-secret";
/// Fake clock: every case is expressed relative to this instant.
const NOW: u64 = 1_700_000_000;
/// 15 minutes, in seconds.
const MAX_GRANT_TTL_SECS: u64 = 900;
/// Bounded verifier/issuer clock skew tolerated on top of the cap.
const CLOCK_SKEW_SECS: u64 = 60;

/// The rejection for an over-cap grant must be a *new, distinct* code:
/// reusing `Expired`/`Malformed`/`BadSignature` would mislabel a
/// validly-signed, unexpired grant and hide issuer misconfiguration.
fn assert_distinct_ttl_rejection(result: Result<grant::Grant, GrantError>, case: &str) {
    assert!(result.is_err(), "{case}: over-cap grant must be rejected");
    assert_ne!(result, Err(GrantError::Expired), "{case}");
    assert_ne!(result, Err(GrantError::BadSignature), "{case}");
    assert_ne!(result, Err(GrantError::Malformed), "{case}");
}

fn token_exp(exp_unix: u64) -> String {
    grant::issue("user-1", "track-9", exp_unix, SECRET)
}

#[test]
fn at_cap_grant_is_accepted() {
    let token = token_exp(NOW + MAX_GRANT_TTL_SECS);
    let granted = grant::verify(&token, SECRET, NOW).expect("exp - now == cap must verify");
    assert_eq!(granted.user_id, "user-1");
    assert_eq!(granted.track_id, "track-9");
    assert_eq!(granted.exp_unix, NOW + MAX_GRANT_TTL_SECS);
}

#[test]
fn under_cap_grants_are_accepted() {
    for ttl in [1, 300, MAX_GRANT_TTL_SECS - 1] {
        let token = token_exp(NOW + ttl);
        assert!(
            grant::verify(&token, SECRET, NOW).is_ok(),
            "ttl {ttl}s must verify"
        );
    }
}

#[test]
fn grant_within_clock_skew_over_cap_is_accepted() {
    // The issuer's clock ran ahead of the verifier's: an at-cap grant
    // can present as cap + (issuer - verifier) skew. Up to the bounded
    // skew allowance it must still verify — exactly at the boundary too.
    for over in [1, 30, CLOCK_SKEW_SECS] {
        let token = token_exp(NOW + MAX_GRANT_TTL_SECS + over);
        assert!(
            grant::verify(&token, SECRET, NOW).is_ok(),
            "cap + {over}s skew must verify"
        );
    }
}

#[test]
fn grant_just_beyond_cap_plus_skew_is_rejected() {
    let token = token_exp(NOW + MAX_GRANT_TTL_SECS + CLOCK_SKEW_SECS + 1);
    assert_distinct_ttl_rejection(grant::verify(&token, SECRET, NOW), "cap + skew + 1s");
}

#[test]
fn far_future_grant_with_valid_signature_is_rejected() {
    // A captured grant must not replay until a distant expiry: even a
    // perfectly signed token is rejected once `exp - now` exceeds
    // cap + skew. `u64::MAX` also pins overflow-safe arithmetic.
    for (case, exp) in [
        ("one hour", NOW + 3_600),
        ("one day", NOW + 86_400),
        ("year 2286 sentinel used by older fixtures", 9_999_999_999),
        ("u64::MAX", u64::MAX),
    ] {
        let token = token_exp(exp);
        assert_distinct_ttl_rejection(grant::verify(&token, SECRET, NOW), case);
    }
}

#[test]
fn cap_is_measured_from_verify_time_not_absolute_expiry() {
    // Same token, two verifier clocks: at issuance time the TTL is
    // exactly at cap; a verifier an hour behind sees `exp - now` of
    // cap + 3600s, far beyond cap + skew, and must reject.
    let token = token_exp(NOW + MAX_GRANT_TTL_SECS);
    assert!(grant::verify(&token, SECRET, NOW).is_ok());
    assert_distinct_ttl_rejection(
        grant::verify(&token, SECRET, NOW - 3_600),
        "verifier clock one hour behind",
    );
}

#[test]
fn near_u64_max_clock_within_cap_is_accepted_without_overflow() {
    // `now + cap + skew` overflows `u64` here if the check is written
    // with plain addition; the comparison must be saturating/subtractive.
    let now = u64::MAX - 10;
    let token = token_exp(u64::MAX);
    let granted = grant::verify(&token, SECRET, now).expect("exp - now == 10s must verify");
    assert_eq!(granted.exp_unix, u64::MAX);
}

#[test]
fn expired_stays_expired_at_and_after_exp() {
    let token = token_exp(NOW + MAX_GRANT_TTL_SECS);
    assert_eq!(
        grant::verify(&token, SECRET, NOW + MAX_GRANT_TTL_SECS),
        Err(GrantError::Expired),
        "now == exp is expired"
    );
    assert_eq!(
        grant::verify(&token, SECRET, NOW + MAX_GRANT_TTL_SECS + 1),
        Err(GrantError::Expired),
        "now > exp is expired"
    );
    // Near the top of the clock range a wrapping `now + cap + skew`
    // comparison must not turn an expired grant back into a live one.
    let token = token_exp(u64::MAX - 5);
    assert_eq!(
        grant::verify(&token, SECRET, u64::MAX),
        Err(GrantError::Expired),
        "expired near u64::MAX stays expired"
    );
}

#[test]
fn clock_skew_never_resurrects_an_expired_grant() {
    // Skew widens the *upper* bound only. A grant that expired within
    // the last skew window (or long ago) is still `Expired`, never Ok
    // and never re-labelled as a TTL rejection.
    let token = token_exp(NOW);
    for now in [NOW, NOW + 1, NOW + CLOCK_SKEW_SECS, NOW + 86_400] {
        assert_eq!(
            grant::verify(&token, SECRET, now),
            Err(GrantError::Expired),
            "now {now}"
        );
    }
}

#[test]
fn tampered_over_cap_token_is_bad_signature_not_a_ttl_rejection() {
    // Signature is checked before the TTL bound: a forged token must
    // not learn anything about the cap from the error code.
    let token = token_exp(NOW + 86_400);
    let mut tampered = token.clone();
    let dot = tampered.find('.').unwrap();
    // SAFETY of the test trick (mirrors the unit test in grant.rs):
    // the payload segment is ASCII base64url, flipping one ASCII byte
    // to another ASCII byte keeps the string valid UTF-8.
    let bytes = unsafe { tampered.as_bytes_mut() };
    bytes[dot + 1] = if bytes[dot + 1] == b'A' { b'B' } else { b'A' };
    assert_eq!(
        grant::verify(&tampered, SECRET, NOW),
        Err(GrantError::BadSignature)
    );
}

#[test]
fn wrong_secret_over_cap_token_is_bad_signature() {
    let token = token_exp(NOW + 86_400);
    assert_eq!(
        grant::verify(&token, OTHER_SECRET, NOW),
        Err(GrantError::BadSignature)
    );
}

#[test]
fn malformed_tokens_are_still_malformed() {
    for bad in ["", "v1", "v1..", "v2.a.b", "not a token"] {
        assert_eq!(
            grant::verify(bad, SECRET, NOW),
            Err(GrantError::Malformed),
            "{bad:?}"
        );
    }
}

fn play_input<'a>(token: &'a str, track_id: &'a str) -> PlayInput<'a> {
    PlayInput {
        token,
        track_id,
        range: None,
        // Never dialed in these cases: authorization fails first.
        server_addr: "127.0.0.1:119".parse::<SocketAddr>().unwrap(),
        segments: &[],
        cipher_key: 0,
        now_unix: NOW,
        up_next: None,
    }
}

#[test]
fn wrong_track_binding_is_still_rejected_on_the_play_path() {
    // Within-cap grant for track-9 presented for another track: the
    // TTL cap must not weaken (or relabel) track binding.
    let token = token_exp(NOW + MAX_GRANT_TTL_SECS);
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let input = play_input(&token, "track-other");
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &input),
        Err(PlayError::WrongTrack)
    ));
    assert_eq!(pool.connects(), 0, "rejected plays must not dial out");
}

#[test]
fn over_cap_grant_is_unauthorized_on_the_play_path_even_for_the_right_track() {
    let token = token_exp(9_999_999_999);
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let input = play_input(&token, "track-9");
    match play(&mut pool, &mut cache, &mut meter, SECRET, &input) {
        Err(PlayError::Unauthorized(err)) => {
            assert_ne!(err, GrantError::Expired);
            assert_ne!(err, GrantError::BadSignature);
            assert_ne!(err, GrantError::Malformed);
        }
        Err(other) => panic!("expected Unauthorized TTL rejection, got {other:?}"),
        Ok(_) => panic!("expected Unauthorized TTL rejection, got a servable play"),
    }
    assert_eq!(pool.connects(), 0, "rejected plays must not dial out");
    assert_eq!(pool.in_use(), 0);
}

fn stream_readme() -> String {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn readme_documents_the_cap_skew_and_rejection() {
    let readme = stream_readme();
    assert!(
        readme.contains("MAX_GRANT_TTL"),
        "README must name the MAX_GRANT_TTL cap"
    );
    assert!(
        readme.contains("15 min") || readme.contains("900"),
        "README must state the cap value (15 min / 900s)"
    );
    let lower = readme.to_ascii_lowercase();
    assert!(
        lower.contains("skew") && readme.contains("60"),
        "README must state the tolerated clock skew (60s)"
    );
    assert!(
        lower.contains("reject") || lower.contains("clamp"),
        "README must say what happens beyond the cap (rejected/clamped)"
    );
}
