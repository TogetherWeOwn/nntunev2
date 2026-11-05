//! Real-PostgreSQL tests for observation-only content checks of an already
//! retained generation (`prepare_retained_content_check`,
//! `record_retained_content_check`, `drive_retained_content_read`).
//!
//! Included as a child of `posted_generation_retained.rs`, so it reuses that
//! module's private fixture helpers (`retained`, `Fixture`, `frozen`,
//! `health`, `seed_evidence`, `add_reference`, `is_stale`, `is_applied`).
//! The reader is SCRIPTED: this is fence/custody/cancellation proof, not
//! network, crypto or topology proof.
//!
//! Scope: there is no promotion API here. Retained states are seeded ONLY by
//! the parent's explicit fixture SQL; these tests are NOT promotion
//! acceptance. A success only stores content evidence and may bump the health
//! revision; it never asserts usability, retention, selection or cleanup.
//! Failures print only static labels; outcomes are matched, never formatted,
//! so no assertion can echo handles, permits, tokens, rows or custody.

use super::*;
use nntune_ingest::posted_generations::{
    drive_retained_content_read, record_retained_content_check, ContentCheckReport,
    ContentCheckResult, GenerationContentTransport, RetainedContentPermit, StoredGeneration,
};

type PreparedContent = Option<(RetainedContentPermit, StoredGeneration)>;
type Rows = Vec<(i64, i32, String, String, i32, i64)>;

#[derive(Clone, Copy)]
enum Answer {
    Intact,
    Unavailable,
    AuthenticationFailed,
    IntegrityFailed,
    Partial,
}

#[derive(Clone, Copy)]
enum Fault {
    Revision,
    Round,
    Token,
    AttemptLease,
    JobStatus,
    RemoveReference,
    DemoteOperation,
}

struct Hook {
    db: Client,
    job: String,
    owner: String,
    fault: Fault,
}

struct Reader {
    expected: Sealed,
    answer: Answer,
    sides: Vec<ArticleSide>,
    hook: Option<Hook>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Reader {
    fn new(s: &Sealed, answer: Answer) -> Self {
        Self {
            expected: s.clone(),
            answer,
            sides: Vec::new(),
            hook: None,
            cancel: None,
        }
    }
}

/// A separate session must be able to lock every mutable fence row while the
/// reader is awaited. NOWAIT makes a leaked transaction fail deterministically.
async fn unlocked(db: &Client, s: &Sealed, job: &str) {
    for (sql, key) in [
        (
            "SELECT 1 FROM ingest_jobs WHERE id = $1 FOR UPDATE NOWAIT",
            job,
        ),
        (
            "SELECT 1 FROM posted_file_job_attempts WHERE job_id = $1 FOR UPDATE NOWAIT",
            job,
        ),
        (
            "SELECT 1 FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE NOWAIT",
            s.hash.as_str(),
        ),
        (
            "SELECT 1 FROM posted_file_operations WHERE id = $1 FOR UPDATE NOWAIT",
            s.op.as_str(),
        ),
        (
            "SELECT 1 FROM posted_file_health WHERE generation_id = $1 FOR UPDATE NOWAIT",
            s.generation.as_str(),
        ),
        (
            "SELECT 1 FROM posted_file_references WHERE generation_id = $1 FOR UPDATE NOWAIT",
            s.generation.as_str(),
        ),
    ] {
        assert!(
            !db.query(sql, &[&key])
                .await
                .quiet("read held a fence lock")
                .is_empty(),
            "lock probe target must exist"
        );
    }
}

fn side_backbone(custody: &PostedFileParams<'_>, side: ArticleSide) -> &'static str {
    // Synthetic fixtures only ever use these two pools.
    let name = if side == ArticleSide::Post {
        custody.backbone_post
    } else {
        custody.backbone_verify
    };
    match name {
        "pool-a" => "pool-a",
        "pool-b" => "pool-b",
        _ => panic!("unexpected synthetic backbone"),
    }
}

/// The exact report the stored bundle demands for `side`.
fn good<'a>(bundle: &'a StoredGeneration, side: ArticleSide) -> ContentCheckReport<'a> {
    let custody = bundle.custody().expect("valid synthetic custody");
    ContentCheckReport {
        backbone: if side == ArticleSide::Post {
            custody.backbone_post
        } else {
            custody.backbone_verify
        },
        plan_sha256: bundle.plan_sha256(),
        plaintext_sha256: custody.plaintext_sha256,
        plaintext_size: custody.plaintext_size,
        checked_articles: custody.article_count,
    }
}

impl GenerationContentTransport for Reader {
    async fn read_content<'a>(
        &'a mut self,
        bundle: &'a StoredGeneration,
        side: ArticleSide,
    ) -> ContentCheckResult<'a> {
        assert!(
            bundle.generation_id() == self.expected.generation,
            "wrong generation"
        );
        assert!(bundle.plan_sha256() == self.expected.plan, "wrong plan");
        assert!(
            matches!(bundle.custody(), Ok(c) if c == self.expected.custody()),
            "reader did not receive the exact stored custody"
        );
        self.sides.push(side);
        if let Some(h) = self.hook.take() {
            unlocked(&h.db, &self.expected, &h.job).await;
            let s = &self.expected;
            let (sql, key): (&str, &str) = match h.fault {
                Fault::Revision => (
                    "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
                    &s.generation,
                ),
                Fault::Round => (
                    "UPDATE posted_file_health SET round = round + 1 WHERE generation_id = $1",
                    &s.generation,
                ),
                Fault::Token => (
                    "UPDATE posted_file_health SET round_token = gen_random_uuid()::text \
                     WHERE generation_id = $1",
                    &s.generation,
                ),
                Fault::AttemptLease => (
                    "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                    &h.job,
                ),
                Fault::JobStatus => (
                    "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                    &h.job,
                ),
                Fault::RemoveReference => (
                    "DELETE FROM posted_file_references WHERE generation_id = $1 \
                     AND kind = 'serving' AND owner_id = $2",
                    &s.generation,
                ),
                Fault::DemoteOperation => (
                    "UPDATE posted_file_operations SET state = 'pending' WHERE id = $1",
                    &s.op,
                ),
            };
            if matches!(h.fault, Fault::RemoveReference) {
                h.db.execute(sql, &[&key, &h.owner])
                    .await
                    .quiet("concurrent mutation during read");
            } else {
                h.db.execute(sql, &[&key])
                    .await
                    .quiet("concurrent mutation during read");
            }
        }
        if let Some(started) = self.cancel.take() {
            let _ = started.send(());
            return std::future::pending().await;
        }
        match self.answer {
            Answer::Unavailable => return ContentCheckResult::Unavailable,
            Answer::AuthenticationFailed => return ContentCheckResult::AuthenticationFailed,
            Answer::IntegrityFailed => return ContentCheckResult::IntegrityFailed,
            _ => {}
        }
        let mut result = good(bundle, side);
        if matches!(self.answer, Answer::Partial) {
            result.checked_articles -= 1;
        }
        ContentCheckResult::Intact(result)
    }
}

async fn content_rows(db: &Client, s: &Sealed) -> Rows {
    db.query(
        "SELECT round::int8, side::int4, backbone::text, state::text, checked_articles::int4, \
         checked_at::int8 FROM posted_file_content_checks WHERE generation_id = $1 \
         ORDER BY round, side",
        &[&s.generation],
    )
    .await
    .quiet("read content rows")
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
    .collect()
}

async fn prep_c(
    db: &mut Client,
    f: &Fixture,
    kind: RetainedReferenceKind,
    owner: &str,
    side: ArticleSide,
) -> Result<PreparedContent, StoreError> {
    prepare_retained_content_check(db, &f.attempt, &f.case.sealed.generation, kind, owner, side)
        .await
}

fn minted_c(
    outcome: Result<PreparedContent, StoreError>,
    ctx: &'static str,
) -> (RetainedContentPermit, StoredGeneration) {
    match outcome {
        Ok(Some(pair)) => pair,
        _ => panic!("{ctx}: expected a retained content permit"),
    }
}

async fn serving_c(
    db: &mut Client,
    f: &Fixture,
    side: ArticleSide,
    ctx: &'static str,
) -> (RetainedContentPermit, StoredGeneration) {
    let owner = f.owner.clone();
    minted_c(
        prep_c(db, f, RetainedReferenceKind::Serving, &owner, side).await,
        ctx,
    )
}

async fn drive(
    db: &mut Client,
    f: &Fixture,
    side: ArticleSide,
    reader: &mut Reader,
) -> Result<RetainedInvalidation, StoreError> {
    let owner = f.owner.clone();
    drive_retained_content_read(
        db,
        &f.attempt,
        &f.case.sealed.generation,
        RetainedReferenceKind::Serving,
        &owner,
        side,
        reader,
    )
    .await
}

/// Health fields that a successful observation must leave alone.
fn same_but_revision(a: &Health, b: &Health) -> bool {
    a.state == b.state
        && a.failure_kind == b.failure_kind
        && a.verified_at == b.verified_at
        && a.retention_checked_at == b.retention_checked_at
        && a.round == b.round
        && a.token == b.token
}

#[test]
fn postgres_retained_content_prepare_binds_exact_custody_and_success_stores_evidence_on_both_sides()
{
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let rollback = format!("pg-rollback-{}", f.case.uuid);
        add_reference(&db, &f.case.sealed, "rollback", &rollback).await;
        let s = f.case.sealed.clone();
        let owner = f.owner.clone();
        let before = frozen(&db, &s).await;
        let h0 = health(&db, &s.generation).await;
        assert_eq!(h0.state, "usable");

        // Prepare is read-only and binds the exact stored bundle for each side.
        for (kind, who) in [
            (RetainedReferenceKind::Serving, owner.as_str()),
            (RetainedReferenceKind::Rollback, rollback.as_str()),
        ] {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                let (permit, bundle) = minted_c(prep_c(&mut db, &f, kind, who, side).await, "prep");
                assert!(bundle.generation_id() == s.generation);
                assert!(bundle.plan_sha256() == s.plan);
                assert!(
                    matches!(bundle.custody(), Ok(c) if c == s.custody()),
                    "custody must be the exact stored bundle"
                );
                let shown = format!("{permit:?}");
                assert!(shown.contains("REDACTED"), "permit debug must be redacted");
                for secret in [&s.generation, &s.hash, &s.plan, &f.case.job] {
                    assert!(!shown.contains(secret.as_str()), "permit debug leaked");
                }
            }
        }
        assert!(
            before == frozen(&db, &s).await,
            "prepare and dropped permits write nothing"
        );
        assert!(h0 == health(&db, &s.generation).await);

        // Success on the post side stores evidence only.
        let (post_permit, post_bundle) = serving_c(&mut db, &f, ArticleSide::Post, "post").await;
        let outcome = record_retained_content_check(
            &mut db,
            &post_permit,
            ContentCheckResult::Intact(good(&post_bundle, ArticleSide::Post)),
        )
        .await;
        assert!(is_applied(&outcome), "applied is observation only");
        let rows = content_rows(&db, &s).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (
                rows[0].0,
                rows[0].1,
                rows[0].2.as_str(),
                rows[0].3.as_str(),
                rows[0].4
            ),
            (1, 0, "pool-a", "intact", 3),
            "full count stored in the existing round"
        );
        let h1 = health(&db, &s.generation).await;
        assert!(
            same_but_revision(&h0, &h1),
            "success never changes health meaning"
        );
        assert_eq!(h1.revision, h0.revision + 1);
        let after = frozen(&db, &s).await;
        for index in [0, 1, 2, 4] {
            assert!(before.0[index] == after.0[index], "only health changes");
        }
        assert!(before.1[0] == after.1[0] && before.1[2] == after.1[2]);
        assert!(
            is_stale(
                &record_retained_content_check(
                    &mut db,
                    &post_permit,
                    ContentCheckResult::Intact(good(&post_bundle, ArticleSide::Post)),
                )
                .await
            ),
            "the revision moved, so the original permit is spent"
        );

        // Success on the verify side through a rollback reference.
        let (verify_permit, verify_bundle) = minted_c(
            prep_c(
                &mut db,
                &f,
                RetainedReferenceKind::Rollback,
                &rollback,
                ArticleSide::Verify,
            )
            .await,
            "verify",
        );
        assert!(is_applied(
            &record_retained_content_check(
                &mut db,
                &verify_permit,
                ContentCheckResult::Intact(good(&verify_bundle, ArticleSide::Verify)),
            )
            .await
        ));
        let rows = content_rows(&db, &s).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[1].1, rows[1].2.as_str(), rows[1].3.as_str(), rows[1].4),
            (1, "pool-b", "intact", 3)
        );
        let h2 = health(&db, &s.generation).await;
        assert!(same_but_revision(&h0, &h2));
        assert_eq!(h2.revision, h0.revision + 2);

        // Identical fresh permits are idempotent: no revision or time change.
        db.execute(
            "UPDATE posted_file_content_checks SET checked_at = $2 WHERE generation_id = $1",
            &[&s.generation, &VERIFIED_AT],
        )
        .await
        .quiet("fixture pin checked_at");
        let rows = content_rows(&db, &s).await;
        let settled = frozen(&db, &s).await;
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            let (permit, bundle) = serving_c(&mut db, &f, side, "fresh").await;
            assert!(is_applied(
                &record_retained_content_check(
                    &mut db,
                    &permit,
                    ContentCheckResult::Intact(good(&bundle, side)),
                )
                .await
            ));
        }
        assert!(rows == content_rows(&db, &s).await, "no row or time change");
        assert!(h2 == health(&db, &s.generation).await, "no revision change");
        assert!(settled == frozen(&db, &s).await);
    });
}

#[test]
fn postgres_retained_content_reports_mismatching_any_dimension_are_rejected_without_writes() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        seed_evidence(&db, &s).await;
        let before = frozen(&db, &s).await;
        let rows_before = content_rows(&db, &s).await;
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            let (permit, bundle) = serving_c(&mut db, &f, side, "prepare").await;
            let custody = bundle.custody().expect("custody");
            let other = side_backbone(
                &custody,
                if side == ArticleSide::Post {
                    ArticleSide::Verify
                } else {
                    ArticleSide::Post
                },
            );
            for defect in 0..7 {
                let mut report = good(&bundle, side);
                match defect {
                    0 => report.backbone = other,
                    1 => report.backbone = "pool-alias",
                    2 => report.plan_sha256 = custody.plaintext_sha256,
                    3 => report.plaintext_sha256 = bundle.plan_sha256(),
                    4 => report.plaintext_size -= 1,
                    5 => report.checked_articles -= 1,
                    _ => report.checked_articles += 1,
                }
                let outcome = record_retained_content_check(
                    &mut db,
                    &permit,
                    ContentCheckResult::Intact(report),
                )
                .await;
                assert!(
                    matches!(outcome, Err(StoreError::InvalidInput)),
                    "every mismatched dimension is invalid input"
                );
                assert!(before == frozen(&db, &s).await, "no writes");
                assert!(rows_before == content_rows(&db, &s).await);
            }
            // The permit was not burned by the rejected reports.
            let mut reader = Reader::new(&s, Answer::Partial);
            let owner = f.owner.clone();
            let driven = drive_retained_content_read(
                &mut db,
                &f.attempt,
                &s.generation,
                RetainedReferenceKind::Serving,
                &owner,
                side,
                &mut reader,
            )
            .await;
            assert!(matches!(driven, Err(StoreError::InvalidInput)));
            assert_eq!(reader.sides, [side]);
            assert!(before == frozen(&db, &s).await);
            assert!(is_applied(
                &record_retained_content_check(
                    &mut db,
                    &permit,
                    ContentCheckResult::Intact(good(&bundle, side)),
                )
                .await
            ));
        }
    });
}

#[test]
fn postgres_retained_content_failed_states_atomically_withdraw_health_and_keep_other_evidence() {
    run_test(async {
        let mut db = connect().await;
        for (answer, state) in [
            (Answer::Unavailable, "unavailable"),
            (Answer::AuthenticationFailed, "authentication_failed"),
            (Answer::IntegrityFailed, "integrity_failed"),
        ] {
            for other_side in [false, true] {
                let f = retained(&mut db, true).await;
                let s = f.case.sealed.clone();
                seed_evidence(&db, &s).await;
                if other_side {
                    db.execute(
                        "INSERT INTO posted_file_content_checks \
                         (generation_id, round, side, backbone, state, checked_articles, \
                          checked_at) VALUES ($1, 1, 1, 'pool-b', 'intact', 3, $2)",
                        &[&s.generation, &VERIFIED_AT],
                    )
                    .await
                    .quiet("fixture verify-side evidence");
                } else {
                    db.execute(
                        "DELETE FROM posted_file_content_checks WHERE generation_id = $1",
                        &[&s.generation],
                    )
                    .await
                    .quiet("fixture clear evidence");
                }
                let before = frozen(&db, &s).await;
                let h0 = health(&db, &s.generation).await;
                let mut reader = Reader::new(&s, answer);
                let outcome = drive(&mut db, &f, ArticleSide::Post, &mut reader).await;
                assert!(
                    is_applied(&outcome),
                    "a failed read is an applied observation"
                );
                assert_eq!(reader.sides, [ArticleSide::Post]);

                let rows = content_rows(&db, &s).await;
                assert_eq!(rows.len(), 1 + usize::from(other_side));
                assert_eq!(
                    (
                        rows[0].0,
                        rows[0].1,
                        rows[0].2.as_str(),
                        rows[0].3.as_str(),
                        rows[0].4
                    ),
                    (1, 0, "pool-a", state, 0),
                    "failed state is stored with count 0 in the original round"
                );
                if other_side {
                    assert_eq!(
                        (rows[1].0, rows[1].2.as_str(), rows[1].3.as_str(), rows[1].4),
                        (1, "pool-b", "intact", 3),
                        "other side evidence is kept"
                    );
                }
                let h1 = health(&db, &s.generation).await;
                assert_eq!(h1.state, "unknown");
                assert_eq!(h1.failure_kind, None);
                assert_eq!(h1.verified_at, None);
                assert_eq!(h1.retention_checked_at, None);
                assert_eq!(h1.revision, h0.revision + 1);
                assert_eq!(h1.round, h0.round + 1);
                assert!(h1.token != h0.token, "the round token rotates");
                let after = frozen(&db, &s).await;
                for index in [0, 1, 2, 4] {
                    assert!(before.0[index] == after.0[index], "only health changes");
                }
                assert!(
                    before.1[0] == after.1[0] && before.1[2] == after.1[2],
                    "progress, budgets and schedule are kept"
                );

                // A fresh read is a new observation, never a replay of the old one.
                let mut again = Reader::new(&s, answer);
                let _ = drive(&mut db, &f, ArticleSide::Post, &mut again).await;
                assert_eq!(
                    health(&db, &s.generation).await.round,
                    h1.round + 1,
                    "a fresh read is a new observation, never a replay"
                );
            }
        }
    });
}

#[test]
fn postgres_retained_content_stale_fences_reject_the_original_callback_and_prepare() {
    run_test(async {
        let mut db = connect().await;
        for label in [
            "revision",
            "round",
            "token",
            "applied elsewhere",
            "expired attempt",
            "superseded attempt",
            "stopped job",
            "removed reference",
            "pending operation",
        ] {
            let f = retained(&mut db, true).await;
            let s = f.case.sealed.clone();
            seed_evidence(&db, &s).await;
            let (old, bundle) = serving_c(&mut db, &f, ArticleSide::Post, "prepare").await;
            let job = f.case.job.clone();
            match label {
                "revision" => {
                    db.execute(
                        "UPDATE posted_file_health SET revision = revision + 1 \
                         WHERE generation_id = $1",
                        &[&s.generation],
                    )
                    .await
                    .quiet("fixture revision drift");
                }
                "round" => {
                    db.execute(
                        "UPDATE posted_file_health SET round = round + 1 WHERE generation_id = $1",
                        &[&s.generation],
                    )
                    .await
                    .quiet("fixture round drift");
                }
                "token" => {
                    db.execute(
                        "UPDATE posted_file_health SET round_token = gen_random_uuid()::text \
                         WHERE generation_id = $1",
                        &[&s.generation],
                    )
                    .await
                    .quiet("fixture token drift");
                }
                "applied elsewhere" => {
                    let (fresh, fresh_bundle) =
                        serving_c(&mut db, &f, ArticleSide::Verify, "fresh").await;
                    assert!(is_applied(
                        &record_retained_content_check(
                            &mut db,
                            &fresh,
                            ContentCheckResult::Intact(good(&fresh_bundle, ArticleSide::Verify)),
                        )
                        .await
                    ));
                }
                "expired attempt" | "superseded attempt" => {
                    db.execute(
                        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                        &[&job],
                    )
                    .await
                    .quiet("fixture expire attempt");
                    if label == "superseded attempt" {
                        let _ = acquire(&mut db, &job, &f.case.owner, 1).await;
                    }
                }
                "stopped job" => {
                    db.execute(
                        "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                        &[&job],
                    )
                    .await
                    .quiet("fixture stop job");
                }
                "removed reference" => {
                    db.execute(
                        "DELETE FROM posted_file_references WHERE generation_id = $1 \
                         AND kind = 'serving' AND owner_id = $2",
                        &[&s.generation, &f.owner],
                    )
                    .await
                    .quiet("fixture remove reference");
                }
                _ => {
                    db.execute(
                        "UPDATE posted_file_operations SET state = 'pending' WHERE id = $1",
                        &[&s.op],
                    )
                    .await
                    .quiet("fixture demote operation");
                }
            }
            let before = frozen(&db, &s).await;
            let rows = content_rows(&db, &s).await;
            let h = health(&db, &s.generation).await;
            for result in [
                ContentCheckResult::Intact(good(&bundle, ArticleSide::Post)),
                ContentCheckResult::Unavailable,
            ] {
                assert!(
                    is_stale(&record_retained_content_check(&mut db, &old, result).await),
                    "{label}: the original callback must be stale"
                );
                assert!(before == frozen(&db, &s).await, "{label}: no writes");
                assert!(rows == content_rows(&db, &s).await);
                assert!(h == health(&db, &s.generation).await);
            }
            if !matches!(label, "revision" | "round" | "token" | "applied elsewhere") {
                // The prepare-time fences are gone too: no permit, no read.
                let mut reader = Reader::new(&s, Answer::Intact);
                let outcome = drive(&mut db, &f, ArticleSide::Post, &mut reader).await;
                assert!(is_stale(&outcome), "{label}: the driver must be stale");
                assert!(reader.sides.is_empty(), "{label}: no read may start");
                assert!(before == frozen(&db, &s).await);
            }
        }
    });
}

#[test]
fn postgres_retained_content_same_permit_on_two_connections_applies_exactly_once() {
    run_test(async {
        let mut db = connect().await;
        for failure in [false, true] {
            let f = retained(&mut db, true).await;
            let s = f.case.sealed.clone();
            let (permit, bundle) = serving_c(&mut db, &f, ArticleSide::Post, "prepare").await;
            let h0 = health(&db, &s.generation).await;
            fn result(failure: bool, bundle: &StoredGeneration) -> ContentCheckResult<'_> {
                if failure {
                    ContentCheckResult::IntegrityFailed
                } else {
                    ContentCheckResult::Intact(good(bundle, ArticleSide::Post))
                }
            }
            let mut left = connect().await;
            let mut right = connect().await;
            let (a, b) = tokio::join!(
                record_retained_content_check(&mut left, &permit, result(failure, &bundle)),
                record_retained_content_check(&mut right, &permit, result(failure, &bundle))
            );
            assert_eq!(
                [&a, &b].iter().filter(|r| is_applied(r)).count(),
                1,
                "exactly one callback applies"
            );
            assert!(is_stale(&a) || is_stale(&b), "the loser is stale");
            let h1 = health(&db, &s.generation).await;
            assert_eq!(h1.revision, h0.revision + 1, "one revision bump");
            assert_eq!(
                h1.round,
                h0.round + i64::from(failure),
                "one withdrawal at most"
            );
            assert_eq!(content_rows(&db, &s).await.len(), 1);
        }
    });
}

#[test]
fn postgres_retained_content_concurrent_mutation_during_async_read_stales_callback_without_locks() {
    run_test(async {
        let mut db = connect().await;
        for fault in [
            Fault::Revision,
            Fault::Round,
            Fault::Token,
            Fault::AttemptLease,
            Fault::JobStatus,
            Fault::RemoveReference,
            Fault::DemoteOperation,
        ] {
            for answer in [Answer::Intact, Answer::Unavailable] {
                let f = retained(&mut db, true).await;
                let s = f.case.sealed.clone();
                seed_evidence(&db, &s).await;
                let rows = content_rows(&db, &s).await;
                let mut reader = Reader::new(&s, answer);
                reader.hook = Some(Hook {
                    db: connect().await,
                    job: f.case.job.clone(),
                    owner: f.owner.clone(),
                    fault,
                });
                let outcome = drive(&mut db, &f, ArticleSide::Post, &mut reader).await;
                assert!(
                    is_stale(&outcome),
                    "the original permit must not rebind after the read"
                );
                assert_eq!(reader.sides.len(), 1);
                assert!(
                    rows == content_rows(&db, &s).await,
                    "a stale callback wrote content"
                );
                let h = health(&db, &s.generation).await;
                assert_eq!(
                    h.state, "usable",
                    "a stale callback must not withdraw health"
                );
                assert_eq!(h.verified_at, Some(VERIFIED_AT));
                assert_eq!(custody_rows(&db, &s).await, 1);
            }
        }
    });
}

#[test]
fn postgres_retained_content_cancelled_read_writes_no_observation() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        seed_evidence(&db, &s).await;
        let before = frozen(&db, &s).await;
        let h0 = health(&db, &s.generation).await;
        let (sender, started) = tokio::sync::oneshot::channel();
        let mut reader = Reader::new(&s, Answer::Unavailable);
        reader.cancel = Some(sender);
        tokio::select! {
            _ = drive(&mut db, &f, ArticleSide::Post, &mut reader) => panic!("read must stay pending"),
            result = started => result.expect("reader started"),
        }
        assert!(before == frozen(&db, &s).await, "cancel writes nothing");
        assert!(h0 == health(&db, &s.generation).await);

        // The unchanged fence still serves a complete read afterwards.
        let outcome = drive(&mut db, &f, ArticleSide::Post, &mut reader).await;
        assert!(is_applied(&outcome));
        assert_eq!(reader.sides.len(), 2);
    });
}

#[test]
fn postgres_retained_content_two_intact_sides_never_upgrade_unknown_and_bad_stored_identity_refuses(
) {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, false).await;
        let s = f.case.sealed.clone();
        let negative = serving(&mut db, &f, "prepare unknown").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &negative).await
        ));
        let h0 = health(&db, &s.generation).await;
        let before = frozen(&db, &s).await;
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            let mut reader = Reader::new(&s, Answer::Intact);
            assert!(is_applied(&drive(&mut db, &f, side, &mut reader).await));
        }
        let h1 = health(&db, &s.generation).await;
        assert!(same_but_revision(&h0, &h1));
        assert_eq!(h1.state, "unknown");
        assert_eq!(h1.verified_at, None);
        assert_eq!(h1.retention_checked_at, None);
        assert_eq!(h1.revision, h0.revision + 2);
        let after = frozen(&db, &s).await;
        for index in [0, 1, 2, 4] {
            assert!(before.0[index] == after.0[index], "no authority change");
        }
        assert_eq!(content_rows(&db, &s).await.len(), 2);

        db.execute(
            "UPDATE posted_file_content_checks SET backbone = 'pool-wrong' \
             WHERE generation_id = $1 AND round = $2 AND side = 0",
            &[&s.generation, &h1.round],
        )
        .await
        .quiet("fixture inconsistent observation identity");
        let before = frozen(&db, &s).await;
        let mut reader = Reader::new(&s, Answer::Intact);
        assert!(matches!(
            drive(&mut db, &f, ArticleSide::Post, &mut reader).await,
            Err(StoreError::Inconsistent)
        ));
        assert!(
            before == frozen(&db, &s).await,
            "inconsistency writes nothing"
        );
    });
}

#[test]
fn postgres_retained_content_prepare_refuses_round_zero_pending_and_unknown_references() {
    run_test(async {
        let mut db = connect().await;

        // Round 0: never verified, so no content observation is accepted.
        let f = retained(&mut db, false).await;
        let s = f.case.sealed.clone();
        assert_eq!(health(&db, &s.generation).await.round, 0);
        let before = frozen(&db, &s).await;
        let owner = f.owner.clone();
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            assert!(
                matches!(
                    prep_c(&mut db, &f, RetainedReferenceKind::Serving, &owner, side).await,
                    Ok(None)
                ),
                "round 0 must be refused"
            );
            let mut reader = Reader::new(&s, Answer::Intact);
            assert!(is_stale(&drive(&mut db, &f, side, &mut reader).await));
            assert!(reader.sides.is_empty(), "no read at round 0");
        }
        assert!(before == frozen(&db, &s).await, "refusal writes nothing");

        // Pending candidate, even with fake serving and rollback references.
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        admitted(&mut db, &attempt, &c.sealed).await;
        let s = &c.sealed;
        let serving_owner = format!("pg-serving-{}", c.uuid);
        let rollback_owner = format!("pg-rollback-{}", c.uuid);
        add_reference(&db, s, "serving", &serving_owner).await;
        add_reference(&db, s, "rollback", &rollback_owner).await;
        let before = frozen(&db, s).await;
        for (kind, who) in [
            (RetainedReferenceKind::Serving, serving_owner.as_str()),
            (RetainedReferenceKind::Rollback, rollback_owner.as_str()),
        ] {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                let outcome = prepare_retained_content_check(
                    &mut db,
                    &attempt,
                    &s.generation,
                    kind,
                    who,
                    side,
                )
                .await;
                assert!(matches!(outcome, Ok(None)), "pending candidate refused");
            }
        }
        assert!(before == frozen(&db, s).await);

        // Wrong kind, wrong owner, missing and malformed identities.
        let f = retained(&mut db, true).await;
        let s = f.case.sealed.clone();
        let before = frozen(&db, &s).await;
        let owner = f.owner.clone();
        for (kind, who) in [
            (RetainedReferenceKind::Rollback, owner.as_str()),
            (RetainedReferenceKind::LegacyPin, owner.as_str()),
            (RetainedReferenceKind::Serving, "pg-other-owner"),
        ] {
            assert!(matches!(
                prep_c(&mut db, &f, kind, who, ArticleSide::Post).await,
                Ok(None)
            ));
        }
        for (generation, who) in [("", owner.as_str()), (s.generation.as_str(), "bad owner")] {
            let outcome = prepare_retained_content_check(
                &mut db,
                &f.attempt,
                generation,
                RetainedReferenceKind::Serving,
                who,
                ArticleSide::Post,
            )
            .await;
            assert!(matches!(outcome, Err(StoreError::InvalidInput)));
        }
        assert!(before == frozen(&db, &s).await);
    });
}
