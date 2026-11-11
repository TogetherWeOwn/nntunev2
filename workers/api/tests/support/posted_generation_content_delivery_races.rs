//! Independent-session barriers between complete-read preparation and renewal.
//! No production hook, elapsed-time ordering or network/topology proof.

use super::*;

#[derive(Clone, Copy)]
enum Change {
    AttemptExpiry,
    OperationExpiry,
    Round,
    RoundToken,
    Revision,
    None,
}

async fn backend(db: &Client) -> i32 {
    db.query_one("SELECT pg_backend_pid()", &[])
        .await
        .quiet("read barrier backend")
        .get(0)
}

async fn blocked_by(db: &Client, waiter: i32, holder: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = db
                .query_one("SELECT $2 = ANY(pg_blocking_pids($1))", &[&waiter, &holder])
                .await
                .quiet("observe ordered row-lock barrier")
                .get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("row-lock barrier timed out");
}

// The same snapshot works inside the intervening transaction and after the
// driver exits. Include deadlines, tokens and all evidence, not just custody.
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
    .quiet("snapshot inter-transaction fences")
    .get(0)
}

async fn interleave(change: Change, side: ArticleSide) {
    let mut db = connect().await;
    let (c, _, handle) = verifying(&mut db).await;
    short_leases(&db, &c, 60, 90).await;
    let before = frozen(&db, &c).await;
    let mut first = connect().await;
    let first_pid = backend(&first).await;
    let first = first.transaction().await.quiet("begin initial barrier");
    first
        .query_one(
            "SELECT id FROM ingest_jobs WHERE id = $1 FOR UPDATE",
            &[&c.job],
        )
        .await
        .quiet("hold initial job row");
    let mut worker = connect().await;
    let worker_pid = backend(&worker).await;
    let mut middle = connect().await;
    let middle_pid = backend(&middle).await;
    let middle = middle.transaction().await.quiet("begin intervening writer");
    let mut reader = Reader::new(&c.sealed, Answer::Intact);

    let (outcome, expected) = tokio::join!(
        drive_content_read(&mut worker, &handle, side, &mut reader),
        async {
            // Preparation already holds a relation lock when its first row
            // wait is observed. Queue a conflicting RELATION lock behind it:
            // a tuple-lock queue alone can be bypassed after its old locker
            // commits. This table-wide barrier is test-only and the disposable
            // acceptance bank runs serially. No production hook is installed.
            blocked_by(&db, worker_pid, first_pid).await;
            let (locked, ()) = tokio::join!(
                middle.batch_execute("LOCK TABLE ingest_jobs IN ACCESS EXCLUSIVE MODE"),
                async {
                    blocked_by(&db, middle_pid, worker_pid).await;
                    first.commit().await.quiet("release preparation barrier");
                }
            );
            locked.quiet("acquire intervening relation barrier");
            // The same driver invocation now waits for this relation lock:
            // preparation has committed successfully, renewal cannot overtake.
            blocked_by(&db, worker_pid, middle_pid).await;
            assert!(
                before == frozen(&middle, &c).await,
                "preparation wrote state"
            );
            match change {
                Change::AttemptExpiry => {
                    middle
                        .execute(
                            "UPDATE posted_file_job_attempts SET lease_until = 0 WHERE job_id = $1",
                            &[&c.job],
                        )
                        .await
                        .quiet("expire attempt between preparation and renewal");
                }
                Change::OperationExpiry => {
                    middle
                        .execute(
                            "UPDATE posted_file_operations SET lease_until = 0 WHERE id = $1",
                            &[&c.sealed.op],
                        )
                        .await
                        .quiet("expire operation between preparation and renewal");
                }
                Change::Round | Change::RoundToken | Change::Revision => {
                    // Mutate each dimension in isolation so the other two
                    // cannot mask a missing comparison. These fixture-only
                    // states do not prove the round/scheduler lifecycle.
                    let sql = match change {
                            Change::Round => "UPDATE posted_file_health SET round = round + 1 WHERE generation_id = $1",
                            Change::RoundToken => "UPDATE posted_file_health SET round_token = gen_random_uuid()::text WHERE generation_id = $1",
                            Change::Revision => "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
                            _ => unreachable!(),
                        };
                    middle
                        .execute(sql, &[&c.sealed.generation])
                        .await
                        .quiet("change health fence between preparation and renewal");
                }
                Change::None => {}
            }
            let expected = frozen(&middle, &c).await;
            middle.commit().await.quiet("release renewal barrier");
            expected
        }
    );
    if matches!(change, Change::None) {
        let next = recorded(outcome);
        assert_eq!(reader.sides, [side]);
        assert_eq!(content_rows(&db, &c.sealed).await.len(), 1);
        assert_eq!(
            content_state(&mut db, &next, side == ArticleSide::Post).await,
            Some(ContentCheckState::Intact)
        );
        assert!(next != handle, "successful read must advance health");
    } else {
        assert!(matches!(outcome, Ok(ContentReadStep::StaleOrDiverged)));
        assert!(reader.sides.is_empty(), "stale renewal started a reader");
        assert!(expected == frozen(&db, &c).await, "refusal changed state");
        assert!(content_rows(&db, &c.sealed).await.is_empty());
    }
    assert_eq!(custody_rows(&db, &c.sealed).await, 1);
    assert_eq!(operation_rows(&db, &c.sealed).await, 1);
}

#[test]
fn postgres_content_driver_expiry_between_preparation_and_renewal_refuses_without_read_or_writes() {
    run_test(async {
        for change in [Change::AttemptExpiry, Change::OperationExpiry] {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                interleave(change, side).await;
            }
        }
    });
}

#[test]
fn postgres_content_driver_health_fence_between_preparation_and_renewal_refuses_without_read_or_writes(
) {
    run_test(async {
        for change in [Change::Round, Change::RoundToken, Change::Revision] {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                interleave(change, side).await;
            }
        }
    });
}

#[test]
fn postgres_content_driver_unchanged_fence_between_preparation_and_renewal_still_reads() {
    run_test(async {
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            interleave(Change::None, side).await;
        }
    });
}
