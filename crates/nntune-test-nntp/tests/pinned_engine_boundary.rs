//! Consumer-boundary acceptance for the pinned velmora engine.
//!
//! The server/corpus suites and the engine suites run in velmora's own CI,
//! and Cargo never runs a dependency's tests. These tests keep a
//! representative protocol assertion in this repository's required gate:
//! the `nntune_core` NNTP client and yEnc decoder (re-exported from the
//! pinned velmora rev) against the `nntune_test_nntp` server at the same
//! pin. A pin bump that regresses fetch, decode or the `POST` spool fails
//! here, not only in velmora.

use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig, PostOutcome};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_test_nntp::{
    default_corpus, generate_wav_bytes, nzb_for, nzb_for_multipart, TestNntpServer,
};

#[test]
fn message_ids_reject_unicode_whitespace_and_controls_at_consumer_boundary() {
    for ch in ['\u{a0}', '\u{85}', '\u{2028}', '\u{80}'] {
        let id = format!("boundary{ch}@nntune.test");
        assert!(nntune_core::nzb::normalize_message_id(&id).is_err());
    }
    let id = "café@nntune.test";
    assert_eq!(nntune_core::nzb::normalize_message_id(id).unwrap(), id);
}

#[test]
fn fixture_nzbs_preserve_escaped_identity_and_encoded_size() {
    let mut article = default_corpus().remove(0);
    article.message_id = "<boundary&escaped@nntune.test>".into();
    let articles = [&article];
    for xml in [
        nzb_for(&articles, "alt.binaries.nntune&test"),
        nzb_for_multipart(&articles, "alt.binaries.nntune&test"),
    ] {
        assert!(xml.contains("<group>alt.binaries.nntune&amp;test</group>"));
        let document = nntune_core::nzb::parse(xml.as_bytes()).expect("fixture NZB parses");
        assert_eq!(document.files.len(), 1);
        let segments = &document.files[0].segments;
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].message_id, "boundary&escaped@nntune.test");
        assert_eq!(segments[0].encoded_bytes, article.raw.len() as i64);
        assert_eq!(document.encoded_bytes, article.raw.len() as i64);
        assert_ne!(article.raw.len(), article.payload.len());
    }
}

async fn connect(server: &TestNntpServer) -> NntpClient<NativeTlsTransport> {
    let config = NntpConfig {
        host: "127.0.0.1".into(),
        port: server.addr().port(),
        tls: false,
        username: Some("testuser".into()),
        password: Some("testpass".into()),
        timeout: Duration::from_secs(5),
        max_pipelined: 4,
        ..NntpConfig::default()
    };
    let transport = connect_native(&config)
        .await
        .expect("connect to fixture server");
    NntpClient::connect(transport, &config)
        .await
        .expect("NNTP handshake")
}

fn bare(message_id: &str) -> &str {
    message_id.trim_start_matches('<').trim_end_matches('>')
}

#[tokio::test]
async fn multipart_fetch_decodes_to_reference_tone() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut client = connect(&server).await;
    let corpus = default_corpus();

    let mut joined = Vec::new();
    for n in 1..=3 {
        let id = format!("<multi-660-part{n}-of-3@nntune.test>");
        let fixture = corpus
            .iter()
            .find(|a| a.message_id == id)
            .expect("multipart fixture article");
        let mut article = Vec::new();
        client
            .article_to(bare(&id), &mut article)
            .await
            .expect("fetch multipart article");
        let mut part = vec![0; fixture.payload.len()];
        let written = nntune_core::yenc::decode_to(bare(&id), &article, &mut part)
            .expect("yEnc part decodes");
        assert_eq!(written, fixture.payload.len(), "part {n} length");
        joined.extend_from_slice(&part[..written]);
    }
    client.quit().await;

    assert_eq!(joined, generate_wav_bytes(660.0, 210, 8000));
}

#[tokio::test]
async fn posted_article_round_trips_through_spool() {
    let server = TestNntpServer::spawn_for_test().await;
    let mut client = connect(&server).await;

    let id = "boundary-post-001@nntune.test";
    let article = format!(
        "From: poster@nntune.test\r\n\
         Newsgroups: alt.binaries.nntune.test\r\n\
         Subject: boundary\r\n\
         Date: Mon, 13 Oct 2026 00:00:00 +0000\r\n\
         Message-ID: <{id}>\r\n\
         \r\n\
         hello pinned spool\r\n"
    );
    let outcome = client
        .post_article(id, article.as_bytes())
        .await
        .expect("POST accepted");
    assert_eq!(outcome, PostOutcome::Posted);
    assert!(server.is_stored(&format!("<{id}>")).await, "spool holds it");

    client.stat(id).await.expect("STAT finds posted article");
    let mut fetched = Vec::new();
    client
        .article_to(id, &mut fetched)
        .await
        .expect("fetch posted article");
    assert!(
        fetched.windows(18).any(|w| w == b"hello pinned spool"),
        "posted body missing: {:?}",
        String::from_utf8_lossy(&fetched)
    );

    let again = client
        .post_article(id, article.as_bytes())
        .await
        .expect("duplicate POST counts as success");
    assert_eq!(again, PostOutcome::Duplicate);
    client.quit().await;
}
