//! Portable NNTP transport abstraction.
//!
//! The crate never touches the network or the clock directly: [`Transport`]
//! is a byte pipe, and every implementation enforces the configured timeout
//! on each operation, reporting expiry as [`io::ErrorKind::TimedOut`].
//!
//! - Native (tokio + rustls): [`crate::transports::tokio_tls`] (feature
//!   `native-tls`).
//! - Cloudflare Workers (`connect()` socket): [`crate::transports::worker_socket`]
//!   (feature `worker-transport`, wasm32 only).
//! - Tests: implement [`Transport`] over an in-memory script.
//!
//! [`BufferedTransport`] adds CRLF line framing and NNTP dot-block reading on
//! top of any [`Transport`].

use std::io;

/// Byte pipe underlying the NNTP client.
///
/// # Timeouts
///
/// Every method must enforce the connection's read/write timeout and report
/// expiry as an [`io::Error`] with kind [`io::ErrorKind::TimedOut`]. The core
/// protocol code owns no timers so it stays portable across tokio (native)
/// and Workers (where [`worker::Delay`](https://docs.rs/worker) backs the
/// timeout) runtimes.
pub trait Transport {
    /// Read up to `buf.len()` bytes; returns the byte count (`0` on EOF).
    /// Must enforce the read timeout.
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl std::future::Future<Output = io::Result<usize>> + 'a;

    /// Write the whole buffer; must enforce the write timeout.
    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl std::future::Future<Output = io::Result<()>> + 'a;

    /// Shut down the connection (NNTP `QUIT` should already have been sent).
    fn shutdown<'a>(&'a mut self) -> impl std::future::Future<Output = io::Result<()>> + 'a;
}

/// Maximum accepted length of one server line (protects the line buffer from
/// unbounded growth on a hostile or broken server).
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// CRLF line framing and NNTP dot-block reading over any [`Transport`].
pub struct BufferedTransport<T: Transport> {
    inner: T,
    buf: Vec<u8>,
}

impl<T: Transport> BufferedTransport<T> {
    /// Wraps a connected transport (the server greeting is still unread).
    pub fn new(inner: T) -> Self {
        BufferedTransport {
            inner,
            buf: Vec::with_capacity(4096),
        }
    }

    /// Returns the wrapped transport.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Shuts the wrapped transport down.
    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }

    /// Reads one CRLF-terminated server line, returned WITHOUT the terminator.
    /// Bare-LF line endings are tolerated.
    pub async fn read_line(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line = self.buf.drain(..=pos).collect::<Vec<u8>>();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            if self.buf.len() >= MAX_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NNTP line exceeds 16 KiB",
                ));
            }
            let mut chunk = [0u8; 4096];
            let n = self.inner.read(&mut chunk).await?;
            if n == 0 {
                if self.buf.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "NNTP connection closed mid-line",
                    ));
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "NNTP connection closed without CRLF",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Reads an NNTP multi-line (dot-terminated) data block into `out`,
    /// performing dot-unstuffing (`..X` → `.X`). The terminating lone `.`
    /// line is consumed and not written. `out` is appended to, not cleared.
    pub async fn read_dot_block(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        loop {
            let line = self.read_line().await?;
            if line == b"." {
                return Ok(());
            }
            if line.starts_with(b"..") {
                out.extend_from_slice(&line[1..]);
            } else {
                out.extend_from_slice(&line);
            }
            out.extend_from_slice(b"\r\n");
        }
    }

    /// Sends one protocol line (appends CRLF) in a single write, so one
    /// command is always one packet on the wire.
    pub async fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let mut framed = Vec::with_capacity(line.len() + 2);
        framed.extend_from_slice(line);
        framed.extend_from_slice(b"\r\n");
        self.inner.write_all(&framed).await
    }
}
