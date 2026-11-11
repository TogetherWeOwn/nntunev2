//! Retained negative-read provenance, not conclusive failure or promotion proof.
//! The parent seeds promoted/usable fixture state after actual admission.
use super::*;
use nntune_ingest::posted_generations::{record_retained_content_check, ContentCheckResult};

async fn observation_count(db: &Client, generation: &str) -> i64 {
    db.query_one(
        "SELECT count(*) FROM posted_file_retained_observations WHERE generation_id = $1",
        &[&generation],
    )
    .await
    .quiet("count retained provenance")
    .get(0)
}

async fn registry(db: &Client, hash: &str) -> (i64, Option<String>) {
    let row = db
        .query_one(
            "SELECT revision, selected_generation FROM posted_file_registry WHERE plaintext_sha256 = $1",
            &[&hash],
        )
        .await
        .quiet("read registry context");
    (row.get(0), row.get(1))
}

async fn content_permit(
    db: &mut Client,
    f: &Fixture,
    side: ArticleSide,
) -> nntune_ingest::posted_generations::RetainedContentPermit {
    prepare_retained_content_check(
        db,
        &f.attempt,
        &f.case.sealed.generation,
        RetainedReferenceKind::Serving,
        &f.owner,
        side,
    )
    .await
    .unwrap_or_else(|_| panic!("prepare content provenance failed"))
    .expect("content permit required")
    .0
}

#[test]
fn postgres_retained_unknown_provenance_binds_original_attempt_reference_round_and_selection() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let before = health(&db, &s.generation).await;
        let selection = registry(&db, &s.hash).await;
        let reference = reference_token(&db, s, "serving", &f.owner).await;
        let permit = serving(&mut db, &f, "prepare provenance").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &permit).await
        ));
        let row = db
            .query_one(
                "SELECT o.*, a.token = o.attempt_token AS exact_attempt \
             FROM posted_file_retained_observations o JOIN posted_file_job_attempts a \
             ON a.job_id = o.job_id AND a.attempt = o.attempt WHERE o.generation_id = $1",
                &[&s.generation],
            )
            .await
            .quiet("read original provenance");
        assert!(
            row.get::<_, bool>("exact_attempt"),
            "original attempt token"
        );
        assert!(row.get::<_, String>("job_id") == f.case.job, "original job");
        assert!(
            row.get::<_, String>("plaintext_sha256") == s.hash,
            "custody hash"
        );
        assert!(
            row.get::<_, i64>("health_revision") == before.revision,
            "original revision"
        );
        assert!(row.get::<_, i64>("round") == before.round, "original round");
        assert!(
            row.get::<_, String>("round_token") == before.token,
            "original round token"
        );
        assert!(
            row.get::<_, String>("reference_token") == reference,
            "original reference"
        );
        assert!(
            row.get::<_, &str>("reference_kind") == "serving",
            "reference kind"
        );
        assert!(
            row.get::<_, String>("reference_owner") == f.owner,
            "reference owner"
        );
        assert!(
            row.get::<_, i64>("prepared_registry_revision") == selection.0,
            "prepared selection revision"
        );
        assert!(
            row.get::<_, i64>("recorded_registry_revision") == selection.0,
            "recorded selection revision"
        );
        assert!(
            row.get::<_, Option<String>>("prepared_selection") == selection.1,
            "prepared selection"
        );
        assert!(
            row.get::<_, Option<String>>("recorded_selection") == selection.1,
            "recorded selection"
        );
        assert!(
            row.get::<_, &str>("state") == "unknown",
            "unknown is not conclusive"
        );
        assert!(
            row.get::<_, Option<i16>>("side").is_none(),
            "no fabricated side"
        );
        assert!(row.get::<_, i64>("recorded_at") > 0, "DB timestamp");
        let after = health(&db, &s.generation).await;
        assert!(
            after.state == "unknown" && after.failure_kind.is_none(),
            "no failure authority"
        );
        assert!(
            after.revision == before.revision + 1 && after.round == before.round + 1,
            "one withdrawal"
        );
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert!(
            observation_count(&db, &s.generation).await == 1,
            "replay adds nothing"
        );
    });
}

#[test]
fn postgres_retained_content_provenance_keeps_each_nonaffirmative_verdict_and_side() {
    run_test(async {
        let mut db = connect().await;
        for (state, side) in [
            ("unavailable", ArticleSide::Post),
            ("authentication_failed", ArticleSide::Verify),
            ("integrity_failed", ArticleSide::Post),
        ] {
            let f = retained(&mut db, true).await;
            let s = &f.case.sealed;
            let before = health(&db, &s.generation).await;
            let permit = content_permit(&mut db, &f, side).await;
            let result = match state {
                "unavailable" => ContentCheckResult::Unavailable,
                "authentication_failed" => ContentCheckResult::AuthenticationFailed,
                _ => ContentCheckResult::IntegrityFailed,
            };
            assert!(is_applied(
                &record_retained_content_check(&mut db, &permit, result).await
            ));
            let row = db.query_one(
                "SELECT o.state, o.side, o.round, o.health_revision, c.state = o.state AS matching_content \
                 FROM posted_file_retained_observations o JOIN posted_file_content_checks c \
                 ON c.generation_id = o.generation_id AND c.round = o.round AND c.side = o.side \
                 WHERE o.generation_id = $1", &[&s.generation],
            ).await.quiet("read content provenance");
            assert!(row.get::<_, &str>("state") == state, "exact typed verdict");
            assert!(
                row.get::<_, i16>("side") == if side == ArticleSide::Post { 0 } else { 1 },
                "exact side"
            );
            assert!(
                row.get::<_, i64>("round") == before.round,
                "pre-withdrawal round"
            );
            assert!(
                row.get::<_, i64>("health_revision") == before.revision,
                "pre-withdrawal revision"
            );
            assert!(
                row.get::<_, bool>("matching_content"),
                "content and provenance agree"
            );
            let after = health(&db, &s.generation).await;
            assert!(
                after.state == "unknown" && after.failure_kind.is_none(),
                "nonaffirmative is not conclusive"
            );
        }
    });
}

#[test]
fn postgres_retained_provenance_distinguishes_selection_aba_from_historical_reference_health() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let before = registry(&db, &s.hash).await;
        let permit = serving(&mut db, &f, "prepare before selection ABA").await;
        db.execute("UPDATE posted_file_registry SET selected_generation = NULL, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash]).await.quiet("fixture deselect");
        db.execute("UPDATE posted_file_registry SET selected_generation = $2, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash, &s.generation]).await.quiet("fixture reselect");
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &permit).await
        ));
        let row = db.query_one("SELECT prepared_registry_revision, recorded_registry_revision, prepared_selection, recorded_selection FROM posted_file_retained_observations WHERE generation_id = $1", &[&s.generation]).await.quiet("read ABA context");
        assert!(
            row.get::<_, i64>(0) == before.0 && row.get::<_, i64>(1) == before.0 + 2,
            "ABA is durable, not fresh selection authority"
        );
        assert!(
            row.get::<_, Option<String>>(2) == row.get::<_, Option<String>>(3),
            "same selected id is not same selection revision"
        );
        let historical = serving(&mut db, &f, "prepare historical reference").await;
        db.execute("UPDATE posted_file_registry SET selected_generation = NULL, revision = revision + 1 WHERE plaintext_sha256 = $1", &[&s.hash]).await.quiet("fixture move global selection");
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &historical).await
        ));
        assert!(
            observation_count(&db, &s.generation).await == 2,
            "each original round retained"
        );
        assert!(
            registry(&db, &s.hash).await.1.is_none(),
            "historical health never reselects"
        );
    });
}

#[test]
fn postgres_retained_stale_callbacks_create_no_provenance() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let generic = serving(&mut db, &f, "prepare old reference").await;
        let content = content_permit(&mut db, &f, ArticleSide::Post).await;
        db.execute("DELETE FROM posted_file_references WHERE generation_id = $1 AND kind = 'serving' AND owner_id = $2", &[&s.generation, &f.owner]).await.quiet("remove reference");
        add_reference(&db, s, "serving", &f.owner).await;
        let before = frozen(&db, s).await;
        assert!(is_stale(
            &invalidate_retained_check(&mut db, &generic).await
        ));
        assert!(is_stale(
            &record_retained_content_check(&mut db, &content, ContentCheckResult::Unavailable)
                .await
        ));
        assert!(
            observation_count(&db, &s.generation).await == 0,
            "no stale provenance"
        );
        assert!(
            before == frozen(&db, s).await,
            "no stale content or health effects"
        );
    });
}

#[test]
fn postgres_retained_provenance_and_content_roll_back_with_health_failure() {
    // Installs a persistent trigger on the shared health table: run
    // exclusive so concurrent tests cannot trip over it.
    run_blocking(true, async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let generic = serving(&mut db, &f, "prepare rollback").await;
        let content = content_permit(&mut db, &f, ArticleSide::Verify).await;
        let before = frozen(&db, s).await;
        db.batch_execute("CREATE FUNCTION pg_temp.refuse_retained_health() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected retained health refusal'; END $$; CREATE TRIGGER refuse_retained_health BEFORE UPDATE ON posted_file_health FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_retained_health();").await.quiet("install rollback fault");
        assert!(
            matches!(
                invalidate_retained_check(&mut db, &generic).await,
                Err(StoreError::Database)
            ),
            "generic transaction fails"
        );
        assert!(
            matches!(
                record_retained_content_check(
                    &mut db,
                    &content,
                    ContentCheckResult::IntegrityFailed
                )
                .await,
                Err(StoreError::Database)
            ),
            "content transaction fails"
        );
        db.batch_execute("DROP TRIGGER refuse_retained_health ON posted_file_health")
            .await
            .quiet("remove rollback fault");
        assert!(
            observation_count(&db, &s.generation).await == 0,
            "no half provenance"
        );
        assert!(before == frozen(&db, s).await, "no half content or health");
        assert!(
            is_applied(&invalidate_retained_check(&mut db, &generic).await),
            "same original permit works after rollback"
        );
    });
}

#[test]
fn postgres_retained_provenance_is_immutable_and_migration_replay_preserves_it() {
    run_test(async {
        let mut db = connect().await;
        let f = retained(&mut db, true).await;
        let s = &f.case.sealed;
        let permit = serving(&mut db, &f, "prepare immutable history").await;
        assert!(is_applied(
            &invalidate_retained_check(&mut db, &permit).await
        ));
        let before: String = db.query_one("SELECT md5(o::text) FROM posted_file_retained_observations o WHERE generation_id = $1", &[&s.generation]).await.quiet("digest provenance").get(0);
        for _ in 0..2 {
            db.batch_execute(include_str!(
                "../../../../migrations/0052_posted_file_retained_observations.sql"
            ))
            .await
            .quiet("replay retained provenance migration");
        }
        for sql in ["UPDATE posted_file_retained_observations SET state = 'unavailable' WHERE generation_id = $1", "DELETE FROM posted_file_retained_observations WHERE generation_id = $1"] {
            let error = db.execute(sql, &[&s.generation]).await.expect_err("provenance mutation must refuse");
            assert!(error.as_db_error().is_some_and(|e| e.code().code() == "23514" && e.message() == "retained observation provenance is immutable"), "exact immutable trigger");
        }
        db.execute(
            "UPDATE posted_file_retained_observations SET state = state WHERE generation_id = $1",
            &[&s.generation],
        )
        .await
        .quiet("provenance no-op");
        let after: String = db.query_one("SELECT md5(o::text) FROM posted_file_retained_observations o WHERE generation_id = $1", &[&s.generation]).await.quiet("digest replayed provenance").get(0);
        assert!(before == after, "replay preserves exact provenance");
        assert!(is_stale(&invalidate_retained_check(&mut db, &permit).await));
        assert!(
            observation_count(&db, &s.generation).await == 1,
            "replay grants no fresh authority"
        );
    });
}
