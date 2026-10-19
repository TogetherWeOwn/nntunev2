//! Pre-review fix verification: a pre-write validation failure in
//! `fetch_many` must not poison the connection (contrast: post-write
//! failures invalidate because unread replies may be outstanding).

use std::collections::VecDeque;
use std::io;

use nntune_core::error::Error;
use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::transport::Transport;

struct Recorder {
    to_client: VecDeque<u8>,
    writes: Vec<Vec<u8>>,
}

impl Recorder {
    fn new(readable: &str) -> Self {
        Recorder {
            to_client: readable.as_bytes().iter().cloned().collect(),
            writes: Vec::new(),
        }
    }
}

impl Transport for Recorder {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.to_client.len());
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "script exhausted",
            ));
        }
        for (i, slot) in buf[..n].iter_mut().enumerate() {
            *slot = self.to_client[i];
        }
        self.to_client.drain(..n);
        Ok(n)
    }

    async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.writes.push(buf.to_vec());
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn fetch_many_validation_failure_leaves_connection_usable() {
    let readable = "200 ready\r\n500 no\r\n281 ok\r\n223 42 <one@x> exists\r\n";
    let config = NntpConfig {
        host: "nntp.example.com".into(),
        tls: false,
        username: Some("user".into()),
        password: Some("pass".into()),
        timeout: std::time::Duration::from_secs(5),
        max_pipelined: 4,
        ..NntpConfig::default()
    };
    let mut client = NntpClient::connect(Recorder::new(readable), &config)
        .await
        .expect("handshake");
    let err = client
        .fetch_many(&["one@x", "bad id@x"])
        .await
        .expect_err("escaping batch must be refused");
    assert!(
        matches!(err, Error::Malformed(_)),
        "expected Malformed, got {err:?}"
    );
    // The connection must still be usable: a plain valid STAT succeeds.
    assert_eq!(client.stat("one@x").await.unwrap(), 42);
}
