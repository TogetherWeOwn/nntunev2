//! Acceptance contract: server-side indexer key injection must honor the
//! saved-key contract the vault and the setup wizard already enforce — a
//! nonempty indexer API key of at most 1024 UTF-8 **bytes**.
//!
//! Contract under test (public seam only: [`build_search_url`],
//! [`inject_api_key`], [`redact_api_key`] in `workers/ingest/src/newznab.rs`):
//!
//! - Keys of 1..=1024 UTF-8 bytes inject successfully, percent-encoded.
//!   The 256-, 257- and 1024-byte boundaries all succeed; 1025 bytes fail.
//!   (Before the fix, injection rejected every key over 256 bytes, so a
//!   saved 257-byte key could never be used.)
//! - The bound counts UTF-8 bytes, not chars/scalars: multibyte keys at
//!   exactly 1024 bytes succeed and multibyte keys over 1024 bytes fail
//!   even when their char count is far below 1024.
//! - Empty keys are refused.
//! - Rejection diagnostics are generic length diagnostics: neither the raw
//!   key nor its percent-encoded form (nor any distinctive fragment of
//!   either) appears in the error.
//! - Redacted URLs never contain the raw or encoded key, for short and
//!   long keys alike; redaction stays global and case-insensitive.
//! - Keys of <=256 bytes keep their existing behavior.
//!
//! All fixtures are bounded synthetic strings; no network, no real
//! credentials. Tests-only: no `src/` change belongs on this branch.

use nntune_ingest::newznab::{build_search_url, inject_api_key, redact_api_key, SearchParams};

/// A keyless search URL built through the public seam, as production builds it.
fn bare_url() -> String {
    build_search_url(&SearchParams {
        base_url: "https://indexer.example",
        query: "x",
        categories: &[],
        limit: 25,
        offset: 0,
    })
    .expect("fixture search params must build")
}

/// Assert `key` (whose UTF-8 length must be exactly `bytes`) injects and the
/// full encoded key rides as the `apikey=` value — no truncation, no raw
/// multibyte/reserved bytes leaking into the URL structure.
fn assert_injects(key: &str, bytes: usize, encoded: &str) {
    assert_eq!(key.len(), bytes, "fixture must be exactly {bytes} bytes");
    let bare = bare_url();
    let full = inject_api_key(&bare, key)
        .unwrap_or_else(|e| panic!("{bytes}-byte key must inject, got error: {e}"));
    assert_eq!(
        full,
        format!("{bare}&apikey={encoded}"),
        "{bytes}-byte key must append fully percent-encoded"
    );
}

/// Assert `key` is refused with a generic diagnostic containing neither the
/// raw key, nor its encoded form, nor the distinctive `marker` fragment.
fn assert_refused_without_leaking(key: &str, encoded: &str, marker: &str) {
    let err = inject_api_key(&bare_url(), key).expect_err("over-contract key must be refused");
    assert!(!err.is_empty(), "refusal must carry a diagnostic");
    assert!(
        !err.contains(marker),
        "diagnostic must not contain key material ({marker:?}): {err}"
    );
    assert!(
        !err.contains(key),
        "diagnostic must not contain the raw key: {err}"
    );
    assert!(
        !err.contains(encoded),
        "diagnostic must not contain the encoded key: {err}"
    );
}

#[test]
fn ascii_keys_at_256_257_and_1024_bytes_inject_and_1025_fails() {
    // 256 bytes: the old ceiling — behavior must stay intact.
    assert_injects(&"k".repeat(256), 256, &"k".repeat(256));
    // 257 bytes: a key the vault/UI happily save; injection must honor it.
    assert_injects(&"k".repeat(257), 257, &"k".repeat(257));
    // 1024 bytes: the saved-key contract ceiling.
    assert_injects(&"k".repeat(1024), 1024, &"k".repeat(1024));
    // 1025 bytes: one past the contract — refused, generically.
    let over = "k".repeat(1025);
    assert_eq!(over.len(), 1025);
    assert_refused_without_leaking(&over, &over, &"k".repeat(64));
}

#[test]
fn multibyte_keys_are_bounded_by_utf8_bytes_not_chars() {
    // U+00E9 is 2 UTF-8 bytes (%C3%A9 encoded): 512 of them are exactly
    // 1024 bytes but only 512 chars.
    let exact = "\u{e9}".repeat(512);
    assert_eq!(exact.chars().count(), 512);
    assert_injects(&exact, 1024, &"%C3%A9".repeat(512));

    // 513 of them are 1026 bytes / 513 chars. A char-count bound would
    // wrongly accept this; the byte contract must refuse it.
    let over = "\u{e9}".repeat(513);
    assert_eq!(over.len(), 1026);
    assert_eq!(over.chars().count(), 513);
    assert_refused_without_leaking(&over, &"%C3%A9".repeat(513), "\u{e9}\u{e9}\u{e9}");

    // 4-byte scalars: 256 emoji are exactly 1024 bytes / 256 chars.
    let emoji_exact = "\u{1f600}".repeat(256);
    assert_eq!(emoji_exact.chars().count(), 256);
    assert_injects(&emoji_exact, 1024, &"%F0%9F%98%80".repeat(256));

    // Mixed ASCII + multibyte straddling the boundary at the last scalar:
    // 1022 ASCII bytes + one 2-byte scalar = 1024 (accept);
    // 1023 ASCII bytes + one 2-byte scalar = 1025 (refuse).
    let mixed_exact = format!("{}\u{e9}", "a".repeat(1022));
    assert_injects(&mixed_exact, 1024, &format!("{}%C3%A9", "a".repeat(1022)));
    let mixed_over = format!("{}\u{e9}", "a".repeat(1023));
    assert_eq!(mixed_over.len(), 1025);
    assert_refused_without_leaking(
        &mixed_over,
        &format!("{}%C3%A9", "a".repeat(1023)),
        &"a".repeat(64),
    );
}

#[test]
fn empty_key_is_refused() {
    assert!(inject_api_key(&bare_url(), "").is_err());
}

#[test]
fn short_key_behavior_and_percent_encoding_are_unchanged() {
    // The pre-existing <=256-byte path: unreserved bytes stay bare.
    assert_injects("test-api-key-1", 14, "test-api-key-1");
    // Reserved characters are percent-encoded so key bytes can never break
    // out of the apikey= value into surrounding query structure.
    assert_injects("a&b=c d+e/f?g#h%i", 17, "a%26b%3Dc%20d%2Be%2Ff%3Fg%23h%25i");
    // A single-byte key is the nonempty floor.
    assert_injects("k", 1, "k");
}

#[test]
fn long_key_percent_encoding_is_retained_not_raw() {
    // A >256-byte key built from a reserved char: every byte must be
    // encoded ('&' -> %26); a raw '&' would split the query.
    let key = "&".repeat(300);
    let bare = bare_url();
    let full = inject_api_key(&bare, &key).expect("300-byte key must inject");
    assert_eq!(full, format!("{bare}&apikey={}", "%26".repeat(300)));
    assert_eq!(
        full.matches('&').count(),
        bare.matches('&').count() + 1,
        "only the apikey separator may be a raw '&'"
    );
}

#[test]
fn redacted_urls_never_contain_raw_or_encoded_key() {
    // Short key (existing behavior) and 1024-byte key (new contract),
    // including a multibyte key whose raw and encoded forms differ.
    for (key, encoded) in [
        ("test-api-key-1".to_string(), "test-api-key-1".to_string()),
        ("k".repeat(1024), "k".repeat(1024)),
        ("\u{e9}".repeat(512), "%C3%A9".repeat(512)),
    ] {
        let bare = bare_url();
        let full = inject_api_key(&bare, &key).expect("in-contract key must inject");
        // Also with a trailing parameter after the key.
        for candidate in [full.clone(), format!("{full}&other=1")] {
            let redacted = redact_api_key(&candidate);
            assert!(
                !redacted.contains(&key),
                "redacted URL must not contain the raw key"
            );
            assert!(
                !redacted.contains(&encoded),
                "redacted URL must not contain the encoded key"
            );
            assert!(redacted.contains("apikey=***"), "got: {redacted}");
            assert!(
                redacted.starts_with(&format!("{bare}&apikey=***")),
                "everything before the key value is preserved: {redacted}"
            );
        }
        let with_tail = redact_api_key(&format!("{full}&other=1"));
        assert!(
            with_tail.ends_with("&other=1"),
            "parameters after the key survive redaction: {with_tail}"
        );
    }
}

#[test]
fn redaction_stays_global_and_case_insensitive() {
    let redacted =
        redact_api_key("https://i.example/api?APIKEY=FIRSTKEY&q=x&ApiKey=SECONDKEY&apikey=THIRD");
    assert!(!redacted.contains("FIRSTKEY"));
    assert!(!redacted.contains("SECONDKEY"));
    assert!(!redacted.contains("THIRD"));
    assert_eq!(redacted.matches("***").count(), 3);
    assert!(redacted.contains("q=x"));
    // Parameter names keep their original case; only values are scrubbed.
    assert!(redacted.contains("APIKEY=***"));
    assert!(redacted.contains("ApiKey=***"));
}

#[test]
fn rejection_diagnostic_for_distinctive_long_key_leaks_nothing() {
    // Distinctive synthetic marker repeated past the ceiling: no fragment
    // of it, raw or encoded, may surface in the refusal.
    let key = "synthetic-marker-".repeat(61); // 17 * 61 = 1037 bytes
    assert_eq!(key.len(), 1037);
    // '-' and alphanumerics are unreserved, so encoded == raw here; the
    // marker fragment check is the discriminating assertion.
    assert_refused_without_leaking(&key, &key, "synthetic-marker-");
}
