//! Registry authority at admission with the narrow multi-generation
//! completion (0054): fenced reuse and terminal-failure replacement.
//!
//! The session-local read projection supplies a second physical identity for
//! each hash without touching custody. It detects the obsolete scalar lookup
//! even on a conflicting registry insert. This is a cardinality regression,
//! not track-wire-up/drain/upgrade acceptance.

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
fn postgres_admission_selected_health_reuses_usable_and_replaces_terminal_failed() {
    run_test(async {
        let mut db = connect().await;
        let (c, old) = legacy_case(&db).await;
        register(&db, &old).await;
        let attempt = acquire(&mut db, &c.job, &c.owner, 0).await;
        // Seeded policy states are not authenticated content or failure proof.
        for state in ["unknown", "verifying"] {
            db.execute(
                "UPDATE posted_file_health SET state = $2, failure_kind = NULL, \
                 verified_at = NULL, retention_checked_at = NULL \
                 WHERE generation_id = $1",
                &[&old.generation, &state],
            )
            .await
            .quiet("seed predecessor policy state");
            let before = snapshot(&db, &old).await;
            assert!(matches!(
                try_admit(&mut db, &attempt, &c.sealed).await,
                Ok(AdmissionOutcome::ClaimBusy)
            ));
            assert_eq!(snapshot(&db, &old).await, before);
            assert_eq!(operation_rows(&db, &c.sealed).await, 0);
        }
        // Usable winners reuse read-only: the stored bundle is returned and
        // no candidate, selection change or custody write is created.
        db.execute(
            "UPDATE posted_file_health SET state = 'usable', failure_kind = NULL, \
             verified_at = 1700000000, retention_checked_at = 1700000000 \
             WHERE generation_id = $1",
            &[&old.generation],
        )
        .await
        .quiet("seed predecessor usable state");
        let before = snapshot(&db, &old).await;
        match try_admit(&mut db, &attempt, &c.sealed).await {
            Ok(AdmissionOutcome::ReusedUsableWinner(bundle)) => {
                assert_eq!(bundle.generation_id(), old.generation);
                assert!(bundle.custody().expect("reused custody") == old.custody());
            }
            _ => panic!("usable selected winner was not reused"),
        }
        assert_eq!(snapshot(&db, &old).await, before);
        assert_eq!(operation_rows(&db, &c.sealed).await, 0);
        // Terminally failed winners admit exactly one replacement that
        // pins the failed generation as predecessor; selection stays put.
        db.execute(
            "UPDATE posted_file_health SET state = 'terminal_failed', \
             failure_kind = 'retention', verified_at = NULL, retention_checked_at = NULL \
             WHERE generation_id = $1",
            &[&old.generation],
        )
        .await
        .quiet("seed predecessor terminal failure");
        let health_revision: i64 = db
            .query_one(
                "SELECT revision FROM posted_file_health WHERE generation_id = $1",
                &[&old.generation],
            )
            .await
            .quiet("read predecessor health revision")
            .get(0);
        match try_admit(&mut db, &attempt, &c.sealed).await {
            Ok(AdmissionOutcome::Admitted(handle, bundle)) => {
                assert_eq!(handle.generation_id(), c.sealed.generation);
                assert_eq!(bundle.plan_sha256(), c.sealed.plan);
            }
            _ => panic!("terminal failed predecessor was not replaced"),
        }
        assert_eq!(operation_rows(&db, &c.sealed).await, 1);
        let op = db
            .query_one(
                "SELECT predecessor, predecessor_health_revision, state \
                 FROM posted_file_operations WHERE id = $1",
                &[&c.sealed.op],
            )
            .await
            .quiet("read replacement predecessor pin");
        assert_eq!(
            op.get::<_, Option<String>>(0).as_deref(),
            Some(old.generation.as_str())
        );
        assert_eq!(op.get::<_, Option<i64>>(1), Some(health_revision));
        assert_eq!(op.get::<_, &str>(2), "pending");
        // Selection still holds the failed predecessor while pending.
        let selected: Option<String> = db
            .query_one(
                "SELECT selected_generation FROM posted_file_registry WHERE plaintext_sha256 = $1",
                &[&old.hash],
            )
            .await
            .quiet("read registry selection")
            .get(0);
        assert_eq!(selected.as_deref(), Some(old.generation.as_str()));
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
