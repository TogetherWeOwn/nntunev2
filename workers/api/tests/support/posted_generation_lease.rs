//! Live-candidate delivery heartbeats on disposable PostgreSQL. No lease expiry
//! permits revival; only the existing acquire/takeover path can rotate authority.
//! All fixtures are synthetic and all failure labels are static/redacted.

use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, renew_candidate_lease, take_over, ArticleSide,
    GenerationHandle, MutationOutcome,
};

async fn prepared(db: &mut Client) -> (Case, AttemptHandle, GenerationHandle) {
    let c = case(db).await;
    let attempt = acquire(db, &c.job, &c.owner, 0).await;
    let handle = admitted(db, &attempt, &c.sealed).await;
    (c, attempt, handle)
}

async fn leases(db: &Client, c: &Case) -> (i64, i64) {
    let r = db
        .query_one(
            "SELECT a.lease_until, o.lease_until FROM posted_file_job_attempts a \
             JOIN posted_file_operations o ON o.job_id = a.job_id WHERE o.id = $1",
            &[&c.sealed.op],
        )
        .await
        .quiet("read delivery leases");
    (r.get(0), r.get(1))
}

async fn set_leases(db: &Client, c: &Case, attempt: i64, operation: i64) {
    db.execute(
        "UPDATE posted_file_job_attempts SET lease_until = $2 WHERE job_id = $1",
        &[&c.job, &attempt],
    )
    .await
    .quiet("set attempt fixture lease");
    db.execute(
        "UPDATE posted_file_operations SET lease_until = $2 WHERE id = $1",
        &[&c.sealed.op, &operation],
    )
    .await
    .quiet("set operation fixture lease");
}

async fn db_now(db: &Client) -> i64 {
    db.query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
        .await
        .quiet("read database clock")
        .get(0)
}

// Everything except the two deadlines: custody, registry, operation/attempt
// identities and tokens, health, references, all progress/evidence and schedule.
async fn frozen(db: &Client, c: &Case) -> Vec<Option<String>> {
    let s = &c.sealed;
    let mut v = snapshot(db, s).await;
    let row = db
        .query_one(
            "SELECT \
             (SELECT md5((to_jsonb(o) - 'lease_until')::text) \
                FROM posted_file_operations o WHERE o.id = $1), \
             (SELECT md5((to_jsonb(a) - 'lease_until')::text) \
                FROM posted_file_job_attempts a WHERE a.job_id = $2), \
             (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round, p.slot, p.side)::text) \
                FROM posted_file_article_progress p WHERE p.generation_id = $3), \
             (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round, p.side)::text) \
                FROM posted_file_content_checks p WHERE p.generation_id = $3), \
             (SELECT md5(jsonb_agg(to_jsonb(p) ORDER BY p.round)::text) \
                FROM posted_file_recheck_schedule p WHERE p.generation_id = $3)",
            &[&s.op, &c.job, &s.generation],
        )
        .await
        .quiet("snapshot heartbeat invariants");
    v[2] = row.get(0);
    v.extend((1..5).map(|i| row.get::<_, Option<String>>(i)));
    v
}

#[test]
fn postgres_live_renewal_extends_both_leases_and_preserves_original_fences_and_progress() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = prepared(&mut db).await;
        let MutationOutcome::Applied(handle) = begin_verification_round(&mut db, &handle)
            .await
            .expect("begin fixture round")
        else {
            panic!("fixture round refused")
        };
        let MutationOutcome::Applied(handle) =
            confirm_article(&mut db, &handle, 0, ArticleSide::Post)
                .await
                .expect("confirm fixture slot")
        else {
            panic!("fixture confirmation refused")
        };
        let now = db_now(&db).await;
        set_leases(&db, &c, now + 30, now + 40).await;
        let before = frozen(&db, &c).await;
        assert!(renew_candidate_lease(&mut db, &handle)
            .await
            .expect("renew"));
        let after = db_now(&db).await;
        let (a, o) = leases(&db, &c).await;
        assert_eq!(a, o);
        assert!(a >= now + 300 && a <= after + 300);
        assert!(
            before == frozen(&db, &c).await,
            "renewal changes only deadlines"
        );

        // A lost acknowledgment is recovered with the same stored custody and
        // current revision. Repeating renewal never rotates a token or a budget.
        let AdmissionOutcome::ResumedSameCandidate(recovered, bundle) =
            recover(&mut db, &attempt, &c.sealed.hash, &c.sealed.op)
                .await
                .expect("recover heartbeat")
        else {
            panic!("heartbeat recovery refused")
        };
        assert!(bundle.custody().expect("stored custody") == c.sealed.custody());
        assert!(recovered == *handle);
        assert!(renew_candidate_lease(&mut db, &recovered)
            .await
            .expect("repeat renewal"));
        assert!(before == frozen(&db, &c).await);
    });
}

#[test]
fn postgres_renewal_never_shortens_either_live_deadline() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, handle) = prepared(&mut db).await;
        let now = db_now(&db).await;
        set_leases(&db, &c, now + 600, now + 900).await;
        let before = frozen(&db, &c).await;
        assert!(renew_candidate_lease(&mut db, &handle)
            .await
            .expect("renew"));
        assert_eq!(leases(&db, &c).await, (now + 900, now + 900));
        assert!(before == frozen(&db, &c).await);
    });
}

#[test]
fn postgres_renewal_refuses_either_expired_lease_without_partial_extension() {
    run_test(async {
        let mut db = connect().await;
        for expired_attempt in [true, false] {
            let (c, _, handle) = prepared(&mut db).await;
            let now = db_now(&db).await;
            let deadlines = if expired_attempt {
                (0, now + 300)
            } else {
                (now + 300, 0)
            };
            set_leases(&db, &c, deadlines.0, deadlines.1).await;
            let before = frozen(&db, &c).await;
            assert!(!renew_candidate_lease(&mut db, &handle)
                .await
                .expect("expired renewal"));
            assert_eq!(leases(&db, &c).await, deadlines);
            assert!(before == frozen(&db, &c).await);
        }
    });
}

#[test]
fn postgres_renewal_refuses_stopped_job_owner_drift_and_old_health_revision() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, old) = prepared(&mut db).await;
        let deadlines = leases(&db, &c).await;
        db.execute(
            "UPDATE ingest_jobs SET status = 'failed' WHERE id = $1",
            &[&c.job],
        )
        .await
        .quiet("stop fixture job");
        assert!(!renew_candidate_lease(&mut db, &old)
            .await
            .expect("stopped job"));
        db.execute(
            "UPDATE ingest_jobs SET status = 'running' WHERE id = $1",
            &[&c.job],
        )
        .await
        .quiet("restore fixture job");
        let other = seed_user(&db, &format!("{}-renew", c.uuid)).await;
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&c.job, &other],
        )
        .await
        .quiet("move fixture job owner");
        assert!(!renew_candidate_lease(&mut db, &old)
            .await
            .expect("owner drift"));
        db.execute(
            "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
            &[&c.job, &c.owner],
        )
        .await
        .quiet("restore fixture owner");
        let MutationOutcome::Applied(current) = begin_verification_round(&mut db, &old)
            .await
            .expect("begin newer fixture round")
        else {
            panic!("newer round refused")
        };
        let before = frozen(&db, &c).await;
        assert!(!renew_candidate_lease(&mut db, &old)
            .await
            .expect("old round"));
        assert_eq!(leases(&db, &c).await, deadlines);
        assert!(before == frozen(&db, &c).await);
        assert!(renew_candidate_lease(&mut db, &current)
            .await
            .expect("current round"));
    });
}

#[test]
fn postgres_superseded_attempt_and_claim_cannot_renew_after_takeover() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, old) = prepared(&mut db).await;
        set_leases(&db, &c, 0, 0).await;
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let expected = observe(&db, &c.sealed.hash)
            .await
            .expect("observe takeover");
        let AdmissionOutcome::ResumedSameCandidate(current, _) =
            take_over(&mut db, &next, &expected, &c.sealed.op)
                .await
                .expect("takeover")
        else {
            panic!("takeover refused")
        };
        let before = frozen(&db, &c).await;
        let deadlines = leases(&db, &c).await;
        assert!(!renew_candidate_lease(&mut db, &old)
            .await
            .expect("old claim"));
        assert_eq!(leases(&db, &c).await, deadlines);
        assert!(before == frozen(&db, &c).await);
        assert!(renew_candidate_lease(&mut db, &current)
            .await
            .expect("current claim"));
    });
}

#[test]
fn postgres_concurrent_renewals_keep_one_claim_and_allow_original_handle_callbacks() {
    run_test(async {
        let mut db = connect().await;
        let (c, _, handle) = prepared(&mut db).await;
        let before = frozen(&db, &c).await;
        let mut second = connect().await;
        let (a, b) = tokio::join!(
            renew_candidate_lease(&mut db, &handle),
            renew_candidate_lease(&mut second, &handle)
        );
        assert!(a.expect("first renewal") && b.expect("second renewal"));
        assert!(before == frozen(&db, &c).await);
        assert!(posting_fence_is_current(&mut db, &handle)
            .await
            .expect("original fence"));
        assert_eq!(leases(&db, &c).await.0, leases(&db, &c).await.1);
    });
}

#[test]
fn postgres_renewal_rechecks_both_leases_after_registry_lock_wait() {
    run_test(async {
        let mut db = connect().await;
        for expire_attempt in [true, false] {
            let (c, _, handle) = prepared(&mut db).await;
            let now = db_now(&db).await;
            let deadlines = if expire_attempt {
                (now + 3, now + 300)
            } else {
                (now + 300, now + 3)
            };
            set_leases(&db, &c, deadlines.0, deadlines.1).await;
            let before = frozen(&db, &c).await;
            let mut blocker = connect().await;
            let tx = blocker
                .transaction()
                .await
                .quiet("hold registry transaction");
            tx.query_one(
                "SELECT revision FROM posted_file_registry WHERE plaintext_sha256 = $1 FOR UPDATE",
                &[&c.sealed.hash],
            )
            .await
            .quiet("hold registry row");
            let mut worker = connect().await;
            let pid: i32 = worker
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .quiet("read worker pid")
                .get(0);
            let (outcome, ()) = tokio::join!(renew_candidate_lease(&mut worker, &handle), async {
                loop {
                    let blocked: bool = db
                        .query_one("SELECT cardinality(pg_blocking_pids($1)) > 0", &[&pid])
                        .await
                        .quiet("observe registry waiter")
                        .get(0);
                    if blocked {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let deadline = deadlines.0.min(deadlines.1);
                db.query_one(
                    "SELECT pg_sleep(GREATEST(0.0, $1::bigint::double precision - \
                         extract(epoch FROM clock_timestamp())::double precision) + 1.0)",
                    &[&deadline],
                )
                .await
                .quiet("wait for database lease expiry");
                tx.commit().await.quiet("release registry row");
            });
            assert!(!outcome.expect("expired waiter renewal"));
            assert_eq!(leases(&db, &c).await, deadlines);
            assert!(before == frozen(&db, &c).await);
        }
    });
}
