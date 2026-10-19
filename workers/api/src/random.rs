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
//! [`crate::ledger`] is intentionally untouched: that module's
//! `RandomnessUnavailable` path (PR #516) can adopt this helper on rebase
//! with `secure_random_bytes::<16>().map_err(|_| LedgerError::..)?`.

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
    fn strip_code_for_propagation_pin(source: &str) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::with_capacity(source.len());
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            // Raw string `r#"..."#` / `r"..."`: detect `r` + hashes + `"`.
            if c == 'r' && (next == Some('"') || next == Some('#')) {
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
                    continue;
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
                continue;
            }
            if c == '\'' {
                // Char literal on one line (at most 4 chars wide incl.
                // quotes): blank its body so a `?` inside cannot satisfy
                // the pin. Lifetimes fall through as plain code.
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
                if closed && j.saturating_sub(i) <= 4 {
                    out.push('\'');
                    i += 1;
                    while i < chars.len() && chars[i] != '\'' {
                        if chars[i] == '\n' {
                            out.push('\n');
                        } else {
                            out.push(' ');
                        }
                        i += 1;
                    }
                    if i < chars.len() {
                        out.push('\'');
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
    fn check_secure_random_propagation(source: &str) -> Result<(), String> {
        // Build needles at runtime so this scanner contributes no raw
        // `getrandom::` text of its own (see module note).
        let gr = "getrandom";
        let fill_needle = format!("{gr}::fill");
        let call_needle = format!("{gr}::");
        let discard_needle = format!("let _ = {gr}");
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
            if !(stmt.contains(".map_err")
                && stmt.contains("RandomnessUnavailable")
                && stmt.contains('?'))
            {
                return Err(format!(
                    "getrandom statement must map failure to RandomnessUnavailable with ?: {stmt}"
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
        let gr = "getrandom";
        let mutant = format!(
            "let _filled = {gr}::fill(&mut bytes); let _note = \".map_err(|_| CryptoError::RandomnessUnavailable)?;\";"
        );
        assert!(
            check_secure_random_propagation(&mutant).is_err(),
            "string-literal needles must not satisfy the pin"
        );
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
