//! Complete-read orchestration against the real adapter, with a SCRIPTED reader.
//! This is fence/custody/cancellation proof, not network, crypto or topology proof.

use super::*;

#[path = "posted_generation_content_delivery_races.rs"]
mod delivery_races;

use nntune_ingest::posted_generations::{
    drive_content_read, ContentReadStep, GenerationContentTransport, StoredGeneration,
};

#[derive(Clone, Copy)]
enum Answer {
    Intact,
    Unavailable,
    AuthenticationFailed,
    IntegrityFailed,
    Partial,
    WrongBackbone,
    WrongDigest,
    WrongPlan,
    WrongSize,
}

#[derive(Clone, Copy)]
enum Fault {
    Revision,
    Round,
    OperationLease,
    AttemptLease,
    JobStatus,
    Takeover,
    RenewedLeases,
}

struct Hook {
    db: Client,
    handle: GenerationHandle,
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

// A separate session must be able to lock every mutable fence row while the
// reader is awaited. NOWAIT makes a leaked transaction fail deterministically.
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
            let mut h = h;
            unlocked(&h.db, &self.expected, &h.job).await;
            match h.fault {
                Fault::RenewedLeases => {
                    let row =
                        h.db.query_one(
                            "SELECT a.lease_until, o.lease_until, \
                         extract(epoch FROM clock_timestamp())::bigint \
                         FROM posted_file_job_attempts a \
                         JOIN posted_file_operations o ON o.job_id = a.job_id WHERE o.id = $1",
                            &[&self.expected.op],
                        )
                        .await
                        .quiet("read renewed content delivery leases");
                    assert_eq!(row.get::<_, i64>(0), row.get::<_, i64>(1));
                    assert!(row.get::<_, i64>(0) >= row.get::<_, i64>(2) + 290);
                }
                Fault::Revision => {
                    applied(
                        confirm_article(&mut h.db, &h.handle, 0, ArticleSide::Verify).await,
                        "concurrent presence revision",
                    );
                }
                Fault::Round => {
                    applied(
                        begin_verification_round(&mut h.db, &h.handle).await,
                        "concurrent content round",
                    );
                }
                Fault::OperationLease => {
                    h.db.execute(
                        "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                        &[&self.expected.op],
                    )
                    .await
                    .quiet("expire operation during read");
                }
                Fault::AttemptLease | Fault::Takeover => {
                    h.db.execute(
                        "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                        &[&h.job],
                    )
                    .await
                    .quiet("expire attempt during read");
                    if matches!(h.fault, Fault::Takeover) {
                        h.db.execute(
                            "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                            &[&self.expected.op],
                        )
                        .await
                        .quiet("expire operation for takeover");
                        let next = acquire(&mut h.db, &h.job, &h.owner, 1).await;
                        let expected = observe(&h.db, &self.expected.hash)
                            .await
                            .expect("observe pending candidate");
                        assert!(
                            matches!(
                                take_over(&mut h.db, &next, &expected, &self.expected.op).await,
                                Ok(AdmissionOutcome::ResumedSameCandidate(_, _))
                            ),
                            "concurrent takeover"
                        );
                    }
                }
                Fault::JobStatus => {
                    h.db.execute(
                        "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
                        &[&h.job],
                    )
                    .await
                    .quiet("change job during read");
                }
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
        let custody = bundle.custody().expect("valid synthetic custody");
        let mut result = ContentCheckReport {
            backbone: if side == ArticleSide::Post {
                custody.backbone_post
            } else {
                custody.backbone_verify
            },
            plan_sha256: bundle.plan_sha256(),
            plaintext_sha256: custody.plaintext_sha256,
            plaintext_size: custody.plaintext_size,
            checked_articles: custody.article_count,
        };
        match self.answer {
            Answer::Partial => result.checked_articles -= 1,
            Answer::WrongBackbone => result.backbone = "pool-alias",
            Answer::WrongDigest => result.plaintext_sha256 = bundle.plan_sha256(),
            Answer::WrongPlan => result.plan_sha256 = custody.plaintext_sha256,
            Answer::WrongSize => result.plaintext_size -= 1,
            _ => {}
        }
        ContentCheckResult::Intact(result)
    }
}

// A delivery may renew deadlines even when the read is cancelled or its report
// is malformed. Every other operation/attempt field and all evidence stay pinned.
async fn delivery_snapshot(db: &Client, c: &Case) -> Vec<Option<String>> {
    let mut snap = snapshot(db, &c.sealed).await;
    let row = db.query_one(
        "SELECT \
         (SELECT md5((to_jsonb(o) - 'lease_until')::text) FROM posted_file_operations o WHERE o.id = $1), \
         (SELECT md5((to_jsonb(a) - 'lease_until')::text) FROM posted_file_job_attempts a WHERE a.job_id = $2), \
         (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round, p.slot, p.side)::text) \
            FROM posted_file_article_progress p WHERE p.generation_id = $3), \
         (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round, p.side)::text) \
            FROM posted_file_content_checks p WHERE p.generation_id = $3), \
         (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round)::text) \
            FROM posted_file_recheck_schedule p WHERE p.generation_id = $3)",
        &[&c.sealed.op, &c.job, &c.sealed.generation],
    ).await.quiet("snapshot content delivery invariants");
    snap[2] = row.get(0);
    snap.extend((1..5).map(|i| row.get::<_, Option<String>>(i)));
    snap
}

async fn exact_delivery_snapshot(db: &Client, c: &Case) -> Vec<Option<String>> {
    let mut snap = delivery_snapshot(db, c).await;
    let row = db
        .query_one(
            "SELECT md5(a::text), md5(o::text) FROM posted_file_job_attempts a \
         JOIN posted_file_operations o ON o.job_id = a.job_id WHERE o.id = $1",
            &[&c.sealed.op],
        )
        .await
        .quiet("snapshot delivery including deadlines");
    snap.extend((0..2).map(|i| row.get::<_, Option<String>>(i)));
    snap
}

async fn short_leases(db: &Client, c: &Case, attempt_seconds: i64, operation_seconds: i64) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = \
         extract(epoch FROM clock_timestamp())::bigint + $2 WHERE job_id = $1",
        &[&c.job, &attempt_seconds],
    )
    .await
    .quiet("set content attempt deadline");
    db.execute(
        "UPDATE posted_file_operations SET lease_until = \
         extract(epoch FROM clock_timestamp())::bigint + $2 WHERE id = $1",
        &[&c.sealed.op, &operation_seconds],
    )
    .await
    .quiet("set content operation deadline");
}

fn recorded(result: Result<ContentReadStep, StoreError>) -> GenerationHandle {
    match result {
        Ok(ContentReadStep::ObservationRecorded(next)) => *next,
        _ => panic!("expected recorded content observation"),
    }
}

#[test]
fn postgres_content_driver_binds_stored_bundle_and_side_without_granting_authority() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, handle) = verifying(&mut db).await;
        let held = guard(&db, &c.sealed).await;
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        let next =
            recorded(drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await);
        let next =
            recorded(drive_content_read(&mut db, &next, ArticleSide::Verify, &mut reader).await);
        assert_eq!(reader.sides, [ArticleSide::Post, ArticleSide::Verify]);
        for post in [true, false] {
            assert_eq!(
                content_state(&mut db, &next, post).await,
                Some(ContentCheckState::Intact)
            );
        }
        assert!(
            guard(&db, &c.sealed).await == held,
            "content read must not grant authority"
        );
        assert!(content_rows(&db, &c.sealed).await.iter().all(|r| r.2 == 3));
    });
}

#[test]
fn postgres_content_driver_explicit_rechecks_invalidate_prior_intact_on_every_failure() {
    run_test(async {
        let mut db = connect().await;
        for (answer, expected) in [
            (Answer::Unavailable, ContentCheckState::Unavailable),
            (
                Answer::AuthenticationFailed,
                ContentCheckState::AuthenticationFailed,
            ),
            (Answer::IntegrityFailed, ContentCheckState::IntegrityFailed),
        ] {
            let (c, _, handle) = verifying(&mut db).await;
            let held = guard(&db, &c.sealed).await;
            let mut reader = Reader::new(&c.sealed, Answer::Intact);
            let next = recorded(
                drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
            );
            reader.answer = answer;
            let next =
                recorded(drive_content_read(&mut db, &next, ArticleSide::Post, &mut reader).await);
            assert_eq!(
                reader.sides.len(),
                2,
                "prior intact must not skip the recheck"
            );
            assert_eq!(content_state(&mut db, &next, true).await, Some(expected));
            assert_eq!(content_state(&mut db, &next, false).await, None);
            assert!(content_rows(&db, &c.sealed).await.iter().all(|r| r.2 == 0));
            assert!(guard(&db, &c.sealed).await == held);
        }
    });
}

#[test]
fn postgres_content_driver_rejects_partial_and_mismatched_reports_without_writes() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, handle) = verifying(&mut db).await;
        let before = delivery_snapshot(&db, &c).await;
        for answer in [
            Answer::Partial,
            Answer::WrongBackbone,
            Answer::WrongDigest,
            Answer::WrongPlan,
            Answer::WrongSize,
        ] {
            let mut reader = Reader::new(&c.sealed, answer);
            assert!(matches!(
                drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
                Err(StoreError::InvalidInput)
            ));
            assert_eq!(reader.sides.len(), 1);
            assert!(delivery_snapshot(&db, &c).await == before);
            assert!(content_rows(&db, &c.sealed).await.is_empty());
        }
    });
}

#[test]
fn postgres_content_driver_unbegun_and_stale_handles_do_not_start_a_read() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let original = admitted(&mut db, &attempt, &c.sealed).await;
        short_leases(&db, &c, 30, 40).await;
        let before = exact_delivery_snapshot(&db, &c).await;
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        assert!(matches!(
            drive_content_read(&mut db, &original, ArticleSide::Post, &mut reader).await,
            Ok(ContentReadStep::StaleOrDiverged)
        ));
        assert!(exact_delivery_snapshot(&db, &c).await == before);
        let next = applied(
            begin_verification_round(&mut db, &original).await,
            "begin content round",
        );
        let before = exact_delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_content_read(&mut db, &original, ArticleSide::Post, &mut reader).await,
            Ok(ContentReadStep::StaleOrDiverged)
        ));
        assert!(exact_delivery_snapshot(&db, &c).await == before);
        assert!(reader.sides.is_empty());
        let _ = recorded(drive_content_read(&mut db, &next, ArticleSide::Post, &mut reader).await);
        assert_eq!(reader.sides.len(), 1);
    });
}

#[test]
fn postgres_content_driver_concurrent_fence_changes_reject_original_read_callbacks_without_locks() {
    run_test(async {
        let mut db = connect().await;
        for fault in [
            Fault::Revision,
            Fault::Round,
            Fault::OperationLease,
            Fault::AttemptLease,
            Fault::JobStatus,
            Fault::Takeover,
        ] {
            for answer in [Answer::Intact, Answer::Unavailable] {
                let (c, _, handle) = verifying(&mut db).await;
                let mut reader = Reader::new(&c.sealed, answer);
                reader.hook = Some(Hook {
                    db: connect().await,
                    handle: handle.clone(),
                    job: c.job.clone(),
                    owner: c.owner.clone(),
                    fault,
                });
                assert!(matches!(
                    drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
                    Ok(ContentReadStep::StaleOrDiverged)
                ));
                assert_eq!(reader.sides.len(), 1);
                assert!(
                    content_rows(&db, &c.sealed).await.is_empty(),
                    "stale callback wrote content"
                );
                assert_eq!(custody_rows(&db, &c.sealed).await, 1);
            }
        }
    });
}

#[test]
fn postgres_content_driver_cancelled_read_writes_no_observation_and_recovery_reuses_custody() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = verifying(&mut db).await;
        let before = delivery_snapshot(&db, &c).await;
        let (sender, started) = tokio::sync::oneshot::channel();
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        reader.cancel = Some(sender);
        tokio::select! {
            _ = drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader) => panic!("read must stay pending"),
            result = started => result.expect("reader started"),
        }
        assert!(delivery_snapshot(&db, &c).await == before);
        assert!(content_rows(&db, &c.sealed).await.is_empty());
        let recovered = fresh_handle(&mut db, &attempt, &c.sealed).await;
        let _ =
            recorded(drive_content_read(&mut db, &recovered, ArticleSide::Post, &mut reader).await);
        assert_eq!(reader.sides.len(), 2);
        assert_eq!(custody_rows(&db, &c.sealed).await, 1);
    });
}

#[test]
fn postgres_content_driver_renews_both_leases_before_read_and_cancel_preserves_all_other_state() {
    run_test(async {
        let mut db = connect().await;
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            let (c, attempt, handle) = verifying(&mut db).await;
            short_leases(&db, &c, 30, 40).await;
            let before = delivery_snapshot(&db, &c).await;
            let (sender, started) = tokio::sync::oneshot::channel();
            let mut reader = Reader::new(&c.sealed, Answer::Intact);
            reader.cancel = Some(sender);
            reader.hook = Some(Hook {
                db: connect().await,
                handle: handle.clone(),
                job: c.job.clone(),
                owner: c.owner.clone(),
                fault: Fault::RenewedLeases,
            });
            tokio::select! {
                _ = drive_content_read(&mut db, &handle, side, &mut reader) => panic!("read must stay pending"),
                result = started => result.expect("reader started after renewal"),
            }
            assert_eq!(reader.sides, [side]);
            assert!(delivery_snapshot(&db, &c).await == before);
            assert!(content_rows(&db, &c.sealed).await.is_empty());
            let recovered = fresh_handle(&mut db, &attempt, &c.sealed).await;
            assert!(
                recovered == handle,
                "renewal must not rotate original authority"
            );
            let _ = recorded(drive_content_read(&mut db, &recovered, side, &mut reader).await);
            assert_eq!(reader.sides, [side, side]);
            assert_eq!(content_rows(&db, &c.sealed).await.len(), 1);
            assert_eq!(custody_rows(&db, &c.sealed).await, 1);
        }
    });
}

#[test]
fn postgres_content_driver_expired_lease_refuses_read_without_partial_renewal() {
    run_test(async {
        let mut db = connect().await;
        for (attempt, operation) in [(-1, 40), (30, -1)] {
            let (c, _, handle) = verifying(&mut db).await;
            short_leases(&db, &c, attempt, operation).await;
            let before = snapshot(&db, &c.sealed).await;
            let attempt_before: i64 = db
                .query_one(
                    "SELECT lease_until FROM posted_file_job_attempts WHERE job_id = $1",
                    &[&c.job],
                )
                .await
                .quiet("read expired attempt fixture")
                .get(0);
            let mut reader = Reader::new(&c.sealed, Answer::Intact);
            assert!(matches!(
                drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
                Ok(ContentReadStep::StaleOrDiverged)
            ));
            assert!(reader.sides.is_empty());
            assert!(snapshot(&db, &c.sealed).await == before);
            let attempt_after: i64 = db
                .query_one(
                    "SELECT lease_until FROM posted_file_job_attempts WHERE job_id = $1",
                    &[&c.job],
                )
                .await
                .quiet("read refused attempt deadline")
                .get(0);
            assert_eq!(attempt_before, attempt_after);
        }
    });
}

#[test]
fn postgres_content_permit_preparation_alone_does_not_renew_delivery_leases() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, handle) = verifying(&mut db).await;
        short_leases(&db, &c, 30, 40).await;
        let before = snapshot(&db, &c.sealed).await;
        let attempt_before: i64 = db
            .query_one(
                "SELECT lease_until FROM posted_file_job_attempts WHERE job_id = $1",
                &[&c.job],
            )
            .await
            .quiet("read preparation attempt deadline")
            .get(0);
        assert!(prepare_content_check(&mut db, &handle, ArticleSide::Post)
            .await
            .unwrap_or_else(|_| panic!("prepare only"))
            .is_some());
        assert!(snapshot(&db, &c.sealed).await == before);
        let attempt_after: i64 = db
            .query_one(
                "SELECT lease_until FROM posted_file_job_attempts WHERE job_id = $1",
                &[&c.job],
            )
            .await
            .quiet("read unchanged preparation deadline")
            .get(0);
        assert_eq!(attempt_before, attempt_after);
        assert!(content_rows(&db, &c.sealed).await.is_empty());
    });
}

#[test]
fn postgres_content_driver_lost_ack_recovery_rechecks_idempotently_without_reminting() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = verifying(&mut db).await;
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        // Lose the successful response rather than claiming a wire-level commit fault.
        drop(recorded(
            drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
        ));
        let recovered = fresh_handle(&mut db, &attempt, &c.sealed).await;
        let revision = health_revision(&db, &c.sealed).await;
        let next =
            recorded(drive_content_read(&mut db, &recovered, ArticleSide::Post, &mut reader).await);
        assert!(next == recovered);
        assert_eq!(health_revision(&db, &c.sealed).await, revision);
        assert_eq!(content_rows(&db, &c.sealed).await.len(), 1);
        assert!(matches!(
            drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader).await,
            Ok(ContentReadStep::StaleOrDiverged)
        ));
        assert_eq!(
            reader.sides.len(),
            2,
            "old handle must not issue another read"
        );
        assert_eq!(custody_rows(&db, &c.sealed).await, 1);
        assert_eq!(operation_rows(&db, &c.sealed).await, 1);
    });
}
