//! Shared parser and NNTP client integration against the synthetic server.

use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig};
use nntune_core::nzb;
use nntune_core::transports::connect_native;
use nntune_test_nntp::{default_corpus, nzb_for, TestNntpServer};

#[tokio::test]
async fn shared_parser_fetches_and_decodes_article_from_synthetic_server() {
    let corpus = default_corpus();
    let article = corpus
        .iter()
        .find(|article| article.message_id == "<single-yenc-001@nntune.test>")
        .expect("single-part fixture article");
    let xml = nzb_for(&[article], "alt.binaries.nntune.test");

    let parsed = nzb::parse(xml.as_bytes()).expect("shared NZB parser accepts fixture NZB");
    assert_eq!(parsed.files.len(), 1);
    assert_eq!(parsed.files[0].segments.len(), 1);
    let message_id = parsed.files[0].segments[0].message_id.as_str();

    let server = TestNntpServer::spawn_for_test().await;
    let config = NntpConfig {
        host: "127.0.0.1".into(),
        port: server.addr().port(),
        tls: false,
        username: Some("testuser".into()),
        password: Some("testpass".into()),
        timeout: Duration::from_secs(2),
        max_pipelined: 4,
        ..NntpConfig::default()
    };
    let transport = connect_native(&config)
        .await
        .expect("connect to fixture server");
    let mut client = NntpClient::connect(transport, &config)
        .await
        .expect("NNTP handshake");
    let mut articles = client
        .fetch_many(&[message_id])
        .await
        .expect("fetch parsed NZB article");
    client.quit().await;

    let fetched = articles.pop().expect("one fetched article");
    assert_eq!(fetched.message_id, message_id);
    let mut decoded = vec![0; article.payload.len()];
    let decoded_len = nntune_core::yenc::decode_to(message_id, &fetched.article, &mut decoded)
        .expect("shared yEnc decoder accepts fetched article");
    assert_eq!(decoded_len, article.payload.len());
    assert_eq!(decoded, article.payload);
}
