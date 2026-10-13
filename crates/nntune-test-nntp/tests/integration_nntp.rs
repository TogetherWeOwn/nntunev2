//! Core NNTP client integration tests against the synthetic server.
//!
//! These run in CI with no real Usenet: every test spawns an in-process
//! `TestNntpServer` on an ephemeral loopback port and speaks raw NNTP.
//! They are the acceptance proof for the fixture server — `nntune-core`
//! (and stream/ingest harnesses) reuse the same spawn helpers.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use nntune_test_nntp::{ServerConfig, TestNntpServer};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(server: &TestNntpServer) -> Self {
        let stream = TcpStream::connect(server.addr()).await.unwrap();
        let (r, w) = stream.into_split();
        let mut c = Self {
            reader: BufReader::new(r),
            writer: w,
        };
        let greeting = c.read_line().await;
        assert!(greeting.starts_with("200"), "greeting: {greeting:?}");
        c
    }

    async fn read_line(&mut self) -> String {
        let mut s = String::new();
        let n = self.reader.read_line(&mut s).await.unwrap();
        assert!(n > 0, "connection closed while reading line");
        s
    }

    /// Send a command, return the single-line response.
    async fn cmd(&mut self, cmd: &str) -> String {
        self.writer
            .write_all(format!("{cmd}\r\n").as_bytes())
            .await
            .unwrap();
        self.read_line().await
    }

    /// Send a command, return (first line, dot-terminated block, unstuffed).
    ///
    /// Byte-oriented (`read_until`): yEnc bodies are arbitrary bytes, so the
    /// block must never be decoded as UTF-8 mid-transfer.
    async fn cmd_block(&mut self, cmd: &str) -> (String, Vec<u8>) {
        self.writer
            .write_all(format!("{cmd}\r\n").as_bytes())
            .await
            .unwrap();
        let first = self.read_line().await;
        let mut raw = Vec::new();
        loop {
            let mut line: Vec<u8> = Vec::new();
            let n = self.reader.read_until(b'\n', &mut line).await.unwrap();
            assert!(n > 0, "connection closed mid-block for {cmd:?}");
            // Strip CRLF/LF terminator.
            if line.ends_with(b"\n") {
                line.pop();
                if line.ends_with(b"\r") {
                    line.pop();
                }
            }
            if line == b"." {
                break;
            }
            // Un-dot-stuff: a stuffed line starts with "..".
            if line.starts_with(b"..") {
                raw.extend_from_slice(&line[1..]);
            } else {
                raw.extend_from_slice(&line);
            }
            raw.extend_from_slice(b"\r\n");
        }
        (first, raw)
    }

    async fn auth(&mut self, user: &str, pass: &str) -> (String, String) {
        let u = self.cmd(&format!("AUTHINFO USER {user}")).await;
        let p = self.cmd(&format!("AUTHINFO PASS {pass}")).await;
        (u, p)
    }
}

fn article_body(block: &[u8]) -> &[u8] {
    let pos = block
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header/body sep");
    &block[pos + 4..]
}

#[tokio::test]
async fn greets_and_lists_capabilities() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    let first = c.cmd("CAPABILITIES").await;
    assert!(first.starts_with("101"), "got {first:?}");
    // Drain capability block.
    loop {
        let l = c.read_line().await;
        if l.trim_end() == "." {
            break;
        }
    }
}

#[tokio::test]
async fn auth_flow_accepts_valid_rejects_invalid() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    let (u, p) = c.auth("testuser", "testpass").await;
    assert!(u.starts_with("381"), "got {u:?}");
    assert!(p.starts_with("281"), "got {p:?}");

    let mut bad = Client::connect(&server).await;
    let (_, p2) = bad.auth("testuser", "wrongpass").await;
    assert!(p2.starts_with("481"), "got {p2:?}");

    let mut unknown = Client::connect(&server).await;
    let (_, p3) = unknown.auth("nosuchuser", "x").await;
    assert!(p3.starts_with("481"), "got {p3:?}");
}

#[tokio::test]
async fn requires_auth_before_group() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    let r = c.cmd("GROUP alt.binaries.nntune.test").await;
    assert!(r.starts_with("481"), "got {r:?}");
}

#[tokio::test]
async fn group_article_body_head_stat_by_id_and_number() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;

    let g = c.cmd("GROUP alt.binaries.nntune.test").await;
    assert!(g.starts_with("211"), "got {g:?}");

    let (a1, block) = c.cmd_block("ARTICLE <single-yenc-001@nntune.test>").await;
    assert!(a1.starts_with("220"), "got {a1:?}");
    assert!(
        block.windows(12).any(|w| w == b"sine-440.wav"),
        "yEnc filename missing"
    );

    let (a2, _) = c.cmd_block("ARTICLE 1").await;
    assert!(a2.starts_with("220"), "by number: {a2:?}");

    let (b, _) = c.cmd_block("BODY <single-yenc-001@nntune.test>").await;
    assert!(b.starts_with("222"), "got {b:?}");

    let (h, head) = c.cmd_block("HEAD <single-yenc-001@nntune.test>").await;
    assert!(h.starts_with("221"), "got {h:?}");
    assert!(
        head.windows(8).any(|w| w == b"Subject:"),
        "HEAD missing Subject"
    );

    let s = c.cmd("STAT <single-yenc-001@nntune.test>").await;
    assert!(s.starts_with("223"), "got {s:?}");

    let q = c.cmd("QUIT").await;
    assert!(q.starts_with("205"), "got {q:?}");
}

#[tokio::test]
async fn xover_lists_all_articles() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;
    let (first, block) = c.cmd_block("XOVER 1-").await;
    assert!(first.starts_with("224"), "got {first:?}");
    let text = String::from_utf8_lossy(&block);
    for id in [
        "<single-yenc-001@nntune.test>",
        "<multi-660-part1-of-3@nntune.test>",
        "<obf-550-part1-of-3@nntune.test>",
        "<enc-rar-001@nntune.test>",
        "<enc-7z-001@nntune.test>",
    ] {
        assert!(text.contains(id), "XOVER missing {id}");
    }
}

#[tokio::test]
async fn yenc_single_part_decodes_to_wav() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;
    let (first, block) = c.cmd_block("ARTICLE <single-yenc-001@nntune.test>").await;
    assert!(first.starts_with("220"));
    let decoded = nntune_test_nntp::yenc::decode(article_body(&block));
    assert_eq!(&decoded[0..4], b"RIFF");
    assert_eq!(&decoded[8..12], b"WAVE");
    // Matches the locally generated reference tone byte-for-byte.
    let expected = nntune_test_nntp::generate_wav_bytes(440.0, 120, 8000);
    assert_eq!(decoded, expected);
}

#[tokio::test]
async fn multipart_reassembles_original() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;
    let mut joined = Vec::new();
    for n in 1..=3 {
        let id = format!("<multi-660-part{n}-of-3@nntune.test>");
        let (first, block) = c.cmd_block(&format!("ARTICLE {id}")).await;
        assert!(first.starts_with("220"), "part {n}: {first:?}");
        joined.extend(nntune_test_nntp::yenc::decode(article_body(&block)));
    }
    assert_eq!(
        joined,
        nntune_test_nntp::generate_wav_bytes(660.0, 210, 8000)
    );
}

#[tokio::test]
async fn obfuscated_subjects_served() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;
    let (first, block) = c
        .cmd_block("ARTICLE <obf-550-part2-of-3@nntune.test>")
        .await;
    assert!(first.starts_with("220"));
    // Served under an opaque subject, but the payload still decodes.
    assert!(block.windows(12).any(|w| w == b"q7w2e9r4t6y1"));
    let decoded = nntune_test_nntp::yenc::decode(article_body(&block));
    assert!(!decoded.is_empty());
}

#[tokio::test]
async fn rar_and_7z_markers_present() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;

    let (_, rar_block) = c.cmd_block("ARTICLE <enc-rar-001@nntune.test>").await;
    let rar = nntune_test_nntp::yenc::decode(article_body(&rar_block));
    assert_eq!(&rar[0..4], b"Rar!");

    let (_, seven_block) = c.cmd_block("ARTICLE <enc-7z-001@nntune.test>").await;
    let seven = nntune_test_nntp::yenc::decode(article_body(&seven_block));
    assert_eq!(&seven[0..2], b"7z");
}

#[tokio::test]
async fn missing_article_returns_430() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;

    let r1 = c.cmd("ARTICLE <does-not-exist@nntune.test>").await;
    assert!(r1.starts_with("430"), "unknown id: {r1:?}");

    let r2 = c.cmd("ARTICLE <missing-430@nntune.test>").await;
    assert!(r2.starts_with("430"), "injected missing: {r2:?}");

    let r3 = c.cmd("ARTICLE 9999").await;
    assert!(r3.starts_with("430"), "unknown number: {r3:?}");
}

#[tokio::test]
async fn connection_cap_per_credential() {
    // testuser allows 2 concurrent connections.
    let server = TestNntpServer::spawn_for_test().await;
    let mut c1 = Client::connect(&server).await;
    let mut c2 = Client::connect(&server).await;
    c1.auth("testuser", "testpass").await;
    c2.auth("testuser", "testpass").await;

    // Third connection for the same credential is rejected with 502.
    let stream = TcpStream::connect(server.addr()).await.unwrap();
    let (r, w) = stream.into_split();
    let mut c3 = Client {
        reader: BufReader::new(r),
        writer: w,
    };
    let greeting = c3.read_line().await;
    assert!(greeting.starts_with("200"));
    let u = c3.cmd("AUTHINFO USER testuser").await;
    assert!(u.starts_with("381"));
    c3.writer
        .write_all(b"AUTHINFO PASS testpass\r\n")
        .await
        .unwrap();
    let verdict = c3.read_line().await;
    assert!(
        verdict.starts_with("502"),
        "expected cap rejection, got {verdict:?}"
    );

    // A different credential with headroom still works.
    let mut other = Client::connect(&server).await;
    let (_, p) = other.auth("otheruser", "otherpass").await;
    assert!(p.starts_with("281"), "other credential blocked: {p:?}");
}

#[tokio::test]
async fn slow_link_throttles_article_body() {
    let mut config = ServerConfig::for_test();
    config.slow_bytes_per_sec = Some(400);
    let server = TestNntpServer::spawn(config).await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;

    let start = Instant::now();
    let (first, _) = c.cmd_block("ARTICLE <single-yenc-001@nntune.test>").await;
    assert!(first.starts_with("220"));
    let elapsed = start.elapsed();
    // ~1.3 KB stuffed body at 400 B/s must take well over a second.
    assert!(
        elapsed >= Duration::from_millis(1500),
        "throttle ineffective: {elapsed:?}"
    );
}

#[tokio::test]
async fn mid_body_disconnect_drops_connection() {
    let mut config = ServerConfig::for_test();
    config
        .drop_after_bytes
        .insert("<single-yenc-001@nntune.test>".to_string(), 16);
    let server = TestNntpServer::spawn(config).await;

    let stream = TcpStream::connect(server.addr()).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap(); // greeting
    w.write_all(b"AUTHINFO USER testuser\r\n").await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    w.write_all(b"AUTHINFO PASS testpass\r\n").await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    w.write_all(b"ARTICLE <single-yenc-001@nntune.test>\r\n")
        .await
        .unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap(); // 220 header
    assert!(line.starts_with("220"), "got {line:?}");

    // Server closes mid-body: read until EOF, expect a truncated body.
    let mut got = Vec::new();
    let _ = reader.read_to_end(&mut got).await;
    assert!(
        got.len() < 1004,
        "expected truncated body, got {} bytes",
        got.len()
    );
}

#[tokio::test]
async fn nzb_fixtures_match_served_ids() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/nzb");
    let mut referenced = HashSet::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        let mut rest = text.as_str();
        while let Some(i) = rest.find("&lt;") {
            rest = &rest[i + 4..];
            if let Some(j) = rest.find("&gt;") {
                referenced.insert(format!("<{}>", &rest[..j]));
                rest = &rest[j + 4..];
            } else {
                break;
            }
        }
    }
    assert!(
        !referenced.is_empty(),
        "no message-ids found in fixtures/nzb"
    );
    // Every referenced ID except the intentionally-missing one must be servable.
    let corpus: HashMap<_, _> = nntune_test_nntp::default_corpus()
        .into_iter()
        .map(|a| (a.message_id.clone(), a))
        .collect();
    for id in &referenced {
        assert!(
            corpus.contains_key(id),
            "NZB references unknown article {id}"
        );
    }

    let server = TestNntpServer::spawn_for_test().await;
    let mut c = Client::connect(&server).await;
    c.auth("testuser", "testpass").await;
    for id in &referenced {
        let r = c.cmd(&format!("STAT {id}")).await;
        assert!(r.starts_with("223"), "{id}: {r:?}");
    }
}
