//! Recovery and exact-mint replay must sample live fences after lock waits.
//! These are disposable independent-session controls, not network/drain proof.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, recover_pending, ArticleSide, MutationOutcome,
};

#[derive(Clone, Copy)]
enum Entry {
    Recover,
    Replay,
    Pending,
}

#[derive(Clone, Copy)]
enum Barrier {
    Registry,
    Health,
}

#[derive(Clone, Copy)]
enum Change {
    None,
    AttemptExpiry,
    OperationExpiry,
    Revision,
    Round,
    RoundToken,
    RegistryRevision,
    PendingRemoved,
}

async fn resume(
    db: &mut Client,
    entry: Entry,
    attempt: &AttemptHandle,
    expected: &Observation,
    sealed: &Sealed,
) -> Result<AdmissionOutcome, StoreError> {
    match entry {
        Entry::Recover => recover(db, attempt, &sealed.hash, &sealed.op).await,
        Entry::Replay => admit(db, attempt, expected, &sealed.mint()).await,
        Entry::Pending => recover_pending(db, attempt, &sealed.hash).await,
    }
}

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read recovery backend")
        .get(0)
}

async fn blocked_by(db: &Client, waiter: i32, holder: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = db
                .query_one("SELECT $2 = ANY(pg_blocking_pids($1))", &[&waiter, &holder])
                .await
                .quiet("observe recovery lock barrier")
                .get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery lock barrier timed out");
}

async fn frozen(db: &impl tokio_postgres::GenericClient, c: &Case) -> String {
    db.query_one(
        "SELECT md5(jsonb_build_array( \
         (SELECT to_jsonb(t) FROM ingest_jobs t WHERE t.id = $1), \
         (SELECT to_jsonb(t) FROM posted_file_job_attempts t WHERE t.job_id = $1), \
         (SELECT to_jsonb(t) FROM posted_files t WHERE t.id = $2), \
         (SELECT to_jsonb(t) FROM posted_file_registry t WHERE t.plaintext_sha256 = $3), \
         (SELECT to_jsonb(t) FROM posted_file_operations t WHERE t.id = $4), \
         (SELECT to_jsonb(t) FROM posted_file_health t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.kind, t.owner_id) \
            FROM posted_file_references t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.slot, t.side) \
            FROM posted_file_article_progress t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round, t.side) \
            FROM posted_file_content_checks t WHERE t.generation_id = $2), \
         (SELECT jsonb_agg(to_jsonb(t) ORDER BY t.round) \
            FROM posted_file_recheck_schedule t WHERE t.generation_id = $2) \
         )::text)",
        &[&c.job, &c.sealed.generation, &c.sealed.hash, &c.sealed.op],
    )
    .await
    .quiet("snapshot recovery invariants")
    .get(0)
}

async fn interleave(entry: Entry, barrier: Barrier, change: Change) {
    let mut db = connect().await;
    let c = case(&db).await;
    let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
    let handle = admitted(&mut db, &attempt, &c.sealed).await;
    let MutationOutcome::Applied(handle) = begin_verification_round(&mut db, &handle)
        .await
        .expect("begin recovery fixture round")
    else {
        panic!("recovery fixture round refused")
    };
    assert!(matches!(
        confirm_article(&mut db, &handle, 0, ArticleSide::Post).await,
        Ok(MutationOutcome::Applied(_))
    ));
    let expected = observe(&db, &c.sealed.hash)
        .await
        .expect("observe recovery fixture");
    let now: i64 = db
        .query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
        .await
        .quiet("read recovery database clock")
        .get(0);
    let deadline = now + 3;
    let attempt_until = if matches!(change, Change::AttemptExpiry) {
        deadline
    } else {
        now + 300
    };
    let operation_until = if matches!(change, Change::OperationExpiry) {
        deadline
    } else {
        now + 300
    };
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = $2 WHERE job_id = $1",
        &[&c.job, &attempt_until],
    )
    .await
    .quiet("set recovery attempt fixture lease");
    db.execute(
        "UPDATE posted_file_operations SET lease_until = $2 WHERE id = $1",
        &[&c.sealed.op, &operation_until],
    )
    .await
    .quiet("set recovery operation fixture lease");
    let before = frozen(&db, &c).await;
    let mut blocker = connect().await;
    let blocker_pid = backend(&blocker).await;
    let tx = blocker.transaction().await.quiet("begin recovery barrier");
    match barrier {
        Barrier::Registry => {
            tx.query_one(
                "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE",
                &[&c.sealed.hash],
            )
            .await
            .quiet("hold recovery registry row");
        }
        Barrier::Health => {
            tx.query_one(
                "SELECT revision FROM posted_file_health WHERE generation_id = $1 FOR UPDATE",
                &[&c.sealed.generation],
            )
            .await
            .quiet("hold recovery health row");
        }
    }
    let mut worker = connect().await;
    let worker_pid = backend(&worker).await;
    let (outcome, after_fixture) = tokio::join!(
        resume(&mut worker, entry, &attempt, &expected, &c.sealed),
        async {
            blocked_by(&db, worker_pid, blocker_pid).await;
            assert!(
                before == frozen(&tx, &c).await,
                "recovery wrote before barrier"
            );
            match change {
                Change::AttemptExpiry | Change::OperationExpiry => {
                    let live: bool = tx
                        .query_one(
                            "SELECT extract(epoch FROM clock_timestamp())::bigint < $1",
                            &[&deadline],
                        )
                        .await
                        .quiet("verify recovery reached barrier before expiry")
                        .get(0);
                    assert!(live, "recovery did not reach barrier with live leases");
                    tx.query_one(
                        "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - \
                             extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                        &[&deadline],
                    )
                    .await
                    .quiet("expire recovery lease on database clock");
                }
                Change::Revision | Change::Round | Change::RoundToken => {
                    let sql = match change {
                        Change::Revision => "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
                        Change::Round => "UPDATE posted_file_health SET round = round + 1 WHERE generation_id = $1",
                        Change::RoundToken => "UPDATE posted_file_health SET round_token = gen_random_uuid()::text WHERE generation_id = $1",
                        _ => unreachable!(),
                    };
                    tx.execute(sql, &[&c.sealed.generation])
                        .await
                        .quiet("change one recovery health fence");
                }
                Change::RegistryRevision | Change::PendingRemoved => {
                    let sql = match change {
                        Change::RegistryRevision => "UPDATE posted_file_registry SET revision = revision + 1 WHERE plaintext_sha256 = $1",
                        Change::PendingRemoved => "UPDATE posted_file_registry SET pending_operation = NULL WHERE plaintext_sha256 = $1",
                        _ => unreachable!(),
                    };
                    tx.execute(sql, &[&c.sealed.hash])
                        .await
                        .quiet("change discovered registry fence");
                }
                Change::None => {}
            }
            let snapshot = frozen(&tx, &c).await;
            tx.commit().await.quiet("release recovery barrier");
            snapshot
        }
    );
    if matches!(change, Change::None) {
        let Ok(AdmissionOutcome::ResumedSameCandidate(handle, bundle)) = outcome else {
            panic!("live recovery did not resume")
        };
        assert!(handle.generation_id() == c.sealed.generation);
        assert!(handle.operation_id() == c.sealed.op);
        assert!(bundle.generation_id() == c.sealed.generation);
        assert!(bundle.plan_sha256() == c.sealed.plan);
        assert!(bundle.custody().expect("recovered custody") == c.sealed.custody());
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("live recovered posting fence"));
    } else {
        assert!(matches!(outcome, Ok(AdmissionOutcome::StaleOrDiverged)));
    }
    assert!(
        after_fixture == frozen(&db, &c).await,
        "recovery changed durable state"
    );
}

#[test]
fn postgres_pending_discovery_rejects_registry_changes_between_discovery_and_recovery() {
    run_test(async {
        for change in [Change::RegistryRevision, Change::PendingRemoved] {
            interleave(Entry::Pending, Barrier::Registry, change).await;
        }
    });
}

#[test]
fn postgres_pending_discovery_rechecks_both_leases_after_registry_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Pending, Barrier::Registry, change).await;
        }
    });
}

#[test]
fn postgres_pending_discovery_rechecks_both_leases_after_health_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Pending, Barrier::Health, change).await;
        }
    });
}

#[test]
fn postgres_recovery_rechecks_both_leases_after_registry_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Recover, Barrier::Registry, change).await;
        }
    });
}

#[test]
fn postgres_admission_replay_rechecks_both_leases_after_registry_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Replay, Barrier::Registry, change).await;
        }
    });
}

#[test]
fn postgres_recovery_rechecks_both_leases_after_health_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Recover, Barrier::Health, change).await;
        }
    });
}

#[test]
fn postgres_admission_replay_rechecks_both_leases_after_health_wait() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            interleave(Entry::Replay, Barrier::Health, change).await;
        }
    });
}

#[test]
fn postgres_recovery_and_replay_reject_each_changed_health_fence_after_wait() {
    run_test(async {
        for entry in [Entry::Recover, Entry::Replay, Entry::Pending] {
            for change in [Change::Revision, Change::Round, Change::RoundToken] {
                interleave(entry, Barrier::Health, change).await;
            }
        }
    });
}

#[test]
fn postgres_recovery_and_replay_resume_exact_bundle_after_unchanged_lock_waits() {
    run_test(async {
        for entry in [Entry::Recover, Entry::Replay, Entry::Pending] {
            for barrier in [Barrier::Registry, Barrier::Health] {
                interleave(entry, barrier, Change::None).await;
            }
        }
    });
}
