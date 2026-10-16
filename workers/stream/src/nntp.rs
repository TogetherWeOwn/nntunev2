//! Blocking NNTP client for the miss path: `BODY <message-id>` fetches over
//! one pooled connection. Blocking I/O keeps the crate dependency-free; the
//! Durable Object runs one fetch at a time per connection.
//!
//! Article bodies are raw bytes (yEnc is binary); callers get the
//! dot-unstuffed bytes with `\n` line separators.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::time::Duration;

/// Documented worker/API error code for stale provider credentials (S7).
/// Shared verbatim with `nntune-core` and `workers/api`: the web app renders
/// an inline re-auth prompt on this code. Safe to log and to return in error
/// bodies: it carries no credential material.
pub const PROVIDER_AUTH_EXPIRED_CODE: &str = "provider_auth_expired";

/// What went wrong talking to the provider.
#[derive(Debug, PartialEq, Eq)]
pub enum NntpError {
    Io(String),
    NoSuchArticle(String),
    UnexpectedResponse(String),
    /// The provider rejected our credentials, or the session is no longer
    /// authenticated (`480`/`481` on `BODY`; RFC 4643 reserves `481`/`482`
    /// for `AUTHINFO`, and a `482` on `BODY` is out-of-sequence — a generic
    /// protocol error, deliberately not mapped here).
    /// Stale credentials are a re-auth signal, not an outage: this must not
    /// quarantine the pool (see [`NntpError::is_provider_failure`]) and the
    /// play path maps it to `provider_auth_expired`, never to a missing
    /// article — but the connection itself is stale and is dropped, never
    /// parked warm for reuse. Carries no credential material.
    AuthExpired,
    /// The segment id failed RFC 5536 validation and was never sent.
    InvalidMessageId(String),
}

impl NntpError {
    /// True when the provider (or the connection to it) is unhealthy, as
    /// opposed to one article being gone (`430`/`423`), stale credentials
    /// (`480`/`481` → re-auth prompt, not an outage), or one id being
    /// malformed, which say nothing about the provider and must not
    /// quarantine the user's pool.
    pub fn is_provider_failure(&self) -> bool {
        !matches!(
            self,
            NntpError::NoSuchArticle(_) | NntpError::AuthExpired | NntpError::InvalidMessageId(_)
        )
    }

    /// Stable worker/API code for this failure. Only [`NntpError::AuthExpired`]
    /// maps to [`PROVIDER_AUTH_EXPIRED_CODE`].
    pub fn code(&self) -> &'static str {
        match self {
            NntpError::AuthExpired => PROVIDER_AUTH_EXPIRED_CODE,
            _ => "request_failed",
        }
    }
}

/// Whether `mid` (outer whitespace / `<>` already stripped) is safe to put
/// on the wire inside `BODY <…>`: RFC 5536 msg-id characters only, so a
/// hostile NZB cannot smuggle CR/LF (extra NNTP commands) or desynchronize
/// the pooled connection.
pub fn valid_message_id(mid: &str) -> bool {
    const MAX_LEN: usize = 250;
    !mid.is_empty()
        && mid.len() <= MAX_LEN
        && !mid
            .bytes()
            .any(|b| matches!(b, b'\r' | b'\n' | b'\0' | b' ' | b'\t' | b'<' | b'>'))
}

impl From<std::io::Error> for NntpError {
    fn from(e: std::io::Error) -> Self {
        NntpError::Io(e.to_string())
    }
}

pub struct NntpClient {
    reader: BufReader<std::net::TcpStream>,
}

impl NntpClient {
    /// Open a provider connection and consume the greeting.
    pub fn connect(addr: &SocketAddr) -> Result<Self, NntpError> {
        let stream = std::net::TcpStream::connect_timeout(addr, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut reader = BufReader::new(stream);
        let mut greeting = String::new();
        reader.read_line(&mut greeting)?;
        if greeting.starts_with("200") || greeting.starts_with("201") {
            Ok(Self { reader })
        } else {
            Err(NntpError::UnexpectedResponse(
                greeting.trim_end().to_string(),
            ))
        }
    }

    /// Fetch one article body; returns the raw dot-unstuffed bytes.
    /// A malformed id is rejected before anything is written, so the
    /// pooled connection can never carry an injected command.
    pub fn article(&mut self, message_id: &str) -> Result<Vec<u8>, NntpError> {
        let mid = message_id.trim().trim_matches(|c| c == '<' || c == '>');
        if !valid_message_id(mid) {
            return Err(NntpError::InvalidMessageId(message_id.to_string()));
        }
        let cmd = format!("BODY <{mid}>\r\n");
        self.reader.get_mut().write_all(cmd.as_bytes())?;
        self.reader.get_mut().flush()?;
        let mut status = String::new();
        self.reader.read_line(&mut status)?;
        if status.starts_with("222") {
            let mut body = Vec::new();
            loop {
                let mut line = Vec::new();
                self.reader.read_until(b'\n', &mut line)?;
                if line.is_empty() {
                    return Err(NntpError::UnexpectedResponse("eof inside article".into()));
                }
                if line.ends_with(b"\n") {
                    line.pop();
                }
                if line.ends_with(b"\r") {
                    line.pop();
                }
                if line == b"." {
                    break;
                }
                body.extend_from_slice(unstuff(&line));
                body.push(b'\n');
            }
            Ok(body)
        } else if status.starts_with("480") || status.starts_with("481") {
            // Session no longer authenticated (480, the standard BODY
            // reply) or credentials rejected (481): re-auth signal, never
            // a missing article. 482 here is out-of-sequence and stays a
            // generic protocol error below. No credential material is
            // retained: the status line is dropped.
            Err(NntpError::AuthExpired)
        } else if status.starts_with("430") || status.starts_with("423") {
            Err(NntpError::NoSuchArticle(message_id.to_string()))
        } else {
            Err(NntpError::UnexpectedResponse(status.trim_end().to_string()))
        }
    }
}

/// Strip one leading dot per NNTP dot-stuffing (RFC 3977 §3.1).
pub(crate) fn unstuff(line: &[u8]) -> &[u8] {
    if line.first() == Some(&b'.') {
        &line[1..]
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_article_is_not_a_provider_failure() {
        assert!(!NntpError::NoSuchArticle("m".into()).is_provider_failure());
        assert!(!NntpError::AuthExpired.is_provider_failure());
        assert_eq!(NntpError::AuthExpired.code(), PROVIDER_AUTH_EXPIRED_CODE);
        assert!(!NntpError::InvalidMessageId("x".into()).is_provider_failure());
        assert!(NntpError::Io("reset".into()).is_provider_failure());
        assert!(NntpError::UnexpectedResponse("502".into()).is_provider_failure());
    }

    #[test]
    fn message_id_validation_rejects_wire_breakers() {
        assert!(valid_message_id("abc123@synthetic.test"));
        assert!(valid_message_id("a-seg0@synthetic.test"));
        assert!(!valid_message_id(""));
        assert!(!valid_message_id("x>\r\nBODY <other@article>"));
        assert!(!valid_message_id("a\nb@c"));
        assert!(!valid_message_id("a\rb@c"));
        assert!(!valid_message_id("a b@c"));
        assert!(!valid_message_id("a<b@c"));
        assert!(!valid_message_id(&"a".repeat(251)));
        assert!(valid_message_id(&format!("{}@c", "a".repeat(248))));
    }

    #[test]
    fn unstuff_strips_exactly_one_dot() {
        assert_eq!(unstuff(b"..stuffed"), &b".stuffed"[..]);
        assert_eq!(unstuff(b"...double"), &b"..double"[..]);
        assert_eq!(unstuff(b"plain"), &b"plain"[..]);
        assert_eq!(unstuff(b""), &b""[..]);
    }
}
