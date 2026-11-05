//! Database-discovered delivery entry: no operation/round/cursor from Queue/KV.
//! Scripted transport proves bookkeeping only, not deployed queue/pool routing.

use super::*;
use nntune_ingest::posted_generations::{
    abandon_candidate, drive_pending_recheck, recover_pending, take_over,
};

async fn delivery_snapshot(db: &Client, c: &Case) -> Vec<Option<String>> {
    let mut result = frozen(db, &c.sealed).await;
    let row = db
        .query_one(
            "SELECT (SELECT md5(t::text) FROM ingest_jobs t WHERE id = $1), \
             (SELECT md5(t::text) FROM posted_file_job_attempts t WHERE job_id = $1)",
            &[&c.job],
        )
        .await
        .quiet("snapshot delivery attempt and job");
    result.push(row.get(0));
    result.push(row.get(1));
    result
}

#[test]
fn postgres_pending_delivery_discovers_exact_custody_and_resumes_without_local_progress() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, _) = begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        let mut t = Transport::new(&c.sealed);
        // Deliberately discard every returned handle: each delivery must recover
        // the latest revision and cursor from the one durable operation.
        for expected_cursor in 1..ARTICLES * 2 {
            assert!(matches!(
                drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::Progress(_))
            ));
            assert_eq!(cursor(&db, &c.sealed).await, expected_cursor);
        }
        let handle = complete_handle(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            "discovered final slice",
        );
        assert!(handle.generation_id() == c.sealed.generation);
        assert!(handle.operation_id() == c.sealed.op);
        assert!(
            t.calls
                == [
                    st(0, true),
                    st(0, false),
                    st(1, true),
                    st(1, false),
                    st(2, true),
                    st(2, false)
                ]
        );
        assert!(t.exact);
        assert_eq!(repost_sum(&db, &c.sealed).await, 0);
        let before = delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::PresenceComplete(_))
        ));
        assert_eq!(t.calls.len(), (ARTICLES * 2) as usize);
        assert!(before == delivery_snapshot(&db, &c).await);
        assert_eq!(health_outcome(&db, &c.sealed).await.0, "verifying");
        assert_eq!(content_rows(&db, &c.sealed).await, 0);
    });
}

#[test]
fn postgres_pending_delivery_invalid_missing_foreign_and_stopped_work_never_reaches_transport() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, handle) = begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        let other = case(&db).await;
        let foreign = acquire(&mut db, &other.job, &other.owner, 0).await;
        let mut t = Transport::new(&c.sealed);
        let before = delivery_snapshot(&db, &c).await;
        for bound in [0, STAT_BATCH_SIZE + 1, usize::MAX] {
            assert!(matches!(
                drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, bound).await,
                Err(StoreError::InvalidInput)
            ));
        }
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, "invalid", &mut t, 1).await,
            Err(StoreError::InvalidInput)
        ));
        assert!(matches!(
            recover_pending(&mut db, &attempt, &other.sealed.hash).await,
            Ok(AdmissionOutcome::InconsistentOrMissing)
        ));
        for (a, hash) in [(&attempt, &other.sealed.hash), (&foreign, &c.sealed.hash)] {
            assert!(matches!(
                drive_pending_recheck(&mut db, a, hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
        }
        assert!(before == delivery_snapshot(&db, &c).await);
        assert!(matches!(
            abandon_candidate(&mut db, &handle).await,
            Ok(nntune_ingest::posted_generations::AbandonmentOutcome::Applied)
        ));
        let stopped = delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(stopped == delivery_snapshot(&db, &c).await);
        assert!(t.calls.is_empty());
    });
}

#[test]
fn postgres_pending_delivery_unbegun_waiting_and_expired_work_does_not_renew_or_start_rounds() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = admitted(&mut db, &attempt, &c.sealed).await;
        let mut t = Transport::new(&c.sealed);
        let before = delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(before == delivery_snapshot(&db, &c).await);
        applied(
            begin_verification_round(&mut db, &handle).await,
            "begin delivery round",
        );
        let waiting = delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::Waiting { .. })
        ));
        assert!(waiting == delivery_snapshot(&db, &c).await);
        make_due(&db, &c.sealed).await;
        for sql in [
            "UPDATE posted_file_operations SET lease_until = 0 WHERE job_id = $1",
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
        ] {
            db.execute(sql, &[&c.job])
                .await
                .quiet("expire delivery fixture lease");
            let expired = delivery_snapshot(&db, &c).await;
            assert!(matches!(
                drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
            assert!(expired == delivery_snapshot(&db, &c).await);
            // Restore the operation only so the second iteration isolates the
            // attempt deadline, without accidentally testing both at once.
            db.execute(
                "UPDATE posted_file_operations SET lease_until = extract(epoch FROM clock_timestamp())::bigint + 300 WHERE job_id = $1",
                &[&c.job],
            ).await.quiet("restore operation fixture lease");
        }
        assert!(t.calls.is_empty());
    });
}

#[test]
fn postgres_pending_delivery_requires_explicit_takeover_and_keeps_the_durable_cursor() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, _) = begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        let mut t = Transport::new(&c.sealed);
        progress_handle(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            "first discovered slice",
        );
        for sql in [
            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
            "UPDATE posted_file_operations SET lease_until = 0 WHERE job_id = $1",
        ] {
            db.execute(sql, &[&c.job])
                .await
                .quiet("expire takeover fixture");
        }
        let next = acquire(&mut db, &c.job, &c.owner, 1).await;
        let before = delivery_snapshot(&db, &c).await;
        for a in [&attempt, &next] {
            assert!(matches!(
                drive_pending_recheck(&mut db, a, &c.sealed.hash, &mut t, 1).await,
                Ok(RecheckStep::StaleOrDiverged)
            ));
        }
        assert!(before == delivery_snapshot(&db, &c).await);
        assert_eq!(t.calls.len(), 1);
        let expected = observe(&db, &c.sealed.hash)
            .await
            .expect("observe delivery takeover");
        assert!(matches!(
            take_over(&mut db, &next, &expected, &c.sealed.op).await,
            Ok(AdmissionOutcome::ResumedSameCandidate(_, _))
        ));
        progress_handle(
            drive_pending_recheck(&mut db, &next, &c.sealed.hash, &mut t, 1).await,
            "taken over discovered slice",
        );
        assert!(t.calls == [st(0, true), st(0, false)]);
        assert_eq!(cursor(&db, &c.sealed).await, 2);
        assert!(t.exact);
        let after = delivery_snapshot(&db, &c).await;
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(after == delivery_snapshot(&db, &c).await);
        assert_eq!(t.calls.len(), 2);
    });
}

#[test]
fn postgres_pending_delivery_keeps_original_callback_fences_without_network_locks() {
    run_test(async {
        let mut db = connect().await;
        let (c, attempt, _) = begun(&mut db).await;
        make_due(&db, &c.sealed).await;
        let mut t = Transport::new(&c.sealed);
        t.hook = Some(Hook {
            db: connect().await,
            attempt: attempt.clone(),
            job: c.job.clone(),
            owner: c.owner.clone(),
            sealed: c.sealed.clone(),
            fault: Fault::Revision,
            at_repost: false,
            fired: false,
        });
        assert!(matches!(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            Ok(RecheckStep::StaleOrDiverged)
        ));
        assert!(t.hook.as_ref().expect("delivery hook").fired);
        assert!(t.calls == [st(0, true)]);
        assert_eq!(cursor(&db, &c.sealed).await, 0);
        assert!(rows(&db, &c.sealed).await == [(2, 1, true, 0)]);
        // Recovery may use the new durable revision on a NEW request, never
        // attach the stale answer from the previous request to that revision.
        progress_handle(
            drive_pending_recheck(&mut db, &attempt, &c.sealed.hash, &mut t, 1).await,
            "fresh discovered callback",
        );
        assert!(t.calls == [st(0, true), st(0, true)]);
        assert_eq!(cursor(&db, &c.sealed).await, 1);
        assert!(t.exact);
    });
}
