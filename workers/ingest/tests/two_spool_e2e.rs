//! Two-spool verification (stage B2.5) against two synthetic spools.
//!
//! Two [`TestNntpServer`] instances stand in for the two backbones: spool A
//! holds the first-spool post, spool B is the verify lane. Simulated loss is
//! runtime hiding on B only ([`TestNntpServer::hide`]) — never killed
//! articles on a real provider. No test in this file touches a live
//! provider; every byte comes from the fixture corpus.
//!
//! The suite pins the card's done-criteria without ever logging or storing
//! message-IDs, subjects, file names or segment lists: article identities
//! travel as corpus indexes, the progress record carries verdicts only, and
//! assertions compare counts and typed outcomes.
//!
//! - missing-on-B articles are detected by `STAT` and same-ID re-posted to
//!   verified ([`TwoSpoolOutcome::Verified`]);
//! - a `435` duplicate on re-post counts as present;
//! - dedupe reuse skips posting entirely for an identical sha256;
//! - a prohibited lane exhausts typed on the original NZB
//!   ([`TwoSpoolOutcome::RepostExhausted`] with `keep_original_nzb`).

use std::time::Duration;

use nntune_core::nntp::{NntpClient, NntpConfig, PostOutcome};
use nntune_core::transports::{connect_native, NativeTlsTransport};
use nntune_ingest::dispatch::content_sha256_hex;
use nntune_ingest::two_spool::{
    classify_post_error, classify_stat_error, conclude, decide_dedupe, validate_same_id_repost,
    DedupeDecision, PostVerdict, StatVerdict, TwoSpoolOutcome, TwoSpoolProgress,
    REPOST_EXHAUSTED_CODE,
};
use nntune_test_nntp::{default_corpus, ServerConfig, TestNntpServer};

/// Backbones are pool-config labels, never hostnames (vault secrets stay in
/// pool config).
const BACKBONE_A: &str = "backbone-a";
const BACKBONE_B: &str = "backbone-b";

fn client_config(port: u16) -> NntpConfig {
    NntpConfig {
        host: "127.0.0.1".into(),
        port,
        tls: false,
        username: Some("testuser".into()),
        password: Some("testpass".into()),
        timeout: Duration::from_secs(5),
        max_pipelined: 4,
        ..NntpConfig::default()
    }
}

fn bare_id(bracketed: &str) -> &str {
    bracketed.trim_matches(|c| c == '<' || c == '>')
}

type NativeClient = NntpClient<NativeTlsTransport>;

/// `STAT` one article on the verify lane and fold the verdict.
async fn stat_index(
    client: &mut NativeClient,
    progress: &mut TwoSpoolProgress,
    ids: &[String],
    index: usize,
) {
    let verdict = match client.stat(&ids[index]).await {
        Ok(_) => StatVerdict::Present,
        Err(err) => classify_stat_error(&err),
    };
    progress.fold_stat(index, verdict);
}

/// Same-ID re-post one missing article: fetch the bytes from spool A, check
/// the re-post carries the same ID and the same bytes, `POST` to spool B.
async fn repost_index(
    spool_a: &mut NativeClient,
    spool_b: &mut NativeClient,
    progress: &mut TwoSpoolProgress,
    ids: &[String],
    index: usize,
) {
    let mut article = Vec::new();
    spool_a
        .article_to(&ids[index], &mut article)
        .await
        .expect("first spool holds the article");
    let digest = content_sha256_hex(&article);
    validate_same_id_repost(&ids[index], &ids[index], &digest, &digest)
        .expect("re-post reuses the ID and the bytes");
    let verdict = match spool_b.post_article(&ids[index], &article).await {
        Ok(PostOutcome::Posted) => PostVerdict::Posted,
        Ok(PostOutcome::Duplicate) => PostVerdict::DuplicatePresent,
        Err(err) => classify_post_error(&err),
    };
    progress.fold_repost(index, verdict);
}

async fn spawn_pair() -> (TestNntpServer, TestNntpServer) {
    let mut config = ServerConfig::for_test();
    config.missing_ids.clear();
    (
        TestNntpServer::spawn(config.clone()).await,
        TestNntpServer::spawn(config).await,
    )
}

#[tokio::test]
async fn missing_articles_same_id_repost_to_verified() {
    let corpus = default_corpus();
    assert!(corpus.len() >= 3, "need at least three fixture articles");
    let ids: Vec<String> = corpus
        .iter()
        .take(3)
        .map(|a| bare_id(&a.message_id).to_string())
        .collect();

    let (spool_a, spool_b) = spawn_pair().await;
    // Propagation gap on the verify lane only: the middle article answers
    // 430 there while spool A still serves it.
    spool_b.hide(&ids[1]).await;

    let mut client_a = NntpClient::connect(
        connect_native(&client_config(spool_a.addr().port()))
            .await
            .expect("connect spool A"),
        &client_config(spool_a.addr().port()),
    )
    .await
    .expect("handshake spool A");
    let mut client_b = NntpClient::connect(
        connect_native(&client_config(spool_b.addr().port()))
            .await
            .expect("connect spool B"),
        &client_config(spool_b.addr().port()),
    )
    .await
    .expect("handshake spool B");

    let mut progress =
        TwoSpoolProgress::new(BACKBONE_A, BACKBONE_B, ids.len()).expect("backbone pair");
    // Stage 1: STAT every article in batches of 100 from the verify lane.
    let batches = progress.stat_batches();
    assert_eq!(batches.len(), 1, "three articles fit one STAT batch");
    for batch in &batches {
        for &index in batch {
            stat_index(&mut client_b, &mut progress, &ids, index).await;
        }
    }
    assert_eq!(progress.pending_repost(), vec![1]);

    // Crash-recovery resume: the persisted slots continue identically after
    // a JSON round-trip; only the missing article still needs work.
    let json = serde_json::to_string(&progress).expect("progress serializes");
    let mut progress: TwoSpoolProgress = serde_json::from_str(&json).expect("progress resumes");
    assert_eq!(progress.pending_stat(), Vec::<usize>::new());
    assert_eq!(progress.pending_repost(), vec![1]);

    // Stage 2: same-ID re-post the missing article through the posting
    // account on the verify lane.
    repost_index(&mut client_a, &mut client_b, &mut progress, &ids, 1).await;
    // A redelivered POST replays the same bytes: the 435 duplicate counts
    // as present (confirmed by STAT inside post_article).
    let mut article = Vec::new();
    client_a
        .article_to(&ids[1], &mut article)
        .await
        .expect("first spool holds the article");
    assert_eq!(
        client_b
            .post_article(&ids[1], &article)
            .await
            .expect("redelivered POST confirms duplicate"),
        PostOutcome::Duplicate
    );

    // Stage 3: re-STAT the re-posted article to verified.
    let reposted = progress.pending_stat();
    assert_eq!(reposted, vec![1]);
    for index in reposted {
        stat_index(&mut client_b, &mut progress, &ids, index).await;
    }
    assert_eq!(
        conclude(&progress),
        TwoSpoolOutcome::Verified { article_count: 3 }
    );
    // Only the missing article crossed the wire as a POST.
    assert_eq!(spool_b.posted_count().await, 1);
    assert_eq!(spool_b.posted_count_for(&[ids[1].as_str()]).await, 1);

    client_a.quit().await;
    client_b.quit().await;
}

#[tokio::test]
async fn dedupe_reuse_skips_posting_for_identical_sha256() {
    let corpus = default_corpus();
    let ids: Vec<String> = corpus
        .iter()
        .take(2)
        .map(|a| bare_id(&a.message_id).to_string())
        .collect();
    let (_spool_a, spool_b) = spawn_pair().await;

    // First pass posts fresh (no verified post exists) and verifies.
    let tag_input = ids.join(",");
    let sha = content_sha256_hex(tag_input.as_bytes());
    assert!(matches!(
        decide_dedupe(&sha, None).expect("valid sha"),
        DedupeDecision::PostFresh { .. }
    ));
    let posted_before = spool_b.posted_count().await;

    // Second pass with the identical tagged plaintext sha256 finds the
    // verified two-spool post and reuses it: no STAT, no POST.
    assert!(matches!(
        decide_dedupe(&sha, Some(("posted_row", true, true))).expect("valid sha"),
        DedupeDecision::Reuse { .. }
    ));
    assert_eq!(spool_b.posted_count().await, posted_before);

    // An unusable existing post cannot use the legacy first-wins insert
    // for fresh custody: refuse until fenced generation replacement exists.
    for (verified, retained) in [(false, true), (true, false), (false, false)] {
        assert_eq!(
            decide_dedupe(&sha, Some(("posted_row", verified, retained))),
            Err("existing post requires fenced generation replacement"),
        );
        assert_eq!(spool_b.posted_count().await, posted_before);
    }
}

#[tokio::test]
async fn prohibited_lane_exhausts_typed_on_the_original_nzb() {
    let corpus = default_corpus();
    let ids: Vec<String> = corpus
        .iter()
        .take(2)
        .map(|a| bare_id(&a.message_id).to_string())
        .collect();

    let (spool_a, spool_b) = spawn_pair().await;
    spool_b.hide(&ids[0]).await;
    // Posting account not permitted on the verify lane: every re-post is
    // refused without burning retry budget.
    spool_b.set_posting_allowed(false).await;

    let mut client_a = NntpClient::connect(
        connect_native(&client_config(spool_a.addr().port()))
            .await
            .expect("connect spool A"),
        &client_config(spool_a.addr().port()),
    )
    .await
    .expect("handshake spool A");
    let mut client_b = NntpClient::connect(
        connect_native(&client_config(spool_b.addr().port()))
            .await
            .expect("connect spool B"),
        &client_config(spool_b.addr().port()),
    )
    .await
    .expect("handshake spool B");

    let mut progress =
        TwoSpoolProgress::new(BACKBONE_A, BACKBONE_B, ids.len()).expect("backbone pair");
    for index in 0..ids.len() {
        stat_index(&mut client_b, &mut progress, &ids, index).await;
    }
    assert_eq!(progress.pending_repost(), vec![0]);

    repost_index(&mut client_a, &mut client_b, &mut progress, &ids, 0).await;
    // The 440 refusal exhausts immediately: no retries burned, no second
    // ID set minted, nothing stored on the verify lane.
    assert_eq!(
        conclude(&progress),
        TwoSpoolOutcome::RepostExhausted {
            verified: 1,
            exhausted: 1,
            keep_original_nzb: true,
        }
    );
    assert_eq!(REPOST_EXHAUSTED_CODE, "repost_budget_exhausted");
    assert_eq!(spool_b.posted_count().await, 0);

    client_a.quit().await;
    client_b.quit().await;
}
