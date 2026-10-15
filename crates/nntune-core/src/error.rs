//! Crate-wide error type.

use std::{fmt, io};

/// Documented worker/API error code for stale provider credentials (S7).
/// Every worker path that observes an NNTP `481`/`482` (or an indexer HTTP
/// 401/403) maps to this code; the web app renders an inline re-auth prompt
/// on it instead of a generic error page. The code is safe to log and to
/// return in error bodies: it carries no credential material.
pub const PROVIDER_AUTH_EXPIRED_CODE: &str = "provider_auth_expired";

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure this crate can report. Transport I/O surfaces as
/// [`Error::Io`]; NNTP protocol failures carry their numeric reply code.
#[derive(Debug)]
pub enum Error {
    /// Underlying transport I/O failure (includes timeouts, which each
    /// transport implementation enforces and reports as [`io::ErrorKind::TimedOut`]).
    Io(io::Error),
    /// Unexpected NNTP reply: (numeric code, human-readable text).
    NntpReply { code: u16, text: String },
    /// Server rejected our credentials (`481`/`482`) or demands auth first.
    AuthFailed(String),
    /// Server refused the command (`502`: no permission / access denied).
    NoPermission(String),
    /// Article does not exist (`430`).
    ArticleNotFound(String),
    /// Malformed yEnc article, NZB document, PAR2 set, or ciphertext.
    Malformed(String),
    /// yEnc CRC32 mismatch after a successful structural decode.
    CrcMismatch {
        message_id: String,
        want: u32,
        got: u32,
    },
    /// An NZB/PAR2 resource bound from [`crate::nzb::Limits`] was exceeded.
    Bounds { bound: &'static str, limit: i64 },
    /// Decryption/authentication failure (bad key, bad nonce, bad padding,
    /// secretbox authentication failure, range outside the plaintext).
    Decrypt(String),
    /// PAR2 verification found damaged blocks (verify-only path, no repair).
    Par2Unhealthy { filename: String, detail: String },
    /// Operation needs a native target (e.g. spawning the `par2` repair binary).
    UnsupportedOnWasm(&'static str),
}

impl Error {
    /// Stable worker/API code for this failure. Only authentication failures
    /// map to [`PROVIDER_AUTH_EXPIRED_CODE`]; every other variant is a
    /// generic protocol error that must not trigger the re-auth prompt.
    pub fn code(&self) -> &'static str {
        match self {
            Error::AuthFailed(_) => PROVIDER_AUTH_EXPIRED_CODE,
            _ => "request_failed",
        }
    }

    /// True when the provider rejected our credentials (`481`/`482`) or
    /// demands auth first. The caller must surface
    /// [`PROVIDER_AUTH_EXPIRED_CODE`] (HTTP 401) with no credential material.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Error::AuthFailed(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "transport I/O: {e}"),
            Error::NntpReply { code, text } => write!(f, "NNTP reply {code}: {text}"),
            // Generic by construction (threat-model §3/§6: no credential
            // material in logs or error bodies). The server reply text is
            // deliberately not echoed: it is attacker-influenced and could
            // carry reflected credential material. Log/return `code()` instead.
            Error::AuthFailed(_) => write!(f, "NNTP authentication failed: {PROVIDER_AUTH_EXPIRED_CODE}"),
            Error::NoPermission(t) => write!(f, "NNTP permission denied: {t}"),
            Error::ArticleNotFound(id) => write!(f, "article not found: {id}"),
            Error::Malformed(r) => write!(f, "malformed input: {r}"),
            Error::CrcMismatch {
                message_id,
                want,
                got,
            } => write!(
                f,
                "yEnc CRC32 mismatch for segment {message_id}: want {want:08x} got {got:08x}"
            ),
            Error::Bounds { bound, limit } => {
                write!(f, "NZB/PAR2 {bound} exceeds the hard limit of {limit}")
            }
            Error::Decrypt(r) => write!(f, "decrypt: {r}"),
            Error::Par2Unhealthy { filename, detail } => {
                write!(f, "par2 verify failed for {filename}: {detail}")
            }
            Error::UnsupportedOnWasm(op) => {
                write!(f, "{op} is only available on native targets")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
