//! Real-PostgreSQL tests for the narrow generation completion path:
//! fenced reuse of a usable winner, one replacement candidate pinned to a
//! terminally failed predecessor, retention confirmation and atomic
//! promotion, plus the 0054 multi-generation schema replay.
//!
//! Included as a submodule of `posted_generations_postgres.rs`, so it reuses
//! that file's harness (`Case`, `Sealed`, `connect`, `acquire`, `admitted`,
//! `observe`, `count`, `try_admit`, `operation_rows`, `run_test`,
//! `run_blocking`, `Quiet`). Fixtures are synthetic sealed byte buffers keyed
//! by database UUIDs; every custody row is retained by design. Failures print
//! only static labels: no connection detail, query text, row data or sealed
//! bytes. Track-pointer wire-up, rollout/drain and R2 cleanup are out of
//! scope and never exercised here.

use super::posted_generation_legacy_fence::insert_legacy;
use super::*;
use nntune_ingest::posted_generations::{
    begin_verification_round, confirm_article, confirm_retention, finish_verification,
    prepare_content_check, promote_candidate, record_content_check, ArticleSide, ContentCheckReport,
    ContentCheckResult, GenerationHandle, MutationOutcome, PromotionOutcome,
};
use std::sync::Arc;
use tokio::sync::Barrier;

const SLOTS: [i32; 3] = [0, 1, 2];

fn applied(outcome: Result<MutationOutcome, StoreError>, ctx: &'static str) -> GenerationHandle {
    match outcome {
        Ok(MutationOutcome::Applied(handle)) => *handle,
        _ => panic!("{ctx}: expected an applied mutation"),
    }
}

/// Seed one legacy predecessor, its registry/health rows and legacy pin, in
/// `terminal_failed`. The pinned health revision is the admit observation.
async fn failed_predecessor(db: &Client, c: &Case) -> (Sealed, i64) {
    let mut old = c.sealed.clone();
    assert_eq!(
        insert_legacy(db, &old)
            .await
            .quiet("insert legacy predecessor"),
        1
    );
    old.generation = db
        .query_one(select_posted_file_id_by_sha_sql(), &[&old.hash])
        .await
        .quiet("read legacy predecessor identity")
        .get(0);
    db.execute(
        "INSERT INTO posted_file_registry (plaintext_sha256, selected_generation) VALUES ($1, $2)",
        &[&old.hash, &old.generation],
    )
    .await
    .quiet("seed predecessor registry");
    db.execute(
        "INSERT INTO posted_file_health (generation_id) VALUES ($1)",
        &[&old.generation],
    )
    .await
    .quiet("seed predecessor health");
    db.execute(
        "INSERT INTO posted_file_references (kind, owner_id, generation_id, plaintext_sha256) \
         VALUES ('legacy_pin', $1, $1, $2)",
        &[&old.generation, &old.hash],
    )
    .await
    .quiet("seed predecessor legacy pin");
    db.execute(
        "UPDATE posted_file_health SET state = 'terminal_failed', failure_kind = 'retention' \
         WHERE generation_id = $1",
        &[&old.generation],
    )
    .await
    .quiet("seed predecessor terminal failure");
    let revision: i64 = db
        .query_one(
            "SELECT revision FROM posted_file_health WHERE generation_id = $1",
            &[&old.generation],
        )
        .await
        .quiet("read predecessor health revision")
        .get(0);
    (old, revision)
}

fn lane(side: ArticleSide) -> &'static str {
    match side {
        ArticleSide::Post => "pool-a",
        ArticleSide::Verify => "pool-b",
    }
}

fn intact(s: &Sealed, side: ArticleSide) -> ContentCheckResult<'_> {
    ContentCheckResult::Intact(ContentCheckReport {
        backbone: lane(side),
        plan_sha256: &s.plan,
        plaintext_sha256: &s.hash,
        plaintext_size: 1024,
        checked_articles: 3,
    })
}

/// Admit the replacement and run it to a usable, promotable candidate.
/// Presence alone never retains: both backbones observe `Intact` content
/// before `confirm_retention` may mark the candidate `usable`.
async fn usable_candidate(db: &mut Client, attempt: &AttemptHandle, c: &Case) -> GenerationHandle {
    let admitted_handle = match try_admit(db, attempt, &c.sealed).await {
        Ok(AdmissionOutcome::Admitted(handle, _)) => handle,
        _ => panic!("replacement was not admitted"),
    };
    let mut handle = applied(
        begin_verification_round(db, &admitted_handle).await,
        "begin verification round",
    );
    for slot in SLOTS {
        for side in [ArticleSide::Post, ArticleSide::Verify] {
            handle = applied(
                confirm_article(db, &handle, slot, side).await,
                "confirm fixture article",
            );
        }
    }
    handle = applied(
        finish_verification(db, &handle).await,
        "finish verification",
    );
    for side in [ArticleSide::Post, ArticleSide::Verify] {
        let permit = match prepare_content_check(db, &handle, side).await {
            Ok(Some(permit)) => permit,
            _ => panic!("expected a content-check permit"),
        };
        handle = applied(
            record_content_check(db, &permit, intact(&c.sealed, side)).await,
            "record intact content",
        );
    }
    applied(confirm_retention(db, &handle).await, "confirm retention")
}

async fn registry_state(db: &Client, hash: &str) -> (Option<String>, Option<String>, i64) {
    let row = db
        .query_one(
            "SELECT selected_generation, pending_operation, revision FROM posted_file_registry \
             WHERE plaintext_sha256 = $1",
            &[&hash],
        )
        .await
        .quiet("read registry state");
    (row.get(0), row.get(1), row.get(2))
}

async fn health_state(db: &Client, generation: &str) -> (String, Option<i64>, Option<i64>) {
    let row = db
        .query_one(
            "SELECT state, verified_at, retention_checked_at FROM posted_file_health \
             WHERE generation_id = $1",
            &[&generation],
        )
        .await
        .quiet("read health state");
    (row.get(0), row.get(1), row.get(2))
}

async fn reference_count(db: &Client, generation: &str, kind: &str) -> i64 {
    count(
        db,
        "SELECT count(*) FROM posted_file_references WHERE generation_id = $1 AND kind = $2",
        &[&generation, &kind],
    )
    .await
}

#[test]
fn postgres_completion_failed_predecessor_runs_to_atomic_publication() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (old, _) = failed_predecessor(&db, &c).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = usable_candidate(&mut db, &attempt, &c).await;
        let hash = c.sealed.hash.clone();

        // Promotion is the only selection mover: still the predecessor.
        let (selected, pending, _) = registry_state(&db, &hash).await;
        assert_eq!(selected.as_deref(), Some(old.generation.as_str()));
        assert_eq!(pending.as_deref(), Some(c.sealed.op.as_str()));
        let (state, verified, retained) = health_state(&db, &c.sealed.generation).await;
        assert_eq!(state, "usable");
        assert!(verified.is_some() && retained.is_some());
        let custody_before: String = db
            .query_one(
                "SELECT md5(t::text) FROM posted_files t WHERE t.id = $1",
                &[&c.sealed.generation],
            )
            .await
            .quiet("read candidate custody digest")
            .get(0);
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "in_flight").await,
            1
        );

        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::Promoted)
            ),
            "usable candidate was not promoted"
        );
        let (selected, pending, _) = registry_state(&db, &hash).await;
        assert_eq!(selected.as_deref(), Some(c.sealed.generation.as_str()));
        assert!(pending.is_none());
        let op_state: String = db
            .query_one(
                "SELECT state FROM posted_file_operations WHERE id = $1",
                &[&c.sealed.op],
            )
            .await
            .quiet("read operation state")
            .get(0);
        assert_eq!(op_state, "promoted");
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "serving").await,
            1
        );
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "in_flight").await,
            0
        );
        // The legacy predecessor and its pin survive publication untouched.
        assert_eq!(reference_count(&db, &old.generation, "legacy_pin").await, 1);
        let (pred_state, _, _) = health_state(&db, &old.generation).await;
        assert_eq!(pred_state, "terminal_failed");
        let custody_after: String = db
            .query_one(
                "SELECT md5(t::text) FROM posted_files t WHERE t.id = $1",
                &[&c.sealed.generation],
            )
            .await
            .quiet("read candidate custody digest after promotion")
            .get(0);
        assert_eq!(custody_before, custody_after, "promotion rewrote custody");

        // Receipt replay with the original handle; fresh admit reuses.
        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::AlreadyPromoted)
            ),
            "promotion replay was not an idempotent receipt"
        );
        match try_admit(&mut db, &attempt, &c.sealed).await {
            Ok(AdmissionOutcome::ReusedUsableWinner(bundle)) => {
                assert_eq!(bundle.generation_id(), c.sealed.generation);
            }
            _ => panic!("promoted winner was not reused"),
        }
        // Two rows share the hash; the legacy lookup stays deterministic.
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            2
        );
        let looked_up: String = db
            .query_one(select_posted_file_id_by_sha_sql(), &[&hash])
            .await
            .quiet("read legacy-first identity")
            .get(0);
        assert_eq!(looked_up, old.generation);
        // A legacy insert under a managed selection writes nothing.
        let mut other = c.sealed.clone();
        other.generation = format!("pg-gen-{}-legacy", c.uuid);
        assert_eq!(
            insert_legacy(&db, &other)
                .await
                .quiet("legacy insert after promotion"),
            0,
            "legacy writer minted a second winner after managed selection"
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            2
        );
    });
}

#[test]
fn postgres_completion_managed_serving_predecessor_promotes_replacement_exactly_once() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (_old, _) = failed_predecessor(&db, &c).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let first = usable_candidate(&mut db, &attempt, &c).await;
        let hash = c.sealed.hash.clone();
        assert!(
            matches!(
                promote_candidate(&mut db, &first).await,
                Ok(PromotionOutcome::Promoted)
            ),
            "first managed generation was not promoted"
        );
        // The promoted managed predecessor holds a serving reference.
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "serving").await,
            1
        );
        let serving_token: String = db
            .query_one(
                "SELECT reference_token FROM posted_file_references \
                 WHERE generation_id = $1 AND kind = 'serving'",
                &[&c.sealed.generation],
            )
            .await
            .quiet("read predecessor serving token")
            .get(0);
        db.execute(
            "UPDATE posted_file_health SET state = 'terminal_failed', \
             failure_kind = 'retention' WHERE generation_id = $1",
            &[&c.sealed.generation],
        )
        .await
        .quiet("fail promoted managed predecessor");

        // A replacement candidate for the same hash, pinned to the
        // terminally failed managed predecessor.
        let uuid = db_uuid(&db).await;
        let plan = digest(&db, "plan", &uuid).await;
        let (job, owner) = seed_job(&db, &uuid).await;
        let replacement = Case {
            sealed: Sealed::new(&uuid, &hash, &plan, 0x20),
            uuid,
            job,
            owner,
        };
        let attempt = acquire(&mut db, &replacement.job, &replacement.owner, 0).await;
        let handle = usable_candidate(&mut db, &attempt, &replacement).await;

        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::Promoted)
            ),
            "replacement of a managed serving predecessor was not promoted"
        );
        let (selected, pending, _) = registry_state(&db, &hash).await;
        assert_eq!(
            selected.as_deref(),
            Some(replacement.sealed.generation.as_str())
        );
        assert!(pending.is_none());
        // The predecessor serving incarnation is retired and a
        // fresh-token rollback incarnation carries the same custody.
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "serving").await,
            0
        );
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "rollback").await,
            1
        );
        let rollback_token: String = db
            .query_one(
                "SELECT reference_token FROM posted_file_references \
                 WHERE generation_id = $1 AND kind = 'rollback'",
                &[&c.sealed.generation],
            )
            .await
            .quiet("read predecessor rollback token")
            .get(0);
        assert_ne!(
            rollback_token, serving_token,
            "rollback must be a fresh incarnation, not the serving token"
        );
        assert_eq!(
            reference_count(&db, &replacement.sealed.generation, "serving").await,
            1
        );
        assert_eq!(
            reference_count(&db, &replacement.sealed.generation, "in_flight").await,
            0
        );
        let (pred_state, _, _) = health_state(&db, &c.sealed.generation).await;
        assert_eq!(pred_state, "terminal_failed");

        // Replay is a receipt and publishes nothing twice.
        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::AlreadyPromoted)
            ),
            "managed-predecessor promotion replay was not a receipt"
        );
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "rollback").await,
            1
        );
        assert_eq!(
            reference_count(&db, &replacement.sealed.generation, "serving").await,
            1
        );
    });
}

#[test]
fn postgres_completion_promotion_refuses_a_drifted_predecessor_basis() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (old, _) = failed_predecessor(&db, &c).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = usable_candidate(&mut db, &attempt, &c).await;
        let hash = c.sealed.hash.clone();

        db.execute(
            "UPDATE posted_file_health SET revision = revision + 1 WHERE generation_id = $1",
            &[&old.generation],
        )
        .await
        .quiet("drift predecessor health revision");
        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::StaleOrDiverged)
            ),
            "drifted predecessor basis was promoted"
        );
        let (selected, pending, _) = registry_state(&db, &hash).await;
        assert_eq!(selected.as_deref(), Some(old.generation.as_str()));
        assert_eq!(pending.as_deref(), Some(c.sealed.op.as_str()));
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "serving").await,
            0
        );
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "in_flight").await,
            1
        );
    });
}

#[test]
fn postgres_completion_promotion_race_commits_exactly_once() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (_old, _) = failed_predecessor(&db, &c).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = usable_candidate(&mut db, &attempt, &c).await;
        let hash = c.sealed.hash.clone();

        let mut left = connect().await;
        let mut right = connect().await;
        let handle_left = handle.clone();
        let handle_right = handle.clone();
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                promote_candidate(&mut left, &handle_left).await
            },
            async {
                gate_right.wait().await;
                promote_candidate(&mut right, &handle_right).await
            }
        );
        let promoted = [&out_left, &out_right]
            .iter()
            .filter(|o| matches!(o, Ok(PromotionOutcome::Promoted)))
            .count();
        let receipt = [&out_left, &out_right]
            .iter()
            .filter(|o| matches!(o, Ok(PromotionOutcome::AlreadyPromoted)))
            .count();
        assert_eq!(
            promoted + receipt,
            2,
            "race outcome must publish or receipt"
        );
        assert_eq!(promoted, 1, "race must promote exactly once");
        let (selected, pending, _) = registry_state(&db, &hash).await;
        assert_eq!(selected.as_deref(), Some(c.sealed.generation.as_str()));
        assert!(pending.is_none());
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "serving").await,
            1
        );
        assert_eq!(
            reference_count(&db, &c.sealed.generation, "in_flight").await,
            0
        );
    });
}

#[test]
fn postgres_completion_concurrent_replacement_admissions_commit_once() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (old, _) = failed_predecessor(&db, &c).await;
        let hash = c.sealed.hash.clone();
        // A second distinct candidate for the same hash and predecessor.
        let other_uuid = db_uuid(&db).await;
        let other_plan = digest(&db, "plan", &other_uuid).await;
        let (other_job, other_owner) = seed_job(&db, &other_uuid).await;
        let other = Sealed::new(&other_uuid, &hash, &other_plan, 0x20);
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let other_attempt = acquire(&mut db, &other_job, &other_owner, 0).await;

        let mut left = connect().await;
        let mut right = connect().await;
        let expected_left = observe(&left, &hash).await.expect("observe left hash");
        let expected_right = observe(&right, &hash).await.expect("observe right hash");
        let sealed_left = c.sealed.clone();
        let sealed_right = other.clone();
        let barrier = Arc::new(Barrier::new(2));
        let (gate_left, gate_right) = (barrier.clone(), barrier);
        let (out_left, out_right) = tokio::join!(
            async {
                gate_left.wait().await;
                admit(&mut left, &attempt, &expected_left, &sealed_left.mint()).await
            },
            async {
                gate_right.wait().await;
                admit(
                    &mut right,
                    &other_attempt,
                    &expected_right,
                    &sealed_right.mint(),
                )
                .await
            }
        );
        let admitted_count = [&out_left, &out_right]
            .iter()
            .filter(|o| matches!(o, Ok(AdmissionOutcome::Admitted(..))))
            .count();
        assert_eq!(admitted_count, 1, "racing replacements must admit once");
        let _ = old;
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_operations WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            2
        );
    });
}

#[test]
fn postgres_completion_migration_0054_replay_preserves_multi_generation_custody() {
    run_blocking(true, async {
        let mut db = connect().await;
        let c = case(&db).await;
        let (_old, _) = failed_predecessor(&db, &c).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let _handle = usable_candidate(&mut db, &attempt, &c).await;
        let hash = c.sealed.hash.clone();
        let snapshot_sql =
            "SELECT (SELECT md5(string_agg(t::text, ',' ORDER BY t.id)) FROM posted_files t \
                 WHERE t.plaintext_sha256 = $1), \
                 (SELECT md5(t::text) FROM posted_file_registry t WHERE t.plaintext_sha256 = $1), \
                 (SELECT md5(t::text) FROM posted_file_operations t WHERE t.id = $3), \
                 (SELECT md5(t::text) FROM posted_file_health t WHERE t.generation_id = $2), \
                 (SELECT md5(string_agg(t::text, ',' ORDER BY t.kind, t.owner_id)) \
                   FROM posted_file_references t WHERE t.generation_id = $2)";
        let before_row = db
            .query_one(snapshot_sql, &[&hash, &c.sealed.generation, &c.sealed.op])
            .await
            .quiet("snapshot multi-generation custody");
        let before: Vec<Option<String>> = (0..5).map(|i| before_row.get(i)).collect();

        let ddl = connect().await;
        ddl.batch_execute(MIGRATION_0054)
            .await
            .quiet("replay migration 0054");
        ddl.batch_execute(MIGRATION_0054)
            .await
            .quiet("replay migration 0054 again");

        let after_row = db
            .query_one(snapshot_sql, &[&hash, &c.sealed.generation, &c.sealed.op])
            .await
            .quiet("snapshot custody after replay");
        let after: Vec<Option<String>> = (0..5).map(|i| after_row.get(i)).collect();
        assert!(
            after.iter().all(Option::is_some),
            "every custody component must resolve: {after:?}"
        );
        assert_eq!(before, after, "migration replay changed custody");
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
                &[&hash]
            )
            .await,
            2
        );
    });
}

#[test]
fn postgres_completion_presence_only_candidate_is_not_retained() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let admitted_handle = match try_admit(&mut db, &attempt, &c.sealed).await {
            Ok(AdmissionOutcome::Admitted(handle, _)) => handle,
            _ => panic!("candidate was not admitted"),
        };
        let mut handle = applied(
            begin_verification_round(&mut db, &admitted_handle).await,
            "begin verification round",
        );
        for slot in SLOTS {
            for side in [ArticleSide::Post, ArticleSide::Verify] {
                handle = applied(
                    confirm_article(&mut db, &handle, slot, side).await,
                    "confirm fixture article",
                );
            }
        }
        handle = applied(
            finish_verification(&mut db, &handle).await,
            "finish verification",
        );
        // Presence completion stamps `verified_at` but is not content
        // proof: retention without any `Intact` observation is incomplete,
        // the candidate stays `verifying`, and it cannot promote.
        assert!(
            matches!(
                confirm_retention(&mut db, &handle).await,
                Ok(MutationOutcome::Incomplete)
            ),
            "presence-only candidate was retained"
        );
        let (state, _, retained) = health_state(&db, &c.sealed.generation).await;
        assert_eq!(state, "verifying");
        assert!(retained.is_none());
        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::NotReady)
            ),
            "presence-only candidate promoted"
        );
        // One intact side is still not enough; both backbones must observe.
        let permit = match prepare_content_check(&mut db, &handle, ArticleSide::Post).await {
            Ok(Some(permit)) => permit,
            _ => panic!("expected a content-check permit"),
        };
        handle = applied(
            record_content_check(&mut db, &permit, intact(&c.sealed, ArticleSide::Post)).await,
            "record intact post side",
        );
        assert!(
            matches!(
                confirm_retention(&mut db, &handle).await,
                Ok(MutationOutcome::Incomplete)
            ),
            "single-sided candidate was retained"
        );
        let permit = match prepare_content_check(&mut db, &handle, ArticleSide::Verify).await {
            Ok(Some(permit)) => permit,
            _ => panic!("expected a content-check permit"),
        };
        handle = applied(
            record_content_check(&mut db, &permit, intact(&c.sealed, ArticleSide::Verify)).await,
            "record intact verify side",
        );
        handle = applied(
            confirm_retention(&mut db, &handle).await,
            "confirm retention after both intact sides",
        );
        let (state, _, retained) = health_state(&db, &c.sealed.generation).await;
        assert_eq!(state, "usable");
        assert!(retained.is_some());
        assert!(
            matches!(
                promote_candidate(&mut db, &handle).await,
                Ok(PromotionOutcome::Promoted)
            ),
            "content-verified candidate was not promoted"
        );
    });
}
