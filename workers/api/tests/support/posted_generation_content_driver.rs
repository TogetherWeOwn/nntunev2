//! Complete-read orchestration against the real adapter, with a SCRIPTED reader.
//! This is fence/custody/cancellation proof, not network, crypto or topology proof.

use super::*;
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
        let before = snapshot(&db, &c.sealed).await;
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
            assert!(snapshot(&db, &c.sealed).await == before);
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
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        assert!(matches!(
            drive_content_read(&mut db, &original, ArticleSide::Post, &mut reader).await,
            Ok(ContentReadStep::StaleOrDiverged)
        ));
        let next = applied(
            begin_verification_round(&mut db, &original).await,
            "begin content round",
        );
        assert!(matches!(
            drive_content_read(&mut db, &original, ArticleSide::Post, &mut reader).await,
            Ok(ContentReadStep::StaleOrDiverged)
        ));
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
        let before = snapshot(&db, &c.sealed).await;
        let (sender, started) = tokio::sync::oneshot::channel();
        let mut reader = Reader::new(&c.sealed, Answer::Intact);
        reader.cancel = Some(sender);
        tokio::select! {
            _ = drive_content_read(&mut db, &handle, ArticleSide::Post, &mut reader) => panic!("read must stay pending"),
            result = started => result.expect("reader started"),
        }
        assert!(snapshot(&db, &c.sealed).await == before);
        assert!(content_rows(&db, &c.sealed).await.is_empty());
        let recovered = fresh_handle(&mut db, &attempt, &c.sealed).await;
        let _ =
            recorded(drive_content_read(&mut db, &recovered, ArticleSide::Post, &mut reader).await);
        assert_eq!(reader.sides.len(), 2);
        assert_eq!(custody_rows(&db, &c.sealed).await, 1);
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
