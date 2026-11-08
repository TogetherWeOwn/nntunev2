//! Actor revalidation and audit use the real control/reconciliation transactions.
//! These disposable tables do not prove session auth or ordered-schema acceptance.
use super::*;

async fn audits(app: &tokio_postgres::Client) -> i64 {
    app.query_one("SELECT count(*) FROM audit_log", &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn controls_and_reconciliation_refuse_missing_demoted_or_deleting_actors_without_mutation() {
    for reconcile in [false, true] {
        let (mut app, original, writing, _, _) = publication_fixture().await;
        if reconcile {
            app.batch_execute("UPDATE ingest_jobs SET admin_checkpoint = NULL")
                .await
                .unwrap();
        }
        let before = authority_row(&app).await;
        for (role, deleting, error) in [
            (Some("user"), None, StoreError::ActorNotAdmin),
            (None, None, StoreError::ActorNotAdmin),
            (Some("unknown"), None, StoreError::ActorNotAdmin),
            (Some("admin"), Some(1_i64), StoreError::AccountDeleting),
        ] {
            app.execute(
                "UPDATE users SET role = $1, deleting_at = $2",
                &[&role, &deleting],
            )
            .await
            .unwrap();
            let result = if reconcile {
                store::reconcile_null(
                    &mut app,
                    actor(),
                    &input(&original),
                    "running",
                    writing.revision(),
                )
                .await
            } else {
                store::apply_control(
                    &mut app,
                    actor(),
                    &input(&original),
                    &writing,
                    Control::Reject,
                )
                .await
            };
            assert_eq!(result.err(), Some(error));
            assert_eq!(authority_row(&app).await, before);
            assert_eq!(audits(&app).await, 0);
        }
        app.batch_execute("DELETE FROM users").await.unwrap();
        let result = if reconcile {
            store::reconcile_null(
                &mut app,
                actor(),
                &input(&original),
                "running",
                writing.revision(),
            )
            .await
        } else {
            store::apply_control(
                &mut app,
                actor(),
                &input(&original),
                &writing,
                Control::Reject,
            )
            .await
        };
        assert_eq!(result.err(), Some(StoreError::ActorNotAdmin));
        assert_eq!(authority_row(&app).await, before);
        assert_eq!(audits(&app).await, 0);
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn controls_and_reconciliation_reject_invalid_actor_context_before_database_work() {
    let (mut app, original, writing, _, _) = publication_fixture().await;
    // Missing tables would cause Database if actor validation reached SQL.
    app.batch_execute("DROP TABLE audit_log; DROP TABLE users")
        .await
        .unwrap();
    let long = "a".repeat(129);
    for context in [
        ControlActor {
            user_id: "",
            ..actor()
        },
        ControlActor {
            user_id: "a:b",
            ..actor()
        },
        ControlActor {
            user_id: "a\0b",
            ..actor()
        },
        ControlActor {
            user_id: &long,
            ..actor()
        },
        ControlActor { now: -1, ..actor() },
    ] {
        assert_eq!(
            store::apply_control(
                &mut app,
                context,
                &input(&original),
                &writing,
                Control::Reject
            )
            .await
            .err(),
            Some(StoreError::Invalid)
        );
        assert_eq!(
            store::reconcile_null(
                &mut app,
                context,
                &input(&original),
                "running",
                writing.revision()
            )
            .await
            .err(),
            Some(StoreError::Invalid)
        );
    }
    assert!(
        store::load(&app, &input(&original), writing.generation())
            .await
            .unwrap()
            == writing
    );
}

#[tokio::test]
async fn every_control_and_reconciliation_commit_one_fixed_redacted_audit_with_authority() {
    for (operation, control) in [
        ("approve", Some(Control::ApproveReview { release: RELEASE })),
        ("retry", Some(Control::RetryReview)),
        ("resume_archive", Some(Control::ResumeArchive)),
        ("reject", Some(Control::Reject)),
        ("reconcile", None),
    ] {
        let (mut app, original, writing, _, _) = publication_fixture().await;
        let mut cp = writing.checkpoint().clone();
        cp.state = if operation == "resume_archive" {
            JobState::NeedsArchiveRead
        } else {
            JobState::NeedsReview
        };
        let expected = writing.successor(cp, false, &identity(&original)).unwrap();
        seed(&app, &expected).await;
        if control.is_none() {
            app.batch_execute("UPDATE ingest_jobs SET status = 'running', admin_checkpoint = NULL")
                .await
                .unwrap();
        }
        let context = ControlActor {
            source_ip: Some("::1".parse().unwrap()),
            ..actor()
        };
        let saved = if let Some(control) = control {
            store::apply_control(&mut app, context, &input(&original), &expected, control)
                .await
                .unwrap()
        } else {
            store::reconcile_null(
                &mut app,
                context,
                &input(&original),
                "running",
                expected.revision(),
            )
            .await
            .unwrap()
        };
        let row = app.query_one("SELECT user_id, action, ok, source_ip, at, subject_user_id, details::text FROM audit_log", &[]).await.unwrap();
        assert_eq!(row.get::<_, String>(0), context.user_id);
        assert_eq!(row.get::<_, String>(1), "ingest.controlled");
        assert!(row.get::<_, bool>(2));
        assert_eq!(row.get::<_, String>(3), "::1");
        assert_eq!(row.get::<_, i64>(4), context.now);
        assert!(row.get::<_, Option<String>>(5).is_none());
        let details: serde_json::Value = serde_json::from_str(&row.get::<_, String>(6)).unwrap();
        assert_eq!(
            details,
            serde_json::json!({
                "job_id": original.job_id(), "operation": operation,
                "previous_status": if control.is_some() { expected.status() } else { "running" },
                "status": saved.status(), "previous_revision": expected.revision(),
                "revision": saved.revision(), "generation": saved.generation(),
            })
        );
        assert!(
            store::load(&app, &input(&original), saved.generation())
                .await
                .unwrap()
                == saved
        );
        // A duplicate old request cannot append an audit or rotate authority again.
        let replay = if let Some(control) = control {
            store::apply_control(&mut app, context, &input(&original), &expected, control).await
        } else {
            store::reconcile_null(
                &mut app,
                context,
                &input(&original),
                "running",
                expected.revision(),
            )
            .await
        };
        assert_eq!(replay.err(), Some(StoreError::Stale));
        assert_eq!(audits(&app).await, 1);
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn audit_error_or_zero_row_rolls_back_control_and_null_reconciliation() {
    for reconcile in [false, true] {
        let (mut app, original, writing, _, _) = publication_fixture().await;
        if reconcile {
            app.batch_execute("UPDATE ingest_jobs SET admin_checkpoint = NULL")
                .await
                .unwrap();
        }
        let before = authority_row(&app).await;
        for function in [
            "CREATE OR REPLACE FUNCTION pg_temp.refuse_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'refused'; END $$",
            "CREATE OR REPLACE FUNCTION pg_temp.refuse_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$",
        ] {
            app.batch_execute(function).await.unwrap();
            app.batch_execute("CREATE TRIGGER refuse_audit BEFORE INSERT ON audit_log FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_audit()").await.unwrap();
            let result = if reconcile {
                store::reconcile_null(&mut app, actor(), &input(&original), "running", writing.revision()).await
            } else {
                store::apply_control(&mut app, actor(), &input(&original), &writing, Control::Reject).await
            };
            assert_eq!(result.err(), Some(StoreError::Database));
            assert_eq!(authority_row(&app).await, before);
            assert_eq!(audits(&app).await, 0);
            assert_eq!(counts(&app).await, (0, 0, 0));
            app.batch_execute("DROP TRIGGER refuse_audit ON audit_log").await.unwrap();
        }
        // The rollback releases actor/admin/job locks and permits a later attempt.
        let saved = if reconcile {
            store::reconcile_null(
                &mut app,
                actor(),
                &input(&original),
                "running",
                writing.revision(),
            )
            .await
            .unwrap()
        } else {
            store::apply_control(
                &mut app,
                actor(),
                &input(&original),
                &writing,
                Control::Reject,
            )
            .await
            .unwrap()
        };
        assert_eq!(audits(&app).await, 1);
        assert!(
            store::load(&app, &input(&original), saved.generation())
                .await
                .unwrap()
                == saved
        );
        let source_ip: String = app
            .query_one("SELECT source_ip FROM audit_log", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(source_ip, "unknown");
    }
}
