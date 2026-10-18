//! Acceptance tests (failing-first): the public contract the
//! implementation must add to `nntune_stream::grant`.
//!
//! This file intentionally does not compile on the current head: it
//! names the API surface the implementation has to provide. The
//! behavioural expectations behind these names are pinned against the
//! existing API in `grant_ttl_cap_acceptance.rs`.
//!
//! Contract to implement (change no existing signature, change no
//! `v1.` wire shape):
//!
//! - `pub const MAX_GRANT_TTL: u64 = 900;` — maximum grant lifetime,
//!   15 minutes, in seconds.
//! - `pub const CLOCK_SKEW_SECS: u64 = 60;` — verifier/issuer clock
//!   skew tolerated on top of the cap when verifying. (Same name and
//!   value as the session clock-skew bound in the api Worker, so the
//!   codebase has one skew vocabulary.)
//! - `GrantError::TtlTooLong` — the documented rejection code returned
//!   by `verify` when `exp - now > MAX_GRANT_TTL + CLOCK_SKEW_SECS`,
//!   even for a validly-signed token. Checked after the signature and
//!   after expiry (`now >= exp` stays `Expired`), computed with
//!   saturating/subtractive arithmetic: `exp == u64::MAX` at a normal
//!   `now` must reject, while `exp - now == 10` near `u64::MAX` must
//!   still verify — a plain `now + cap + skew` comparison would wrap.
//! - `pub fn issue_at(user_id, track_id, now_unix, ttl_secs, secret)
//!   -> String` — the production issuer: mints a grant expiring at
//!   `now_unix + min(ttl_secs, MAX_GRANT_TTL)` (saturating). Requests
//!   beyond the cap are *clamped*, mirroring how session issuance in
//!   the api Worker clamps a requested lifetime to its maximum; the
//!   verify-side `TtlTooLong` bound is the hard enforcement against
//!   any other (or older) issuer. The pre-existing
//!   `issue(user_id, track_id, exp_unix, secret)` stays as the raw
//!   signer so existing callers and these tests can mint arbitrary
//!   expiries.
//! - `workers/stream/README.md` documents `MAX_GRANT_TTL`,
//!   `CLOCK_SKEW_SECS` and the `TtlTooLong` rejection.
//!
//! Existing fixtures that mint far-future grants for *successful*
//! plays (e.g. an expiry in the year 2286) will need to move inside
//! the cap when this lands — that is the point of the cap.
//!
//! Do not edit these tests to make them pass: implement the contract.

use std::net::SocketAddr;
use std::path::PathBuf;

use nntune_stream::cache::ContentCache;
use nntune_stream::grant::{self, GrantError};
use nntune_stream::meter::MemoryMeter;
use nntune_stream::play::{play, PlayError, PlayInput, StreamPool};
use nntune_stream::pool::Pool;

const SECRET: &[u8] = b"grant-ttl-contract-secret";
const NOW: u64 = 1_700_000_000;

#[test]
fn cap_and_skew_constants_have_the_documented_values() {
    assert_eq!(grant::MAX_GRANT_TTL, 900);
    assert_eq!(grant::MAX_GRANT_TTL, 15 * 60, "cap is 15 minutes");
    assert_eq!(grant::CLOCK_SKEW_SECS, 60);
}

#[test]
fn issue_at_clamps_a_request_beyond_the_cap() {
    for requested in [grant::MAX_GRANT_TTL + 1, 3_600, 86_400, u64::MAX] {
        let token = grant::issue_at("user-1", "track-9", NOW, requested, SECRET);
        let granted =
            grant::verify(&token, SECRET, NOW).expect("clamped grant must verify at issue time");
        assert_eq!(
            granted.exp_unix,
            NOW + grant::MAX_GRANT_TTL,
            "requested ttl {requested}s must clamp to the cap"
        );
    }
}

#[test]
fn issue_at_preserves_at_and_under_cap_ttls() {
    for ttl in [1, 300, grant::MAX_GRANT_TTL] {
        let token = grant::issue_at("user-1", "track-9", NOW, ttl, SECRET);
        let granted = grant::verify(&token, SECRET, NOW).expect("within-cap grant must verify");
        assert_eq!(granted.exp_unix, NOW + ttl);
        assert_eq!(granted.user_id, "user-1");
        assert_eq!(granted.track_id, "track-9");
    }
}

#[test]
fn issue_at_zero_ttl_is_already_expired() {
    let token = grant::issue_at("user-1", "track-9", NOW, 0, SECRET);
    assert_eq!(grant::verify(&token, SECRET, NOW), Err(GrantError::Expired));
}

#[test]
fn issue_at_saturates_instead_of_overflowing_near_u64_max() {
    let now = u64::MAX - 10;
    let token = grant::issue_at("user-1", "track-9", now, u64::MAX, SECRET);
    let granted = grant::verify(&token, SECRET, now).expect("saturated grant must verify");
    assert_eq!(granted.exp_unix, u64::MAX);
}

#[test]
fn verify_rejects_over_cap_grants_with_ttl_too_long() {
    for exp in [
        NOW + grant::MAX_GRANT_TTL + grant::CLOCK_SKEW_SECS + 1,
        NOW + 86_400,
        9_999_999_999,
        u64::MAX,
    ] {
        let token = grant::issue("user-1", "track-9", exp, SECRET);
        assert_eq!(
            grant::verify(&token, SECRET, NOW),
            Err(GrantError::TtlTooLong),
            "exp {exp}"
        );
    }
}

#[test]
fn verify_accepts_exactly_cap_plus_skew_and_rejects_one_second_more() {
    let boundary = NOW + grant::MAX_GRANT_TTL + grant::CLOCK_SKEW_SECS;
    let token = grant::issue("user-1", "track-9", boundary, SECRET);
    assert!(grant::verify(&token, SECRET, NOW).is_ok());

    let token = grant::issue("user-1", "track-9", boundary + 1, SECRET);
    assert_eq!(
        grant::verify(&token, SECRET, NOW),
        Err(GrantError::TtlTooLong)
    );
}

#[test]
fn play_path_reports_over_cap_grants_as_unauthorized_ttl_too_long() {
    let token = grant::issue("user-1", "track-9", NOW + 86_400, SECRET);
    let mut pool: StreamPool = Pool::new(1);
    let mut cache = ContentCache::new();
    let mut meter = MemoryMeter::default();
    let input = PlayInput {
        trace_id: None,
        token: &token,
        track_id: "track-9",
        range: None,
        server_addr: "127.0.0.1:119".parse::<SocketAddr>().unwrap(),
        segments: &[],
        cipher_key: 0,
        now_unix: NOW,
        up_next: None,
        segment_ranges: None,
    };
    assert!(matches!(
        play(&mut pool, &mut cache, &mut meter, SECRET, &input),
        Err(PlayError::Unauthorized(GrantError::TtlTooLong))
    ));
    assert_eq!(pool.connects(), 0, "rejected plays must not dial out");
}

#[test]
fn readme_names_the_constants_and_the_rejection_code() {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for needle in ["MAX_GRANT_TTL", "CLOCK_SKEW_SECS", "TtlTooLong"] {
        assert!(readme.contains(needle), "README must document {needle}");
    }
}
