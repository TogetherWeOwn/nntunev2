//! Registry authority at admission; not enabled multi-generation replacement.
//!
//! The session-local read projection supplies a second physical identity for
//! each hash without dropping the public unique constraint or touching custody.
//! It detects the obsolete scalar lookup even on a conflicting registry insert.
//! This is a cardinality regression, not migration/drain/upgrade acceptance.

use super::posted_generation_legacy_fence::insert_legacy;
use super::*;

async fn legacy_case(db: &Client) -> (Case, Sealed) {
    let c = case(db).await;
    assert_eq!(
        insert_legacy(db, &c.sealed)
            .await
            .quiet("insert legacy custody"),
        1
    );
    let mut old = c.sealed.clone();
    old.generation = db
        .query_one(select_posted_file_id_by_sha_sql(), &[&old.hash])
        .await
        .quiet("read legacy identity")
        .get(0);
    (c, old)
}

async fn register(db: &Client, old: &Sealed) {
    db.execute(
        "INSERT INTO posted_file_registry (plaintext_sha256, selected_generation) VALUES ($1, $2)",
        &[&old.hash, &old.generation],
    )
    .await
    .quiet("seed registered predecessor");
    db.execute(
        "INSERT INTO posted_file_health (generation_id) VALUES ($1)",
        &[&old.generation],
    )
    .await
    .quiet("seed unknown predecessor health");
}

async fn project_retained_history(db: &Client) {
    db.batch_execute(
        "CREATE TEMP VIEW posted_files AS \
         SELECT * FROM public.posted_files \
         UNION ALL SELECT (jsonb_populate_record(NULL::public.posted_files, \
             to_jsonb(p) || jsonb_build_object('id', 'history-' || p.id))).* \
         FROM public.posted_files p",
    )
    .await
    .quiet("project multiple retained physical identities");
}

async fn require_two_identities(db: &Client, hash: &str) {
    assert_eq!(
        count(
            db,
            "SELECT count(*) FROM posted_files WHERE plaintext_sha256 = $1",
            &[&hash]
        )
        .await,
        2
    );
    // This exact obsolete bootstrap mechanism must fail in the projection.
    // The real adapter below must classify its registry, not evaluate it.
    expect_db_error(
        db.query_one(
            "SELECT (SELECT id FROM posted_files WHERE plaintext_sha256 = $1)",
            &[&hash],
        )
        .await,
        &SqlState::CARDINALITY_VIOLATION,
        None,
        "obsolete scalar physical lookup",
    );
}

#[test]
fn postgres_admission_missing_legacy_registration_refuses_without_adopting_custody() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let before = snapshot(&db, &old).await;
        let xmin: String = db
            .query_one(
                "SELECT xmin::text FROM posted_files WHERE id = $1",
                &[&old.generation],
            )
            .await
            .quiet("read predecessor tuple version")
            .get(0);
        assert!(matches!(
            try_admit(&mut db, &attempt, &c.sealed).await,
            Ok(AdmissionOutcome::InconsistentOrMissing)
        ));
        assert_eq!(snapshot(&db, &old).await, before);
        assert_eq!(operation_rows(&db, &c.sealed).await, 0);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&old.hash]
            )
            .await,
            0
        );
        assert!(db
            .query_one(
                "SELECT xmin::text = $2 FROM posted_files WHERE id = $1",
                &[&old.generation, &xmin]
            )
            .await
            .quiet("unchanged predecessor tuple version")
            .get::<_, bool>(0));
    });
}

#[test]
fn postgres_admission_selected_health_never_implicitly_reuses_or_replaces() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        register(&db, &old).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        // Seeded policy states are not authenticated content or failure proof.
        for state in ["unknown", "verifying", "usable", "terminal_failed"] {
            db.execute(
                "UPDATE posted_file_health SET state = $2, \
                 failure_kind = CASE WHEN $2 = 'terminal_failed' THEN 'retention' ELSE NULL END, \
                 verified_at = CASE WHEN $2 = 'usable' THEN 1700000000 ELSE NULL END, \
                 retention_checked_at = CASE WHEN $2 = 'usable' THEN 1700000000 ELSE NULL END \
                 WHERE generation_id = $1",
                &[&old.generation, &state],
            )
            .await
            .quiet("seed predecessor policy state");
            let before = snapshot(&db, &old).await;
            let result = try_admit(&mut db, &attempt, &c.sealed).await;
            if matches!(state, "usable" | "terminal_failed") {
                assert!(matches!(
                    result,
                    Ok(AdmissionOutcome::InconsistentOrMissing)
                ));
            } else {
                assert!(matches!(result, Ok(AdmissionOutcome::ClaimBusy)));
            }
            assert_eq!(snapshot(&db, &old).await, before);
            assert_eq!(operation_rows(&db, &c.sealed).await, 0);
        }
    });
}

#[test]
fn postgres_admission_registered_history_uses_registry_without_scalar_physical_lookup() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        register(&db, &old).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let before = snapshot(&db, &old).await;
        project_retained_history(&db).await;
        require_two_identities(&db, &old.hash).await;
        assert!(matches!(
            try_admit(&mut db, &attempt, &c.sealed).await,
            Ok(AdmissionOutcome::ClaimBusy)
        ));
        assert_eq!(snapshot(&db, &old).await, before);
        assert_eq!(operation_rows(&db, &c.sealed).await, 0);
    });
}

#[test]
fn postgres_admission_unregistered_history_is_inconsistent_not_an_arbitrary_winner() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let before = snapshot(&db, &old).await;
        project_retained_history(&db).await;
        require_two_identities(&db, &old.hash).await;
        assert!(matches!(
            try_admit(&mut db, &attempt, &c.sealed).await,
            Ok(AdmissionOutcome::InconsistentOrMissing)
        ));
        assert_eq!(snapshot(&db, &old).await, before);
        assert_eq!(operation_rows(&db, &c.sealed).await, 0);
    });
}

#[test]
fn postgres_admission_pending_history_replays_only_the_exact_stored_candidate() {
    run_test(async {
        let mut db = connect().await;
        let c = case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let handle = admitted(&mut db, &attempt, &c.sealed).await;
        let before = snapshot(&db, &c.sealed).await;
        project_retained_history(&db).await;
        require_two_identities(&db, &c.sealed.hash).await;
        match try_admit(&mut db, &attempt, &c.sealed).await {
            Ok(AdmissionOutcome::ResumedSameCandidate(replayed, bundle)) => {
                assert!(replayed == handle);
                assert!(bundle.custody().expect("exact stored custody") == c.sealed.custody());
                assert_eq!(bundle.generation_id(), c.sealed.generation);
            }
            _ => panic!("registered history did not replay the exact candidate"),
        }
        let mut divergent = c.sealed.clone();
        divergent.key = vec![0x73; divergent.key.len()];
        assert!(matches!(
            try_admit(&mut db, &attempt, &divergent).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(snapshot(&db, &c.sealed).await, before);
    });
}

#[test]
fn postgres_admission_history_does_not_repair_a_stale_registry_observation() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        let absent = observe(&db, &old.hash)
            .await
            .expect("observe absent registry");
        register(&db, &old).await;
        let before = snapshot(&db, &old).await;
        project_retained_history(&db).await;
        require_two_identities(&db, &old.hash).await;
        assert!(matches!(
            admit(&mut db, &attempt, &absent, &c.sealed.mint()).await,
            Ok(AdmissionOutcome::StaleOrDiverged)
        ));
        assert_eq!(snapshot(&db, &old).await, before);
        assert_eq!(operation_rows(&db, &c.sealed).await, 0);
    });
}
