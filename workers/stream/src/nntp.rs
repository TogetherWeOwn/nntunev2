//! Blocking NNTP client for the miss path: `BODY <message-id>` fetches over
//! one pooled connection. Blocking I/O keeps the crate dependency-free; the
//! Durable Object runs one fetch at a time per connection.
//!
//! Article bodies are raw bytes (yEnc is binary); callers get the
//! dot-unstuffed bytes with `\n` line separators.

use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct NntpDeadline {
    started: Instant,
    budget: Duration,
}

impl NntpDeadline {
    pub(crate) fn new(budget: Duration) -> Self {
        Self {
            started: Instant::now(),
            budget,
        }
    }

    fn remaining(&self) -> io::Result<Duration> {
        let remaining = self.budget.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            Err(io::Error::new(
                ErrorKind::TimedOut,
                "NNTP operation deadline exceeded",
            ))
        } else {
            Ok(remaining)
        }
    }
}

pub struct NntpClient {
    reader: BufReader<std::net::TcpStream>,
    read_timeout: Duration,
}

impl NntpClient {
    /// Open a provider connection and consume the greeting.
    pub fn connect(addr: &SocketAddr) -> Result<Self, NntpError> {
        Self::connect_with_timeouts(addr, UpstreamTimeouts::default())
    }

    /// Open a provider connection with per-upstream timeouts and consume the
    /// greeting. The connect, greeting and first article can share one deadline
    /// so a stale warm connection cannot multiply the segment budget on retry.
    pub fn connect_with_timeouts(
        addr: &SocketAddr,
        timeouts: UpstreamTimeouts,
    ) -> Result<Self, NntpError> {
        let deadline = NntpDeadline::new(timeouts.connect.saturating_add(timeouts.read));
        Self::connect_with_deadline(addr, timeouts, &deadline)
    }

    pub(crate) fn connect_with_deadline(
        addr: &SocketAddr,
        timeouts: UpstreamTimeouts,
        deadline: &NntpDeadline,
    ) -> Result<Self, NntpError> {
        let connect_timeout = timeouts.connect.min(deadline.remaining()?);
        if connect_timeout.is_zero() {
            return Err(
                io::Error::new(ErrorKind::TimedOut, "NNTP connect deadline exceeded").into(),
            );
        }
        let stream = std::net::TcpStream::connect_timeout(addr, connect_timeout)?;
        stream.set_read_timeout(Some(timeouts.read))?;
        stream.set_write_timeout(Some(timeouts.read))?;
        let mut client = Self {
            reader: BufReader::new(stream),
            read_timeout: timeouts.read,
        };
        let greeting = client.read_line_with_deadline(deadline)?;
        if greeting.starts_with("200") || greeting.starts_with("201") {
            Ok(client)
        } else {
            Err(NntpError::UnexpectedResponse(
                greeting.trim_end().to_string(),
            ))
        }
    }

    /// Reuse this connection only for the peer it was dialed to, and apply
    /// the current play's idle timeout instead of retaining a stale setting.
    pub(crate) fn prepare_for_reuse(
        &mut self,
        expected_peer: &SocketAddr,
        read_timeout: Duration,
    ) -> io::Result<bool> {
        if self.reader.get_ref().peer_addr()? != *expected_peer {
            return Ok(false);
        }
        self.reader.get_ref().set_read_timeout(Some(read_timeout))?;
        self.reader
            .get_ref()
            .set_write_timeout(Some(read_timeout))?;
        self.read_timeout = read_timeout;
        Ok(true)
    }

    /// Fetch one article body; returns the raw dot-unstuffed bytes. The read
    /// timeout is both the idle-read limit and the total BODY-response budget.
    /// A malformed id is rejected before anything is written, so the
    /// pooled connection can never carry an injected command.
    pub fn article(&mut self, message_id: &str) -> Result<Vec<u8>, NntpError> {
        let deadline = NntpDeadline::new(self.read_timeout);
        self.article_with_deadline(message_id, &deadline)
    }

    pub(crate) fn article_with_deadline(
        &mut self,
        message_id: &str,
        deadline: &NntpDeadline,
    ) -> Result<Vec<u8>, NntpError> {
        let mid = message_id.trim().trim_matches(|c| c == '<' || c == '>');
        if !valid_message_id(mid) {
            return Err(NntpError::InvalidMessageId(message_id.to_string()));
        }
        let cmd = format!("BODY <{mid}>\r\n");
        let write_timeout = self.read_timeout.min(deadline.remaining()?);
        self.reader
            .get_mut()
            .set_write_timeout(Some(write_timeout))?;
        self.reader.get_mut().write_all(cmd.as_bytes())?;
        self.reader.get_mut().flush()?;
        let status = self.read_line_with_deadline(deadline)?;
        if status.starts_with("222") {
            let mut body = Vec::new();
            loop {
                let mut line = Vec::new();
                self.read_until_with_deadline(b'\n', &mut line, deadline)?;
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

    fn read_line_with_deadline(&mut self, deadline: &NntpDeadline) -> io::Result<String> {
        let mut bytes = Vec::new();
        self.read_until_with_deadline(b'\n', &mut bytes, deadline)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
    }

    fn read_until_with_deadline(
        &mut self,
        delimiter: u8,
        output: &mut Vec<u8>,
        deadline: &NntpDeadline,
    ) -> io::Result<usize> {
        let mut read = 0;
        loop {
            let remaining = deadline.remaining()?;
            self.reader
                .get_ref()
                .set_read_timeout(Some(self.read_timeout.min(remaining)))?;
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(read);
            }
            let count = available
                .iter()
                .position(|byte| *byte == delimiter)
                .map_or(available.len(), |index| index + 1);
            let terminated = available[count - 1] == delimiter;
            output.extend_from_slice(&available[..count]);
            self.reader.consume(count);
            read += count;
            if terminated {
                return Ok(read);
            }
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

/// Per-upstream timeouts for the NNTP dial and article reads.
/// `connect` bounds the TCP dial; `read` bounds each idle read and the total
/// status-plus-body article response. The first-segment dial, greeting and
/// article share a `connect + read` deadline, including a warm-connection
/// retry; later articles each get their own `read` deadline.
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
    fn warm_connection_matches_its_peer_and_uses_current_read_timeout() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind warm NNTP");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream.write_all(b"200 warm NNTP ready\r\n").unwrap();
            let mut byte = [0];
            let _ = stream.read(&mut byte);
        });

        let original = UpstreamTimeouts::new(Duration::from_secs(2), Duration::from_millis(20));
        let mut client = NntpClient::connect_with_timeouts(&addr, original).expect("greeting");
        let current_read_timeout = Duration::from_millis(500);
        assert!(client
            .prepare_for_reuse(&addr, current_read_timeout)
            .expect("socket options update"));
        assert_eq!(client.read_timeout, current_read_timeout);

        let other_peer: SocketAddr = "127.0.0.1:119".parse().unwrap();
        assert!(!client
            .prepare_for_reuse(&other_peer, Duration::from_secs(1))
            .expect("peer check"));
        assert_eq!(client.read_timeout, current_read_timeout);

        drop(client);
        server.join().expect("server thread");
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
    fn article_deadline_bounds_a_trickling_body() {
        use std::io::{BufRead, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind trickle NNTP");
        let addr = listener.local_addr().expect("local addr");
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            stream.write_all(b"200 trickle NNTP ready\r\n").unwrap();
            let mut command = Vec::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_until(b'\n', &mut command)
                .unwrap();
            stream.write_all(b"222 article follows\r\n").unwrap();
            for _ in 0..20 {
                if server_stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
                if stream.write_all(b"body line\r\n").is_err() {
                    break;
                }
            }
            let _ = stream.write_all(b".\r\n");
        });
        let timeouts =
            UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(200));
        let mut client =
            NntpClient::connect_with_timeouts(&addr, timeouts).expect("greeting arrives");
        let started = std::time::Instant::now();
        let error = client
            .article("<trickle-seg0@synthetic.test>")
            .expect_err("a trickling article must hit its operation deadline");
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        server.join().expect("server thread");

        assert!(
            matches!(error, NntpError::Io(_)),
            "deadline is a provider I/O failure: {error:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "the article deadline should stop a slow trickle promptly, took {elapsed:?}"
        );
    }

    #[test]
    fn delayed_greeting_then_silent_respects_shared_connect_plus_read_deadline() {
        use std::io::{BufRead, Write};
        use std::net::TcpListener;

        // Shared first-segment budget: connect + read. The greeting delay
        // consumes most of it, so the silent article that follows must time
        // out on the *remaining* budget, not a fresh read timeout. This pins
        // the `read_timeout.min(remaining)` socket bound: with a plain
        // `read_timeout` a single blocking read overruns the budget.
        let timeouts =
            UpstreamTimeouts::new(Duration::from_millis(200), Duration::from_millis(1500));
        let budget = timeouts.connect.saturating_add(timeouts.read);
        let greeting_delay = Duration::from_millis(900);

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind delayed NNTP");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client");
            std::thread::sleep(greeting_delay);
            if stream.write_all(b"200 delayed NNTP ready\r\n").is_err() {
                return;
            }
            let _ = stream.flush();
            // Read the BODY command, then go silent: hold the connection open
            // so the client's deadline (not EOF) ends the op. Detached like
            // the hung test: no join, the 5 s hold outlives the assertion.
            let mut command = Vec::new();
            let _ =
                BufReader::new(stream.try_clone().expect("clone")).read_until(b'\n', &mut command);
            std::thread::sleep(Duration::from_secs(5));
        });

        let deadline = NntpDeadline::new(budget);
        let t0 = std::time::Instant::now();
        let mut client = NntpClient::connect_with_deadline(&addr, timeouts, &deadline)
            .expect("greeting still arrives within budget");
        let err = client
            .article_with_deadline("<delayed-seg0@synthetic.test>", &deadline)
            .expect_err("silent article after a delayed greeting must time out");
        let elapsed = t0.elapsed();

        assert!(
            err.is_provider_failure(),
            "deadline is a provider failure: {err:?}"
        );
        assert!(
            elapsed < budget + Duration::from_millis(400),
            "a single blocking read must not overrun the shared connect+read budget ({budget:?}), took {elapsed:?}"
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
