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
        // Strip `//` line comments (incl. `///` / `//!` docs) so prose
        // about the old `.expect(..)` spelling cannot satisfy the pin.
        let mut code = String::with_capacity(production.len());
        for line in production.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            code.push_str(line);
            code.push('\n');
        }
        let norm = code.split_whitespace().collect::<Vec<_>>().join(" ");
        // Anti-vacuity: the helper must still draw from the OS CSPRNG.
        assert!(
            norm.contains("getrandom::fill"),
            "helper must still draw randomness via getrandom::fill",
        );
        // No discarding spelling: `let _ =` would serve zero buffers.
        assert!(
            !norm.contains("let _ = getrandom"),
            "getrandom result must be propagated, never discarded with let _ =",
        );
        for (idx, _) in norm.match_indices("getrandom::") {
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
                assert!(
                    !stmt.contains(banned),
                    "getrandom statement must propagate, found banned {banned}: {stmt}",
                );
            }
            assert!(
                stmt.contains(".map_err")
                    && stmt.contains("RandomnessUnavailable")
                    && stmt.contains('?'),
                "getrandom statement must map failure to RandomnessUnavailable with ?: {stmt}",
            );
        }
    }
}
