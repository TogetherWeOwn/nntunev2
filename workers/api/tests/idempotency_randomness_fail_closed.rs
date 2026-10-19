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
//! - The mint body draws entropy through the shared helper and
//!   propagates its failure, verbatim in shape (no direct
//!   `getrandom::` in `ledger.rs`; the sole fill lives in
//!   `crate::random`):
//!
//!   ```ignore
//!   pub fn new_idempotency_key() -> Result<String, LedgerError> {
//!       let bytes = crate::random::secure_random_bytes::<16>()
//!           .map_err(|_| LedgerError::RandomnessUnavailable)?;
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
//! shim below, (b) the mint's sole helper call
//! (`crate::random::secure_random_bytes::<16>()`, no direct
//! `getrandom::` in `ledger.rs`) propagating with `map_err(...)?` to
//! the dedicated variant, and (c) a ban on every
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
//! - Ignoring the error (`let _ = secure_random_bytes::<16>()`) or
//!   encoding a zero buffer anyway drops the `map_err(...)?`
//!   propagation pin and adds a second (or loses the only) `Ok(` in
//!   the body; restoring a direct `getrandom::fill` breaks the
//!   no-direct-getrandom count pins.
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

// --- Spelling-insensitive scan helpers ---------------------------------------
//
// The scanners below never match raw source text. They first blank comments
// and string/char/raw-string contents with `strip_code_for_scan` (mirrored
// from `strip_code_for_propagation_pin` in `src/random.rs`; tests cannot
// reach that `#[cfg(test)]` helper, so keep the copies in sync), then anchor
// on the bare `getrandom` word token. That makes the scans insensitive to
// fmt-clean rewrites a raw `getrandom::` needle misses: a crate alias
// (`use getrandom as gr; gr::fill(..)`), whitespace or a block comment
// around the path separator (`getrandom /* entropy */ ::fill(..)`), and a
// leading `::`. Statement windows are located on the stripped text, so a
// `;` inside a string no longer terminates a statement early. Prose on
// comment lines and inside strings stays invisible, as before.

/// Identifier-continue for literal suffixes and raw identifiers
/// (conservative XID-Continue: non-ASCII in code outside strings and
/// comments only occurs in identifiers, and `is_alphanumeric` misses
/// XID-Continue chars like U+203F and U+0301 that rustc eats as suffix).
fn ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric() || !c.is_ascii()
}

/// Identifier-start for raw identifiers (`r#ident`, possibly multi-`#`).
fn ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic() || !c.is_ascii()
}

/// True when the `r` at `chars[i]` starts a raw-string prefix at a token
/// boundary (mirrors `rustc_lexer`): the maximal identifier-char run
/// ending at this `r` must be exactly `r`, `br` or `cr` — a digit-led run
/// like `1r` is excluded by the exact-run rule — and the char before that
/// run must not be a quote, so a literal suffix (`"a"r`, `'a'r`) never
/// counts as a raw prefix. The run walk uses [`ident_continue`] so
/// XID-Continue suffixes (`"a"e<U+203F>r`) are read as one suffix, not
/// as a bare `r`. Hash-closed raw suffixes (`r#"a"#r`) and raw
/// identifiers (`r#r`) are handled by forward token consumption in the
/// stripper, not by this backward check.
fn is_raw_prefix_at(chars: &[char], i: usize) -> bool {
    let mut run_start = i;
    while run_start > 0 {
        let prev = chars[run_start - 1];
        if ident_continue(prev) {
            run_start -= 1;
        } else {
            break;
        }
    }
    let run: String = chars[run_start..=i].iter().collect();
    if run != "r" && run != "br" && run != "cr" {
        return false;
    }
    if run_start > 0 {
        let before = chars[run_start - 1];
        if before == '"' || before == '\'' {
            return false;
        }
    }
    true
}

fn strip_code_for_scan(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Raw string `r#"..."#` / `r"..."` (and `br`/`cr` forms): `r` +
        // hashes + `"` only at a token boundary, so a literal suffix `r`
        // never blanks a following plain string with raw rules.
        if c == 'r' && (next == Some('"') || next == Some('#')) && is_raw_prefix_at(&chars, i) {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while chars.get(j).copied() == Some('#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j).copied() == Some('"') {
                for ch in chars.iter().take(j + 1).skip(i) {
                    out.push(*ch);
                }
                i = j + 1;
                // Blank the raw-string body; preserve newlines.
                loop {
                    if i >= chars.len() {
                        break;
                    }
                    if chars[i] == '"'
                        && (0..hashes).all(|h| chars.get(i + 1 + h).copied() == Some('#'))
                    {
                        out.push('"');
                        for h in 0..hashes {
                            out.push(chars[i + 1 + h]);
                        }
                        i += 1 + hashes;
                        break;
                    }
                    if chars[i] == '\n' {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 1;
                }
                // Consume a following literal suffix (`r#"a"#r`) as part of
                // this literal, so its trailing `r` never re-enters the
                // raw branch with `#` before it.
                while i < chars.len() && ident_continue(chars[i]) {
                    out.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            // Raw identifier `r#ident` (possibly multi-`#`): consume `r` +
            // hashes + identifier as one token, so the identifier's
            // trailing `r` (as in `r#r`) never re-enters the raw branch.
            // Only bare `r` starts a raw identifier; `br`/`cr` + `#` is an
            // identifier plus `#` and correctly falls through.
            if hashes >= 1 && chars.get(j).is_some_and(|&d| ident_start(d)) {
                let mut run_start = i;
                while run_start > 0 && ident_continue(chars[run_start - 1]) {
                    run_start -= 1;
                }
                let run: String = chars[run_start..=i].iter().collect();
                if run == "r" {
                    while i < j {
                        out.push(chars[i]);
                        i += 1;
                    }
                    while i < chars.len() && ident_continue(chars[i]) {
                        out.push(chars[i]);
                        i += 1;
                    }
                    continue;
                }
            }
        }
        if c == '/' && next == Some('/') {
            out.push(' ');
            out.push(' ');
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            out.push(' ');
            out.push(' ');
            i += 2;
            let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                let d = chars[i];
                let dn = chars.get(i + 1).copied();
                if d == '/' && dn == Some('*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '*' && dn == Some('/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            continue;
        }
        if c == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if d == '"' {
                    out.push('"');
                    i += 1;
                    break;
                }
                if d == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            // Consume a following literal suffix (`"a"e<U+203F>r`) as part
            // of this literal, so its trailing `r` never re-enters the raw
            // branch.
            while i < chars.len() && ident_continue(chars[i]) {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if c == '\'' {
            // Char literal on one line: blank its body so a `?` inside
            // cannot satisfy the scan. Non-escape bodies are exactly one
            // character (quotes exactly 2 apart); a 2-3 char body is
            // never a valid char, only a lifetime/label quote reaching
            // a later quote (e.g. `break 'a "'...'"` spans exactly 4).
            // Escape-led bodies (`'\..'`) are accepted at any width
            // because lifetimes never contain a backslash. Lifetimes
            // and labels fall through as plain code.
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() && chars[j] != '\n' {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == '\'' {
                    closed = true;
                    break;
                }
                j += 1;
            }
            let escape_led = chars.get(i + 1).copied() == Some('\\');
            if closed && (j.saturating_sub(i) == 2 || escape_led) {
                // Blank via the escape-aware lookahead endpoint `j`:
                // rescanning for the first `'` would stop at an escaped
                // quote (`'\''`) and reinterpret the real closer as a new
                // char start, leaking a following string as code.
                out.push('\'');
                i += 1;
                while i < j {
                    if chars[i] == '\n' {
                        out.push('\n');
                    } else {
                        out.push(' ');
                    }
                    i += 1;
                }
                out.push('\'');
                i += 1;
                // Consume a following literal suffix as part of this char
                // literal, mirroring the string branches.
                while i < chars.len() && ident_continue(chars[i]) {
                    out.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Byte offsets of every whole-word occurrence of `word` in `haystack`:
/// the neighbouring characters (if any) must not continue an identifier,
/// so `my_getrandom_x` never matches but `::getrandom`, `r#getrandom` and
/// a bare `getrandom` in a `use` all do.
fn word_indices(haystack: &str, word: &str) -> Vec<usize> {
    let chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let want: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    if want.is_empty() || chars.len() < want.len() {
        return out;
    }
    for i in 0..=chars.len() - want.len() {
        if (0..want.len()).all(|k| chars[i + k].1 == want[k]) {
            let before_ok = i == 0 || !ident_continue(chars[i - 1].1);
            let after = i + want.len();
            let after_ok = after >= chars.len() || !ident_continue(chars[after].1);
            if before_ok && after_ok {
                out.push(chars[i].0);
            }
        }
    }
    out
}

/// Whole-word `getrandom` tokens in the stripped code of `source`.
fn getrandom_word_count(source: &str) -> usize {
    word_indices(&strip_code_for_scan(source), "getrandom").len()
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
            "crate::random::secure_random_bytes::<16>() \
             .map_err(|_| LedgerError::RandomnessUnavailable)?;"
        ),
        "entropy must come from the shared helper and its failure must \
         propagate as LedgerError::RandomnessUnavailable via \
         map_err(...)?, body was: {body}"
    );
    assert!(
        !body.contains("[0u8;"),
        "the helper owns the entropy buffer: no local zero buffer may be \
         encoded on any path, body was: {body}"
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
        body.matches("secure_random_bytes").count(),
        1,
        "the mint must draw entropy exactly once, body was: {body}"
    );
    // Spelling-insensitive counts: the stripped whole-word token catches
    // a crate alias (`use getrandom as gr;`), a comment/whitespace-
    // separated path (`getrandom /* entropy */ ::fill`) and a leading
    // `::`, all of which a raw `getrandom::` needle misses, while prose
    // in comments/strings cannot false-positive.
    assert_eq!(
        getrandom_word_count(&body),
        0,
        "the mint must not bypass the shared helper with a direct fill \
         under any spelling, body was: {body}"
    );
    assert_eq!(
        getrandom_word_count(LEDGER_SRC),
        0,
        "ledger.rs must contain no getrandom token under any spelling \
         (sole fill is the shared helper in random.rs)"
    );
    // Scanner self-checks for those families, on synthetic sources.
    for src in [
        "use getrandom as gr;\ngr::fill(&mut b).expect(\"probe\");",
        "getrandom /* entropy */ ::fill(&mut b).expect(\"probe\");",
        "::getrandom::fill(&mut b).expect(\"probe\");",
        "getrandom :: fill (&mut b).expect(\"probe\");",
    ] {
        assert!(getrandom_word_count(src) > 0, "must count: {src}");
    }
    for src in [
        "// getrandom::fill(&mut b).expect(\"gone\");",
        "/* getrandom::fill(&mut b).expect(\"gone\"); */",
        "let s = \"getrandom::fill(&mut b)\";",
        "let my_getrandom_x = 1;",
    ] {
        assert_eq!(getrandom_word_count(src), 0, "must ignore: {src}");
    }
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
