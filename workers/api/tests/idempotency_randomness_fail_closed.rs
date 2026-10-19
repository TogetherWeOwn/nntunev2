//! Fail-closed randomness for fresh idempotency keys (tests-only).
//!
//! Contract under test — the Builder implements exactly this shape in
//! `workers/api/src/ledger.rs` and must not edit this file:
//!
//! - `pub fn new_idempotency_key() -> Result<String, LedgerError>` (today:
//!   `-> String`, with `getrandom::fill(&mut bytes).expect("secure
//!   randomness unavailable")` at `ledger.rs:114` — in a Worker that panic
//!   is an uncaught 500 on the billing path).
//! - A dedicated unit variant `LedgerError::RandomnessUnavailable`,
//!   distinct from `LedgerError::Invalid`, with
//!   `LedgerError::RandomnessUnavailable => 503` in `http_status()` and
//!   `LedgerError::RandomnessUnavailable => write!(f, "secure randomness
//!   unavailable")` in `Display`. That is the typed error callers map to
//!   the existing 503 shape (`ApiError::service_unavailable`: status 503,
//!   code `service_unavailable`, retryable, message sealed to
//!   `service unavailable`). `Invalid` stays 422 / `invalid usage row`.
//! - The mint body propagates the entropy failure, verbatim in shape:
//!
//!   ```ignore
//!   pub fn new_idempotency_key() -> Result<String, LedgerError> {
//!       let mut bytes = [0u8; 16];
//!       getrandom::fill(&mut bytes).map_err(|_| LedgerError::RandomnessUnavailable)?;
//!       Ok(crate::b64::encode(&bytes))
//!   }
//!   ```
//!
//!   Never a panic (`expect`/`unwrap`/`panic!`), never a fallback key
//!   (`unwrap_or*`, a cached/static key, encoding the zero buffer), never
//!   a reused key. The success path is byte-identical: the same 16
//!   entropy bytes through the same `crate::b64::encode`, i.e. a fresh
//!   22-char unpadded-base64url key per call.
//! - Existing callers in `ledger.rs` (in-module tests) and
//!   `tests/ledger_tests.rs` move to the fallible signature; there are no
//!   production callers of the fresh-key mint outside those tests today.
//!
//! Why source assertions + a compile-compatible shim: native `getrandom`
//! failure cannot be injected through this crate's public API (no test
//! hook, and process-wide syscall fault injection is not portable to
//! CI/wasm). The failure path is therefore pinned by (a) the fallible
//! signature, observed at a real call site through the `MintOutcome`
//! shim below, (b) the sole `getrandom::fill` in `ledger.rs` propagating
//! with `map_err(...)?` to the dedicated variant, and (c) a ban on every
//! panic/fallback token in the mint body. The shim has one impl for
//! today's infallible `String` and one for the required
//! `Result<String, LedgerError>`, so this file compiles on the RED base
//! (failing by assertion, not compile error) and still compiles,
//! unchanged, once the Builder flips the signature. If the Builder
//! returns `Result<String, SomeOtherError>`, the shim no longer matches
//! and the suite stops compiling — that is the signature pin working.
//!
//! Sensitivity (each turns at least one RED test back red):
//!
//! - Keeping `.expect("secure randomness unavailable")`, or swapping in
//!   `.unwrap()`, breaks the no-expect and body-ban pins.
//! - Ignoring the error (`let _ = getrandom::fill(...)`) or encoding the
//!   buffer anyway drops the `map_err(...)?` propagation pin and adds a
//!   second (or loses the only) `Ok(` in the body.
//! - `unwrap_or_default()` / a fixed fallback / a `static` cached key
//!   trips the fallback-token ban; returning the all-zero encoding
//!   `AAAAAAAAAAAAAAAAAAAAAA` is also rejected behaviourally.
//! - Mapping entropy failure to `LedgerError::Invalid` (422) instead of
//!   the dedicated variant breaks the variant and `=> 503` pins; Rust's
//!   exhaustiveness check forces both `http_status` and `Display` to
//!   cover the new variant, which the occurrence count tracks.
//! - Changing the success encoding (byte count, encoder, alphabet)
//!   breaks the body pin and the 22-char / 16-byte behaviour tests.
//!
//! Out of scope: every other randomness `expect` in the api crate
//! (`auth`, `session`, `vault`, `library`, `native_rest`, `handlers`),
//! `derive_play_idempotency_key` validation, and retry semantics. This
//! suite pins only `ledger::new_idempotency_key`.
//!
//! Run only this file:
//!
//! ```sh
//! cargo test --locked --manifest-path workers/api/Cargo.toml \
//!     --test idempotency_randomness_fail_closed
//! ```

use std::collections::HashSet;

use nntune_api::api_error::ApiError;
use nntune_api::b64;
use nntune_api::ledger::{
    derive_play_idempotency_key, new_idempotency_key, prepare_ledger_row, LedgerError,
};

const LEDGER_SRC: &str = include_str!("../src/ledger.rs");

/// Compile-compatible view of the mint result (see module docs). Today's
/// infallible `String` reports `is_fallible() == false`; the required
/// `Result<String, LedgerError>` reports `true` and hands back the real
/// typed error.
trait MintOutcome {
    fn into_key_result(self) -> Result<String, LedgerError>;
    fn is_fallible(&self) -> bool;
}

impl MintOutcome for String {
    fn into_key_result(self) -> Result<String, LedgerError> {
        Ok(self)
    }
    fn is_fallible(&self) -> bool {
        false
    }
}

impl MintOutcome for Result<String, LedgerError> {
    fn into_key_result(self) -> Result<String, LedgerError> {
        self
    }
    fn is_fallible(&self) -> bool {
        true
    }
}

fn minted_key() -> String {
    new_idempotency_key()
        .into_key_result()
        .expect("minting with a working OS RNG must succeed")
}

fn normalized(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The whitespace-normalized body of `new_idempotency_key`, from its
/// signature to its closing brace at column zero.
fn mint_body() -> String {
    let start = LEDGER_SRC
        .find("pub fn new_idempotency_key")
        .expect("ledger.rs must define pub fn new_idempotency_key");
    let rest = &LEDGER_SRC[start..];
    let end = rest
        .find("\n}\n")
        .expect("new_idempotency_key must end with a closing brace at column zero");
    normalized(&rest[..end])
}

// --- RED: the fail-closed contract (fails on the base SHA) -------------------

#[test]
fn mint_is_fallible_and_returns_typed_ledger_error() {
    let outcome = new_idempotency_key();
    assert!(
        outcome.is_fallible(),
        "new_idempotency_key must return Result<String, LedgerError> so a \
         randomness failure is a typed error, not a panic"
    );
    assert!(
        outcome.into_key_result().is_ok(),
        "the success path must still mint a key"
    );
    assert!(
        normalized(LEDGER_SRC)
            .contains("pub fn new_idempotency_key() -> Result<String, LedgerError>"),
        "signature must be exactly \
         `pub fn new_idempotency_key() -> Result<String, LedgerError>`"
    );
}

#[test]
fn ledger_source_has_no_expect_on_secure_randomness() {
    assert!(
        !LEDGER_SRC.contains(".expect(\"secure randomness unavailable\")"),
        "no production path in ledger.rs may panic when secure randomness \
         is unavailable; propagate a typed error instead"
    );
}

#[test]
fn randomness_failure_propagates_instead_of_panicking_or_falling_back() {
    let body = mint_body();
    assert!(
        body.contains(
            "getrandom::fill(&mut bytes).map_err(|_| LedgerError::RandomnessUnavailable)?;"
        ),
        "entropy failure must propagate as LedgerError::RandomnessUnavailable \
         via map_err(...)?, body was: {body}"
    );
    assert!(
        body.contains("let mut bytes = [0u8; 16];"),
        "success path must still draw exactly 16 entropy bytes, body was: {body}"
    );
    assert!(
        body.contains("Ok(crate::b64::encode(&bytes))"),
        "success path must stay byte-identical: Ok(crate::b64::encode(&bytes)), \
         body was: {body}"
    );
    assert_eq!(
        body.matches("Ok(").count(),
        1,
        "exactly one Ok( in the mint body: any fallback Ok( means a key was \
         minted without entropy, body was: {body}"
    );
    assert_eq!(
        body.matches("getrandom::fill").count(),
        1,
        "the mint must draw entropy exactly once, body was: {body}"
    );
    assert_eq!(
        LEDGER_SRC.matches("getrandom::fill").count(),
        1,
        "ledger.rs must contain exactly one getrandom::fill (the mint's)"
    );
    for banned in [
        ".expect(",
        ".unwrap(",
        ".unwrap_or",
        "panic!",
        "todo!",
        "unimplemented!",
        "OnceLock",
        "static ",
    ] {
        assert!(
            !body.contains(banned),
            "mint body must never panic or fall back: found `{banned}` in: {body}"
        );
    }
}

#[test]
fn randomness_unavailable_is_a_distinct_503_typed_error() {
    let source = normalized(LEDGER_SRC);
    assert!(
        source.contains("RandomnessUnavailable,"),
        "LedgerError must declare a dedicated RandomnessUnavailable variant"
    );
    assert!(
        source.contains("LedgerError::RandomnessUnavailable => 503,"),
        "RandomnessUnavailable must map to HTTP 503 in http_status() so \
         callers can emit the existing service_unavailable shape"
    );
    assert!(
        source.contains(
            "LedgerError::RandomnessUnavailable => write!(f, \"secure randomness unavailable\")"
        ),
        "Display must give the typed error a generic, input-free name"
    );
    assert!(
        source.matches("RandomnessUnavailable").count() >= 4,
        "the variant must appear in the enum, http_status, Display, and the \
         map_err propagation (found {})",
        source.matches("RandomnessUnavailable").count()
    );
    // Distinct from the shape error: entropy failure is not a 422.
    assert!(
        source.contains("LedgerError::Invalid => 422,"),
        "Invalid must stay the 422 variant"
    );
}

// --- Green controls: adjacent behavior that must stay green ------------------

#[test]
fn minted_keys_keep_the_existing_22_char_base64url_shape() {
    let key = minted_key();
    assert_eq!(key.len(), 22, "128-bit key must stay 22 base64url chars");
    assert!(
        key.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "key must stay in the unpadded base64url alphabet: {key}"
    );
    let raw = b64::decode(&key).expect("minted key must decode as base64url");
    assert_eq!(raw.len(), 16, "minted key must still carry 128 bits");
    assert_ne!(
        key, "AAAAAAAAAAAAAAAAAAAAAA",
        "the all-zero encoding must never be minted: entropy must be drawn \
         and its failure must not degrade to a zero key"
    );
    assert!(
        raw.iter().any(|&b| b != 0),
        "minted key bytes must not be zero"
    );
    // The minted handle is accepted by the adjacent ledger intent path.
    let row = prepare_ledger_row("user-alice", "track-1", &key, 48_000, 1, 1_767_225_600)
        .expect("a fresh minted key must prepare a ledger row");
    assert_eq!(row.idempotency_key, key);
}

#[test]
fn minted_keys_are_fresh_never_reused() {
    let keys: HashSet<String> = (0..128).map(|_| minted_key()).collect();
    assert_eq!(
        keys.len(),
        128,
        "every mint must be a fresh key: no caching, no reuse"
    );
}

#[test]
fn deterministic_play_keys_and_invalid_rows_are_unchanged() {
    assert_eq!(
        derive_play_idempotency_key("user-1", "track-9", "play-7"),
        Ok("play:user-1:track-9:play-7".to_string()),
        "deterministic play-key derivation must not move"
    );
    assert_eq!(
        derive_play_idempotency_key("a:b", "track", "play"),
        Err(LedgerError::Invalid),
        "key-field validation must still reject separators"
    );
    assert_eq!(
        LedgerError::Invalid.http_status(),
        422,
        "shape violations stay 422; only randomness failure is 503"
    );
    assert_eq!(
        LedgerError::Invalid.to_string(),
        "invalid usage row",
        "existing error text must not move"
    );
    assert_eq!(
        prepare_ledger_row("user-alice", "track-1", "some-key", 0, 0, 1_767_225_600),
        Err(LedgerError::Invalid),
        "zero-byte rows must still fail closed"
    );
    assert_eq!(
        prepare_ledger_row(
            "user-alice",
            "track-1",
            "some-key",
            48_000,
            2,
            1_767_225_600
        ),
        Err(LedgerError::Invalid),
        "seconds disagreeing with bytes must still fail closed"
    );
}

#[test]
fn existing_service_unavailable_shape_is_the_503_caller_mapping() {
    // The shape a caller emits for LedgerError::RandomnessUnavailable
    // already exists and must not move: registered 503, retryable, and
    // the 5xx message sealed to fixed public text (never the caller's
    // internal detail).
    let err = ApiError::service_unavailable("secure randomness unavailable");
    assert_eq!(err.status, 503);
    assert_eq!(err.code, "service_unavailable");
    assert!(err.retryable, "503 service_unavailable must stay retryable");
    assert_eq!(
        err.body_json(),
        r#"{"code":"service_unavailable","message":"service unavailable","retryable":true}"#,
        "the 503 wire shape must stay byte-identical"
    );
}
