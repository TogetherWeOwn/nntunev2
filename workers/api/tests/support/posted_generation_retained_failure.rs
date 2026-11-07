//! Real conclusion transactions over explicitly SEEDED retained repair evidence.
//! Promotion and the retained STAT/repost/scheduler collector are not exercised.
//! The positive case proves the decision boundary, not an enabled replacement.
use super::*;
use nntune_ingest::posted_generations::{conclude_retained_failure, RetainedFailureOutcome};
use nntune_ingest::two_spool::{MAX_RECHECKS, MAX_REPOST_ATTEMPTS_PER_ARTICLE};

async fn withdrawn(db: &mut Client) -> Fixture {
    let f = retained(db, true).await;
    let original = serving(db, &f, "prepare negative origin").await;
    assert!(is_applied(&invalidate_retained_check(db, &original).await));
    f
}

async fn seed_repair(db: &Client, f: &Fixture) {
    let s = &f.case.sealed;
    db.execute(
        "INSERT INTO posted_file_article_progress \
         (generation_id, round, slot, side, confirmed, repost_attempts, \
          presence_verdict, presence_repost_attempts) \
         SELECT h.generation_id, h.round, slot, side::smallint, NOT (slot = 0 AND side = 0), \
         CASE WHEN slot = 0 AND side = 0 THEN $2 ELSE 0 END, \
         CASE WHEN slot = 0 AND side = 0 THEN 'missing' ELSE 'present' END, \
         CASE WHEN slot = 0 AND side = 0 THEN $2 ELSE 0 END \
         FROM posted_file_health h JOIN posted_files p ON p.id = h.generation_id \
         CROSS JOIN LATERAL generate_series(0, p.article_count - 1) slot \
         CROSS JOIN generate_series(0, 1) side WHERE h.generation_id = $1",
        &[&s.generation, &(MAX_REPOST_ATTEMPTS_PER_ARTICLE as i32)],
    )
    .await
    .quiet("seed current settled repair slots");
    db.execute(
        "INSERT INTO posted_file_recheck_schedule \
         (generation_id, round, completed_passes, state, not_before) \
         SELECT generation_id, round, $2, 'exhausted', NULL FROM posted_file_health \
         WHERE generation_id = $1",
        &[&s.generation, &(MAX_RECHECKS as i32)],
    )
    .await
    .quiet("seed tracked exhausted repair schedule");
}

async fn decisions(db: &Client, generation: &str) -> i64 {
    db.query_one(
        "SELECT count(*) FROM posted_file_retained_failures WHERE generation_id = $1",
        &[&generation],
    )
    .await
    .quiet("count failure receipts")
    .get(0)
}

fn applied(result: &Result<RetainedFailureOutcome, StoreError>) -> bool {
    matches!(result, Ok(RetainedFailureOutcome::Applied))
}

fn insufficient(result: &Result<RetainedFailureOutcome, StoreError>) -> bool {
    matches!(result, Ok(RetainedFailureOutcome::InsufficientEvidence))
}

fn stale(result: &Result<RetainedFailureOutcome, StoreError>) -> bool {
    matches!(result, Ok(RetainedFailureOutcome::StaleOrDiverged))
}

#[test]
fn postgres_retained_failure_snapshots_repair_evidence_and_preserves_custody_on_exact_replay() {
    run_test(async {
        let mut db = connect().await;
        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let permit = serving(&mut db, &f, "prepare conclusive decision").await;
        let s = &f.case.sealed;
        let before = frozen(&db, s).await;
        let original = health(&db, &s.generation).await;
        assert!(applied(&conclude_retained_failure(&mut db, &permit).await));
        let after = health(&db, &s.generation).await;
        assert!(
            after.state == "terminal_failed" && after.failure_kind.as_deref() == Some("retention"),
            "retention policy only"
        );
        assert!(
            after.revision == original.revision + 1
                && after.round == original.round + 1
                && after.token != original.token,
            "one fenced conclusion"
        );
        let after_snapshot = frozen(&db, s).await;
        for index in [0, 1, 2, 4] {
            assert!(
                before.0[index] == after_snapshot.0[index],
                "custody selection operation and references survive"
            );
        }
        assert!(
            before.1 == after_snapshot.1,
            "evidence budgets and schedule survive"
        );
        let receipt = db.query_one(
            "SELECT f.health_revision = $2 AND f.outcome_revision = $3 AS revision_bound, \
             f.origin_health_revision = o.health_revision AS origin_bound, \
             jsonb_array_length(f.repair_evidence->'slots') = p.article_count * 2 AS all_slots, \
             f.repair_evidence->'schedule'->>'state' = 'exhausted' AS exhausted, \
             f.reference_token = r.reference_token AS reference_bound \
             FROM posted_file_retained_failures f JOIN posted_file_retained_observations o \
             ON o.generation_id = f.generation_id AND o.health_revision = f.origin_health_revision \
             JOIN posted_files p ON p.id = f.generation_id \
             JOIN posted_file_references r ON r.generation_id = f.generation_id \
             AND r.kind = f.reference_kind AND r.owner_id = f.reference_owner WHERE f.generation_id = $1",
            &[&s.generation, &original.revision, &after.revision],
        ).await.quiet("read exact decision snapshot");
        for field in [
            "revision_bound",
            "origin_bound",
            "all_slots",
            "exhausted",
            "reference_bound",
        ] {
            assert!(
                receipt.get::<_, bool>(field),
                "complete immutable decision binding"
            );
        }
        assert!(
            matches!(
                conclude_retained_failure(&mut db, &permit).await,
                Ok(RetainedFailureOutcome::AlreadyFailed)
            ),
            "original receipt replay"
        );
        assert!(decisions(&db, &s.generation).await == 1, "one decision");
        assert!(
            after_snapshot == frozen(&db, s).await,
            "receipt grants no renewed mutation"
        );
    });
}

#[test]
fn postgres_retained_failure_unknown_partial_transport_and_pre_repair_evidence_never_conclude() {
    run_test(async {
        let mut db = connect().await;
        for scenario in 0..8 {
            let f = withdrawn(&mut db).await;
            let s = &f.case.sealed;
            if scenario != 0 {
                seed_repair(&db, &f).await;
                let sql = match scenario {
                    1 => "DELETE FROM posted_file_article_progress WHERE generation_id = $1 AND slot = 1 AND side = 1",
                    2 => "UPDATE posted_file_article_progress SET presence_verdict = 'unavailable' WHERE generation_id = $1 AND slot = 0 AND side = 0",
                    3 => "UPDATE posted_file_article_progress SET presence_repost_attempts = repost_attempts - 1 WHERE generation_id = $1 AND slot = 0 AND side = 0",
                    4 => "UPDATE posted_file_article_progress SET repost_attempts = repost_attempts - 1, presence_repost_attempts = presence_repost_attempts - 1 WHERE generation_id = $1 AND slot = 0 AND side = 0",
                    5 => "UPDATE posted_file_recheck_schedule SET state = 'untracked', completed_passes = 0 WHERE generation_id = $1",
                    6 => "UPDATE posted_file_recheck_schedule SET state = 'waiting', completed_passes = 4, not_before = 0 WHERE generation_id = $1",
                    _ => "UPDATE posted_file_article_progress SET confirmed = TRUE, presence_verdict = 'present' WHERE generation_id = $1",
                };
                db.execute(sql, &[&s.generation])
                    .await
                    .quiet("seed incomplete policy evidence");
            }
            let permit = serving(&mut db, &f, "prepare insufficient conclusion").await;
            let before = frozen(&db, s).await;
            assert!(
                insufficient(&conclude_retained_failure(&mut db, &permit).await),
                "unknown timer partial or transport is not failure"
            );
            assert!(
                before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
                "insufficient evidence writes nothing"
            );
        }
    });
}

#[test]
fn postgres_retained_failure_intact_content_vetoes_absence_and_mismatched_content_refuses() {
    run_test(async {
        let mut db = connect().await;
        for malformed in [false, true] {
            let f = withdrawn(&mut db).await;
            seed_repair(&db, &f).await;
            let s = &f.case.sealed;
            let inserted = db
                .execute(
                    "INSERT INTO posted_file_content_checks \
                     (generation_id, round, side, backbone, state, checked_articles, checked_at) \
                     SELECT p.id, h.round, 0, CASE WHEN $2 THEN 'wrong-pool' ELSE p.backbone_post END, \
                     'intact', p.article_count, floor(extract(epoch FROM clock_timestamp()))::bigint \
                     FROM posted_files p JOIN posted_file_health h \
                     ON h.generation_id = p.id WHERE p.id = $1",
                    &[&s.generation, &malformed],
                )
                .await
                .quiet("seed contradicting full content");
            assert!(
                inserted == 1,
                "content veto requires one persisted observation"
            );
            let permit = serving(&mut db, &f, "prepare content veto").await;
            let before = frozen(&db, s).await;
            let result = conclude_retained_failure(&mut db, &permit).await;
            assert!(
                if malformed {
                    matches!(result, Err(StoreError::Inconsistent))
                } else {
                    insufficient(&result)
                },
                "validate identity before using content veto"
            );
            assert!(
                before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
                "content never authorizes loss"
            );
        }
    });
}

#[test]
fn postgres_retained_failure_requires_current_selected_origin_and_rejects_selection_aba() {
    run_test(async {
        let mut db = connect().await;
        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let s = &f.case.sealed;
        let permit = serving(&mut db, &f, "prepare before selection ABA").await;
        db.execute("UPDATE posted_file_registry SET selected_generation = NULL, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash]).await.quiet("fixture deselect");
        assert!(
            stale(&conclude_retained_failure(&mut db, &permit).await),
            "historical reference not selected authority"
        );
        db.execute("UPDATE posted_file_registry SET selected_generation = $2, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash, &s.generation]).await.quiet("fixture reselect");
        assert!(
            stale(&conclude_retained_failure(&mut db, &permit).await),
            "same id not same selection"
        );
        let fresh = serving(&mut db, &f, "prepare after selection ABA").await;
        let before = frozen(&db, s).await;
        assert!(
            insufficient(&conclude_retained_failure(&mut db, &fresh).await),
            "old negative origin not refreshed by new permit"
        );
        assert!(
            before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
            "selection drift cannot relabel evidence"
        );
    });
}

#[test]
fn postgres_retained_failure_reference_recreation_and_changed_health_retire_original_permits() {
    run_test(async {
        let mut db = connect().await;
        for reference_change in [false, true] {
            let f = withdrawn(&mut db).await;
            seed_repair(&db, &f).await;
            let s = &f.case.sealed;
            let permit = serving(&mut db, &f, "prepare fenced conclusion").await;
            if reference_change {
                db.execute("DELETE FROM posted_file_references WHERE generation_id = $1 AND kind = 'serving' AND owner_id = $2", &[&s.generation, &f.owner]).await.quiet("remove reference incarnation");
                add_reference(&db, s, "serving", &f.owner).await;
            } else {
                db.execute("UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1", &[&s.generation]).await.quiet("advance health revision");
            }
            let before = frozen(&db, s).await;
            assert!(
                stale(&conclude_retained_failure(&mut db, &permit).await),
                "original authority retired"
            );
            assert!(
                before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
                "stale decision writes nothing"
            );
        }
    });
}

#[test]
fn postgres_retained_failure_receipt_error_rolls_back_health_and_same_permit_can_retry() {
    run_test(async {
        let mut db = connect().await;
        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let s = &f.case.sealed;
        let permit = serving(&mut db, &f, "prepare atomic failure").await;
        let before = frozen(&db, s).await;
        db.batch_execute("CREATE FUNCTION pg_temp.refuse_retained_decision() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected decision refusal'; END $$; CREATE TRIGGER refuse_retained_decision BEFORE INSERT ON posted_file_retained_failures FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_retained_decision();").await.quiet("install receipt fault");
        assert!(
            matches!(
                conclude_retained_failure(&mut db, &permit).await,
                Err(StoreError::Database)
            ),
            "receipt error returns"
        );
        db.batch_execute("DROP TRIGGER refuse_retained_decision ON posted_file_retained_failures")
            .await
            .quiet("remove receipt fault");
        assert!(
            before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
            "no half terminal health or receipt"
        );
        assert!(
            applied(&conclude_retained_failure(&mut db, &permit).await),
            "same original permit succeeds after rollback"
        );
    });
}

#[test]
fn postgres_retained_failure_concurrent_decisions_commit_once_and_lease_expiry_after_wait_refuses()
{
    run_test(async {
        let mut db = connect().await;
        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let permit = serving(&mut db, &f, "prepare concurrent conclusion").await;
        let mut other = connect().await;
        let (a, b) = tokio::join!(
            conclude_retained_failure(&mut db, &permit),
            conclude_retained_failure(&mut other, &permit)
        );
        assert!(
            matches!(
                (a, b),
                (
                    Ok(RetainedFailureOutcome::Applied),
                    Ok(RetainedFailureOutcome::AlreadyFailed)
                ) | (
                    Ok(RetainedFailureOutcome::AlreadyFailed),
                    Ok(RetainedFailureOutcome::Applied)
                )
            ),
            "one commit and one original receipt"
        );
        assert!(
            decisions(&db, &f.case.sealed.generation).await == 1,
            "one concurrent receipt"
        );

        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let permit = serving(&mut db, &f, "prepare expiry conclusion").await;
        let s = &f.case.sealed;
        let deadline: i64 = db.query_one("UPDATE posted_file_job_attempts SET lease_until = floor(extract(epoch FROM clock_timestamp()))::bigint + 2 WHERE job_id = $1 RETURNING lease_until", &[&f.case.job]).await.quiet("set test deadline").get(0);
        let pid = backend_pid(&other).await;
        let before = frozen(&db, s).await;
        let result = expire_during_lock_wait(
            &db,
            REGISTRY_LOCK,
            &s.hash,
            pid,
            deadline,
            conclude_retained_failure(&mut other, &permit),
        )
        .await;
        assert!(stale(&result), "post-wait DB clock refuses expired attempt");
        assert!(
            before == frozen(&db, s).await && decisions(&db, &s.generation).await == 0,
            "expiry writes no decision"
        );
    });
}

#[test]
fn postgres_retained_failure_decision_is_immutable_and_forward_migration_replay_preserves_receipt()
{
    run_test(async {
        let mut db = connect().await;
        let f = withdrawn(&mut db).await;
        seed_repair(&db, &f).await;
        let permit = serving(&mut db, &f, "prepare immutable decision").await;
        let s = &f.case.sealed;
        assert!(applied(&conclude_retained_failure(&mut db, &permit).await));
        let before: String = db
            .query_one(
                "SELECT md5(f::text) FROM posted_file_retained_failures f WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("digest decision")
            .get(0);
        db.batch_execute(include_str!(
            "../../../../migrations/0053_posted_file_retained_failures.sql"
        ))
        .await
        .quiet("replay forward failure migration");
        for sql in ["UPDATE posted_file_retained_failures SET registry_revision = registry_revision + 1 WHERE generation_id = $1", "DELETE FROM posted_file_retained_failures WHERE generation_id = $1"] {
            let error = db.execute(sql, &[&s.generation]).await.expect_err("decision mutation must refuse");
            assert!(error.as_db_error().is_some_and(|e| e.code().code() == "23514" && e.message() == "retained failure decision is immutable"), "exact immutable trigger");
        }
        db.execute("UPDATE posted_file_retained_failures SET repair_evidence = repair_evidence WHERE generation_id = $1", &[&s.generation]).await.quiet("decision no-op");
        let after: String = db
            .query_one(
                "SELECT md5(f::text) FROM posted_file_retained_failures f WHERE generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("digest replayed decision")
            .get(0);
        assert!(before == after, "immutable decision survives replay");
        assert!(
            matches!(
                conclude_retained_failure(&mut db, &permit).await,
                Ok(RetainedFailureOutcome::AlreadyFailed)
            ),
            "replay preserves original receipt only"
        );
    });
}
