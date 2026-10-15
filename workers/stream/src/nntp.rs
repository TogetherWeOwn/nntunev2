//! Blocking NNTP client for the miss path: `BODY <message-id>` fetches over
//! one pooled connection. Blocking I/O keeps the crate dependency-free; the
//! Durable Object runs one fetch at a time per connection.
//!
//! Article bodies are raw bytes (yEnc is binary); callers get the
//! dot-unstuffed bytes with `\n` line separators.

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::time::Duration;

/// What went wrong talking to the provider.
#[derive(Debug, PartialEq, Eq)]
pub enum NntpError {
    Io(String),
    NoSuchArticle(String),
    UnexpectedResponse(String),
    /// The segment id failed RFC 5536 validation and was never sent.
    InvalidMessageId(String),
}

impl NntpError {
    /// True when the provider (or the connection to it) is unhealthy, as
    /// opposed to one article being gone (`430`/`423`) or one id being
    /// malformed, which say nothing about the provider and must not
    /// quarantine the user's pool.
    pub fn is_provider_failure(&self) -> bool {
        !matches!(
            self,
            NntpError::NoSuchArticle(_) | NntpError::InvalidMessageId(_)
        )
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
        Self::connect_with_timeouts(addr, UpstreamTimeouts::default())
    }

    /// Open a provider connection with per-upstream timeouts and consume the
    /// greeting. Both the dial and every blocking read on the connection are
    /// bounded, so a hung upstream surfaces as [`NntpError::Io`] (a provider
    /// failure that counts toward the pool circuit breaker) instead of
    /// hanging the play path.
    pub fn connect_with_timeouts(
        addr: &SocketAddr,
        timeouts: UpstreamTimeouts,
    ) -> Result<Self, NntpError> {
        let stream = std::net::TcpStream::connect_timeout(addr, timeouts.connect)?;
        stream.set_read_timeout(Some(timeouts.read))?;
        stream.set_write_timeout(Some(timeouts.read))?;
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
        } else if status.starts_with("430")
            || status.starts_with("423")
            || status.starts_with("482")
        {
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

/// Per-upstream timeouts for the NNTP dial and every blocking read on it.
/// Defaults (`5s` connect / `10s` read) bound the play path to
/// `connect + read + margin` per segment op instead of hanging on a dead
/// upstream; callers with a tighter budget (tests, low-latency providers)
/// pass shorter values through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpstreamTimeouts {
    pub connect: Duration,
    pub read: Duration,
}

impl Default for UpstreamTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            read: Duration::from_secs(10),
        }
    }
}

impl UpstreamTimeouts {
    pub fn new(connect: Duration, read: Duration) -> Self {
        Self { connect, read }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_article_is_not_a_provider_failure() {
        assert!(!NntpError::NoSuchArticle("m".into()).is_provider_failure());
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
    fn hung_upstream_trips_the_configured_read_timeout() {
        use std::io::Write;
        use std::net::TcpListener;
        // Greeting, then silence: the server accepts but never answers BODY.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind hung NNTP");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let _ = s.write_all(b"200 hung NNTP ready\r\n");
                let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                let mut buf = [0u8; 1024];
                use std::io::Read;
                // Drain the BODY command, then never reply.
                let _ = s.read(&mut buf);
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        let timeouts =
            UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(200));
        let t0 = std::time::Instant::now();
        let mut client =
            NntpClient::connect_with_timeouts(&addr, timeouts).expect("greeting arrives");
        let err = client
            .article("<hung-seg0@synthetic.test>")
            .expect_err("hung upstream must time out");
        let elapsed = t0.elapsed();
        assert!(
            err.is_provider_failure(),
            "a hung read indicts the provider"
        );
        assert!(
            elapsed < timeouts.read + Duration::from_secs(5),
            "play path stays within timeout + margin, took {elapsed:?}"
        );
    }

    #[test]
    fn unstuff_strips_exactly_one_dot() {
        assert_eq!(unstuff(b"..stuffed"), &b".stuffed"[..]);
        assert_eq!(unstuff(b"...double"), &b"..double"[..]);
        assert_eq!(unstuff(b"plain"), &b"plain"[..]);
        assert_eq!(unstuff(b""), &b""[..]);
    }
}
