//! Shared fail-closed secure randomness for the API worker.
//!
//! All request-serving randomness in this crate flows through
//! [`secure_random_bytes`]: one audited `getrandom` call site whose failure
//! propagates as [`CryptoError::RandomnessUnavailable`] (HTTP 503
//! `service_unavailable`, retryable) instead of panicking. Callers map the
//! error into their module error (each of which renders the existing
//! `service_unavailable` shape) with `?` — never `.expect(..)`, never
//! `.unwrap(..)`, never a fallback or reused key.
//!
//! The success path is byte-identical to the previous per-module fills: the
//! same `N` entropy bytes from the same `getrandom` backend, encoded by the
//! caller exactly as before.
//!
//! [`crate::ledger`]'s `new_idempotency_key` also mints through this
//! helper, mapping failure to `LedgerError::RandomnessUnavailable`
//! (HTTP 503 `service_unavailable`).

use crate::api_error::ApiError;

/// Secure-randomness failure. Exactly one variant today: the OS RNG was
/// unavailable while minting request-serving key material. Never a fallback
/// key, never a reused key — the request fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// `getrandom` failed: HTTP 503 `service_unavailable` (retryable).
    RandomnessUnavailable,
}

impl CryptoError {
    /// Stable registry code for the structured error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            CryptoError::RandomnessUnavailable => "service_unavailable",
        }
    }

    /// HTTP status for the envelope.
    pub fn http_status(&self) -> u16 {
        match self {
            CryptoError::RandomnessUnavailable => 503,
        }
    }
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Sealed public text: matches the `service_unavailable` envelope
        // message, carries no OS or crypto internals.
        match self {
            CryptoError::RandomnessUnavailable => write!(f, "service unavailable"),
        }
    }
}

impl From<CryptoError> for ApiError {
    fn from(_: CryptoError) -> Self {
        // `by_code` seals 5xx messages to fixed public text.
        Self::service_unavailable("service unavailable")
    }
}

/// Fill `N` bytes from the OS CSPRNG, failing closed on unavailability.
///
/// Success is byte-identical to the historical per-module
/// `getrandom::fill(&mut buf)` fills: same backend, same `N` bytes, no
/// transformation. Failure returns [`CryptoError::RandomnessUnavailable`]
/// so the caller fails the request closed with the existing 503
/// `service_unavailable` shape.
pub fn secure_random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_returns_n_bytes_on_success() {
        let bytes = secure_random_bytes::<16>().expect("rng available in tests");
        assert_eq!(bytes.len(), 16);
        let empty: [u8; 0] = secure_random_bytes::<0>().expect("zero-length fill");
        assert!(empty.is_empty());
        let big = secure_random_bytes::<32>().expect("rng available in tests");
        assert_eq!(big.len(), 32);
    }

    #[test]
    fn randomness_unavailable_maps_to_503_service_unavailable() {
        let err = CryptoError::RandomnessUnavailable;
        assert_eq!(err.code(), "service_unavailable");
        assert_eq!(err.http_status(), 503);
        assert_eq!(err.to_string(), "service unavailable");
        let api = ApiError::from(err);
        assert_eq!(
            (api.code, api.status, api.retryable),
            ("service_unavailable", 503, true)
        );
        assert_eq!(api.message, "service unavailable");
        assert_eq!(crate::api_error::spec("service_unavailable").status, 503);
    }

    /// Structural pin: the single audited `getrandom` call site must
    /// propagate failure as `RandomnessUnavailable` — never panic
    /// (`.expect(`/unwrap spellings), never discard the result
    /// (`let _ =` / `.ok()`), which would serve zero-filled key material
    /// as constant tokens, session ids, vault DEK and nonces. OS RNG
    /// failure cannot be induced portably in a unit test, so this source
    /// shape is the regression protection for the fail-closed claim
    /// (complementing the paired `randomness_fail_closed_nine` suite,
    /// which scans the six caller files).
    ///
    /// Only real code satisfies the pin: line comments, block comments
    /// (nesting-aware) and string/char/raw-string contents are stripped
    /// before scanning, so needles carried by comments or strings cannot
    /// fabricate propagation evidence.
    ///
    /// NOTE (paired-suite false-positive avoidance): this module lives
    /// under `src/`, so the crate-wide `EXPECT_BAN` scan sees it. The
    /// scanner below builds its `getrandom` needles at runtime via
    /// concatenation, so this file contributes no additional raw
    /// `getrandom::` occurrences beyond production; synthetic mutant
    /// sources are likewise built with `format!`, keeping their banned
    /// spellings outside any scanned statement window.
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

    fn strip_code_for_propagation_pin(source: &str) -> String {
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
                // cannot satisfy the pin. Non-escape bodies are exactly one
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

    /// Check a production-source slice for fail-closed propagation.
    /// Returns `Ok(())` when every audited call propagates, `Err(reason)`
    /// otherwise. Takes the source as a parameter (mirroring
    /// `getrandom_offenders` in the paired suite) so self-tests can pin
    /// comment/string-carried mutants without touching production.
    ///
    /// The positive check is an EXACT statement match, not a substring
    /// search: any combinator that swallows the mapped error (`.or(..)`,
    /// `.or_else(..)`, `.ok()`, ...) changes the statement text and is
    /// rejected. The banned list below stays for precise diagnostics only.
    fn check_secure_random_propagation(source: &str) -> Result<(), String> {
        // Build needles at runtime so this scanner contributes no raw
        // `getrandom::` text of its own (see module note).
        let gr = "getrandom";
        let fill_needle = format!("{gr}::fill");
        let call_needle = format!("{gr}::");
        let discard_needle = format!("let _ = {gr}");
        let fill_call = format!("{fill_needle}(&mut bytes)");
        let expected = format!("{fill_call}.map_err(|_| CryptoError::RandomnessUnavailable)?;");
        let stripped = strip_code_for_propagation_pin(source);
        let norm = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        if !norm.contains(&fill_needle) {
            return Err("helper must still draw randomness via getrandom::fill".into());
        }
        if norm.contains(&discard_needle) {
            return Err("getrandom result must be propagated, never discarded with let _ =".into());
        }
        let mut found = false;
        let mut search_from = 0usize;
        while let Some(rel) = norm[search_from..].find(&call_needle) {
            let idx = search_from + rel;
            let end = norm[idx..]
                .find(';')
                .map_or(norm.len(), |semi| idx + semi + 1);
            let stmt = &norm[idx..end];
            for banned in [
                ".expect(",
                ".unwrap(",
                ".unwrap_or(",
                ".unwrap_or_else(",
                ".unwrap_or_default(",
                ".ok()",
            ] {
                if stmt.contains(banned) {
                    return Err(format!(
                        "getrandom statement must propagate, found banned {banned}: {stmt}"
                    ));
                }
            }
            if *stmt != *expected {
                return Err(format!(
                    "getrandom statement must be exactly {expected}: {stmt}"
                ));
            }
            found = true;
            search_from = end;
        }
        if !found {
            return Err("no audited getrandom:: call found".into());
        }
        Ok(())
    }

    #[test]
    fn helper_propagates_randomness_failure() {
        // Slice ONLY the pre-test production source first: the banned
        // needles below appear verbatim in this test's own literals, so
        // searching the whole file would match the test itself.
        let source = include_str!("random.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("test module must exist")
            .0;
        check_secure_random_propagation(production)
            .expect("helper must propagate getrandom failure as RandomnessUnavailable");
    }

    #[test]
    fn pin_rejects_block_comment_carried_needles() {
        // Round-2 mutant: named discard with the positive needles present
        // only inside a block comment. Must fail even though the raw text
        // contains `.map_err`, `RandomnessUnavailable` and `?`.
        let gr = "getrandom";
        let mutant = format!(
            "let _filled = {gr}::fill(&mut bytes) /* .map_err(|_| CryptoError::RandomnessUnavailable)?; */;"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "block-comment needles must not satisfy the pin"
        );
    }

    #[test]
    fn pin_rejects_trailing_comment_carried_needles() {
        let gr = "getrandom";
        let mutant = format!(
            "let _filled = {gr}::fill(&mut bytes) // .map_err(|_| CryptoError::RandomnessUnavailable)?;"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "trailing-comment needles must not satisfy the pin"
        );
    }

    #[test]
    fn pin_rejects_string_carried_needles() {
        // Needles live INSIDE the audited statement (tuple fabrication):
        // only string stripping can hide them. A separate-statement
        // fixture would be vacuous because statement slicing already
        // excludes it even with no stripping at all.
        let gr = "getrandom";
        let mutant = format!(
            "let _pair = ({gr}::fill(&mut bytes), \".map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "string-literal needles must not satisfy the pin"
        );
    }

    #[test]
    fn pin_rejects_raw_string_carried_needles() {
        // Raw-string body carries a `"` before the needles, so the
        // plain-string branch alone cannot backstop a deleted raw branch.
        // Built with escaped quotes (no raw literal in test source) and
        // the fill needle via runtime concat (see module note).
        let gr = "getrandom";
        let q = '"';
        let mutant = format!(
            "let _pair = ({gr}::fill(&mut bytes), r#{q}a{q}b .map_err(|_| CryptoError::RandomnessUnavailable)?;{q}#);"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "raw-string needles must not satisfy the pin"
        );
    }

    #[test]
    fn pin_rejects_char_carried_question_mark() {
        // `map_err` + `RandomnessUnavailable` are real code; the only `?`
        // is a char literal, so only char stripping rejects it.
        let gr = "getrandom";
        let mutant = format!(
            "let _t = ({gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable), '?');"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "char-literal ? must not satisfy the pin"
        );
    }

    #[test]
    fn pin_rejects_swallowed_mapped_error() {
        // Mapped error followed by a swallowing combinator: all positive
        // substrings are present in real code, yet RNG failure yields
        // zero-filled key material.
        let gr = "getrandom";
        let mutant = format!(
            "{gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable).or(Ok(()))?;"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "swallowed mapped error must not satisfy the pin"
        );
    }

    #[test]
    fn stripper_blanks_plain_string_needles() {
        // Direct branch pin: deleting the `"` string branch of
        // `strip_code_for_propagation_pin` must turn THIS test red (the
        // checker-level tuple test above stays red either way under the
        // exact-shape rule, so it cannot pin the branch by itself).
        let src = "let _note = \".map_err(|_| CryptoError::RandomnessUnavailable)?;\";";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_raw_string_with_inner_quote() {
        // Body holds a `"` before the needles: with the raw branch
        // deleted, the plain-string branch blanks only up to the inner
        // quote and leaves the needles visible, so this goes red.
        let src = "let _note = r#\"a\"b .map_err(|_| CryptoError::RandomnessUnavailable)?;\"#;";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "raw-string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "raw-string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "raw-string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_char_question_mark() {
        let src = "let _q = '?';";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains('?'),
            "char ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_line_comment_needles() {
        // Direct pin for the `//` branch: the checker-level trailing
        // test is subsumed by the exact-shape rule, so this unit test
        // is what turns red if line-comment stripping breaks.
        let src = "let x = 1; // .map_err(|_| CryptoError::RandomnessUnavailable)?;";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "line-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "line-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "line-comment ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_block_comment_needles() {
        // Direct pin for the `/* */` branch (same rationale as above).
        let src = "let x = 1; /* .map_err(|_| CryptoError::RandomnessUnavailable)?; */";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "block-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "block-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "block-comment ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_escaped_quote_in_plain_string() {
        // Direct pin for the plain-string escape skip (`\\` at the `"`
        // branch): the body holds an escaped quote before the needles. With
        // the skip disabled, the escaped quote ends the string early and the
        // needles leak as code.
        let src = "let _note = \"a\\\"b .map_err(|_| CryptoError::RandomnessUnavailable)?;\";";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "escaped-quote string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "escaped-quote string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "escaped-quote string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_nested_block_comment_needles() {
        // Direct pin for block-comment nesting (`depth += 1`): needles sit
        // after the inner close but before the outer close. Without nesting
        // the inner close ends the comment early and the needles leak.
        let src =
            "let x = 1; /* outer /* inner */ .map_err(|_| CryptoError::RandomnessUnavailable)?; */";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "nested block-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "nested block-comment body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "nested block-comment ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_double_hash_raw_string_with_inner_hash() {
        // Direct pin for raw-string hash counting: an `r##` body holding a
        // quote-plus-one-hash (`"#`) must not close early. Capping hashes at
        // one would end at the inner `"#` and leak the needles.
        let src = "let _note = r##\"a\"#b .map_err(|_| CryptoError::RandomnessUnavailable)?;\"##;";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "double-hash raw-string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "double-hash raw-string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "double-hash raw-string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_escaped_quote_char_before_string() {
        // Direct pin for the char escape-consistency sub-branch: an
        // escaped-quote char `'\''` followed by a quote-led string whose body
        // opens with `'` (so the rescan bug reinterprets the real closer as a
        // new char start and blanks the string's opening `"` as char body,
        // leaking the needles as code). The fixed blank-via-`j` keeps both
        // the char body and the string body blanked.
        let src = "let _t = ('\\'', \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "escaped-char-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "escaped-char-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "escaped-char-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_escaped_char_adjacent_string_carried_statement() {
        // Checker-level regression for the same bug: a compilable tuple that
        // pairs `'\''` with a string carrying the exact expected statement
        // (leading `'` so the rescan bug leaks it as code). It never executes
        // `getrandom::fill` — zero-filled bytes returned as `Ok` — yet the
        // buggy stripper reports `Ok(())`.
        let gr = "getrandom";
        let mutant = format!(
            "let _t = ('\\'', \"'{gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "escaped-char-adjacent string statement must not satisfy the pin"
        );
    }

    #[test]
    fn stripper_blanks_hex_escape_char_before_string() {
        // Wide escape char (`'\x3f'`, 6 wide incl. quotes) paired with a
        // quote-led string: the old `<= 4` width guard fell through as a
        // lifetime, letting the string body leak as code.
        let src = "let _t = ('\\x3f', \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "hex-escape-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "hex-escape-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "hex-escape-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_unicode_escape_char_before_string() {
        // `'\u{3f}'` is 8 wide incl. quotes: same lifetime-fallthrough leak
        // class as the hex form.
        let src = "let _t = ('\\u{3f}', \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "unicode-escape-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "unicode-escape-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "unicode-escape-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_long_unicode_escape_char_before_string() {
        // Long unicode escape (`'\u{10FFFF}'`): widest valid char spelling.
        let src =
            "let _t = ('\\u{10FFFF}', \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "long-unicode-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "long-unicode-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "long-unicode-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn stripper_blanks_hex_byte_char_before_string() {
        // Byte char (`b'\x3f'`): the quote after `b` opens an escape-led
        // body at width 5, same fallthrough class.
        let src = "let _t = (b'\\x3f', \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "byte-char-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "byte-char-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "byte-char-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_wide_escape_adjacent_string_carried_statement() {
        // Checker-level regression for the width-guard class: compilable
        // tuples pairing each wide escape char with a quote-led string
        // carrying the exact expected statement. Each never executes
        // `getrandom::fill` — zero-filled bytes returned as `Ok` — yet the
        // guarded stripper reports `Ok(())`.
        let gr = "getrandom";
        let prefixes = ["'\\x3f'", "'\\u{3f}'", "'\\u{10FFFF}'", "b'\\x3f'"];
        for prefix in prefixes {
            let mutant = format!(
                "let _t = ({prefix}, \"'{gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
            );
            assert!(
                check_secure_random_propagation(&mutant).is_err(),
                "wide-escape {prefix} adjacent string statement must not satisfy the pin"
            );
        }
    }

    #[test]
    fn stripper_blanks_one_letter_label_before_quote_led_string() {
        // Direct pin for the non-escape char width rule: a one-letter label
        // on a break followed by a quote-led string (`break 'a "'...'`)
        // spans exactly 4 chars from the label quote to the string body's
        // leading `'`. An at-most-4 guard misreads that span as a char
        // literal, blanking the label letter, the space and the string's
        // opening `"` as char body, so the string body leaks as code.
        let src = "break 'a \"' .map_err(|_| CryptoError::RandomnessUnavailable)?;\";";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "label-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "label-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "label-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_label_before_quote_led_string_carried_statement() {
        // Checker-level regression for the same bug: a labelled break whose
        // value is a quote-led string carrying the exact expected statement.
        // It never executes `getrandom::fill` — zero-filled bytes returned
        // as `Ok` — yet the at-most-4 stripper reports `Ok(())`.
        let gr = "getrandom";
        let mutant = format!(
            "'a: loop {{ break 'a \"'{gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\"; }}"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "label-adjacent string statement must not satisfy the pin"
        );
    }

    #[test]
    fn stripper_blanks_suffixed_literal_before_plain_string() {
        // Direct pin for the raw-branch token-boundary rule: `"a"r` is one
        // suffixed string literal, so the `r` is a suffix, not a raw prefix.
        // The following plain string opens with an escaped quote plus hash;
        // raw-rules blanking (no escape handling) would close at that inner
        // `"#` and leak the needles as code.
        let src =
            "stringify!(\"a\"r#\"\\\"# .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "suffix-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "suffix-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "suffix-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_suffixed_literal_string_carried_statement() {
        // Checker-level regression for the same bug: a `stringify!` line whose
        // token stream is a suffixed literal `"a"r#` plus a plain string
        // carrying the exact expected statement after an escaped quote+hash.
        // It never executes `getrandom::fill` — zero-filled bytes returned
        // as `Ok` — yet the boundary-free stripper reports `Ok(())`.
        let gr = "getrandom";
        let mutant = format!(
            "stringify!(\"a\"r#\"\\\"# {gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "suffix-adjacent string statement must not satisfy the pin"
        );
    }

    #[test]
    fn stripper_blanks_raw_identifier_before_plain_string() {
        // Direct pin for raw-identifier consumption: `r#r` is one raw-identifier
        // token, so its trailing `r` is not a raw prefix. The following plain
        // string opens with an escaped quote plus hash; raw-rules blanking
        // (no escape handling) would close at that inner `"#` and leak the
        // needles as code.
        let src = "stringify!(r#r\"\\\"# .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "raw-ident-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "raw-ident-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "raw-ident-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_raw_identifier_string_carried_statement() {
        // Checker-level regression for the same bug: a `stringify!` line whose
        // token stream is a raw identifier `r#r` plus a plain string carrying
        // the exact expected statement after an escaped quote+hash. It never
        // executes `getrandom::fill` — zero-filled bytes returned as `Ok` —
        // yet the raw-ident-blind stripper reports `Ok(())`.
        let gr = "getrandom";
        let mutant = format!(
            "stringify!(r#r\"\\\"# {gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "raw-ident-adjacent string statement must not satisfy the pin"
        );
    }

    #[test]
    fn stripper_blanks_hash_closed_raw_suffix_before_plain_string() {
        // Direct pin for literal-suffix consumption: `r#"a"#r` is a raw string
        // with suffix `r`, so the trailing `r` never re-enters the raw branch.
        // Without suffix consumption the `#`-before-`r` slips past the quote
        // guard and the following plain string is blanked with raw rules,
        // closing at its inner `"#` and leaking the needles.
        let src =
            "stringify!(r#\"a\"#r\"\\\"# .map_err(|_| CryptoError::RandomnessUnavailable)?;\");";
        let stripped = strip_code_for_propagation_pin(src);
        assert!(
            !stripped.contains(".map_err"),
            "hash-suffix-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains("RandomnessUnavailable"),
            "hash-suffix-adjacent string body must be blanked: {stripped:?}"
        );
        assert!(
            !stripped.contains('?'),
            "hash-suffix-adjacent string ? must be blanked: {stripped:?}"
        );
    }

    #[test]
    fn pin_rejects_hash_closed_raw_suffix_string_carried_statement() {
        // Checker-level regression for the same bug across `r`/`br`/`cr` raw
        // literals, each with suffix `r` plus a plain string carrying the exact
        // expected statement. None executes `getrandom::fill`.
        let gr = "getrandom";
        for prefix in ["r", "br", "cr"] {
            let mutant = format!(
                "stringify!({prefix}#\"a\"#r\"\\\"# {gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
            );
            assert!(
                check_secure_random_propagation(&mutant).is_err(),
                "{prefix}-suffix-adjacent string statement must not satisfy the pin"
            );
        }
    }

    #[test]
    fn stripper_blanks_xid_suffix_before_plain_string() {
        // Direct pin for the XID-Continue run walk: `e<U+203F>r` / `e<U+0301>r`
        // after a string is one literal suffix (rustc eats the whole run), so
        // its trailing `r` is not a raw prefix. `is_alphanumeric` is false for
        // both non-ASCII chars, so a narrow walk reads the run as exactly `r`.
        for suffix in ["\u{203f}", "\u{301}"] {
            let src = format!(
                "stringify!(\"a\"e{suffix}r\"\\\"# .map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
            );
            let stripped = strip_code_for_propagation_pin(&src);
            assert!(
                !stripped.contains(".map_err"),
                "xid-suffix-adjacent string body must be blanked: {stripped:?}"
            );
            assert!(
                !stripped.contains("RandomnessUnavailable"),
                "xid-suffix-adjacent string body must be blanked: {stripped:?}"
            );
            assert!(
                !stripped.contains('?'),
                "xid-suffix-adjacent string ? must be blanked: {stripped:?}"
            );
        }
    }

    #[test]
    fn pin_rejects_xid_suffix_string_carried_statement() {
        // Checker-level regression for the same bug: string plus XID-Continue
        // suffix plus plain string carrying the exact expected statement. It
        // never executes `getrandom::fill`.
        let gr = "getrandom";
        for suffix in ["\u{203f}", "\u{301}"] {
            let mutant = format!(
                "stringify!(\"a\"e{suffix}r\"\\\"# {gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;\");"
            );
            assert!(
                check_secure_random_propagation(&mutant).is_err(),
                "xid-suffix-adjacent string statement must not satisfy the pin"
            );
        }
    }

    #[test]
    fn pin_accepts_propagating_helper_shape() {
        let gr = "getrandom";
        let good =
            format!("{gr}::fill(&mut bytes).map_err(|_| CryptoError::RandomnessUnavailable)?;");
        assert!(
            check_secure_random_propagation(&good).is_ok(),
            "honest propagation must satisfy the pin"
        );
    }
}
