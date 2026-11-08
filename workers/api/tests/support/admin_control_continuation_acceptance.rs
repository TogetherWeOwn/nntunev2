//! Internal control-to-send composition, not session routing or crash recovery.
use super::*;
use store::DeliveryError;

async fn prepared(operation: &str) -> (Client, OriginalBody, Snapshot) {
    let (app, original, writing, _, _) = publication_fixture().await;
    if operation == "reconcile" {
        app.batch_execute("UPDATE ingest_jobs SET admin_checkpoint = NULL")
            .await
            .unwrap();
        return (app, original, writing);
    }
    let mut cp = writing.checkpoint().clone();
    cp.state = if operation == "resume_archive" {
        JobState::NeedsArchiveRead
    } else {
        JobState::NeedsReview
    };
    let expected = writing.successor(cp, false, &identity(&original)).unwrap();
    seed(&app, &expected).await;
    (app, original, expected)
}

async fn complete(
    app: &mut Client,
    original: &OriginalBody,
    expected: &Snapshot,
    operation: &str,
    queue: &mut impl store::DeliverySender,
) -> Result<store::ControlCompletion, StoreError> {
    if operation == "reconcile" {
        store::reconcile_null_and_enqueue(
            app,
            actor(),
            &input(original),
            expected.status(),
            expected.revision(),
            queue,
        )
        .await
    } else {
        let control = match operation {
            "approve" => Control::ApproveReview { release: RELEASE },
            "retry" => Control::RetryReview,
            "resume_archive" => Control::ResumeArchive,
            "reject" => Control::Reject,
            _ => panic!("unknown test operation"),
        };
        store::apply_control_and_enqueue(app, actor(), &input(original), expected, control, queue)
            .await
    }
}

async fn audit_count(app: &Client) -> i64 {
    app.query_one("SELECT count(*) FROM audit_log", &[])
        .await
        .unwrap()
        .get(0)
}

struct ObservedQueue<'a> {
    observer: &'a Client,
    generation: i64,
    operation: &'a str,
    inner: DeliveryQueue,
}

impl store::DeliverySender for ObservedQueue<'_> {
    type Error = StoreError;

    async fn send(&mut self, encoded: String, delay_secs: u32) -> Result<(), Self::Error> {
        // A separate connection must see the mutation AND audit before send.
        // No snapshot/audit could be visible here if send were under that tx.
        let row = self.observer.query_one(
            "SELECT admin_checkpoint_revision, (admin_checkpoint->>'generation')::bigint FROM ingest_jobs",
            &[],
        ).await.unwrap();
        assert_eq!(row.get::<_, i64>(0), self.generation);
        assert_eq!(row.get::<_, i64>(1), self.generation);
        let audit = self
            .observer
            .query_one(
                "SELECT details->>'operation', (details->>'revision')::bigint FROM audit_log",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(audit.get::<_, String>(0), self.operation);
        assert_eq!(audit.get::<_, i64>(1), self.generation);
        assert_eq!(audit_count(self.observer).await, 1);
        assert_eq!(delay_secs, 0);
        assert_eq!(
            store::Delivery::decode(&encoded).unwrap().generation(),
            self.generation
        );
        store::DeliverySender::send(&mut self.inner, encoded, delay_secs).await
    }
}

#[tokio::test]
async fn controls_enqueue_only_the_committed_audited_generation_and_rejection_sends_nothing() {
    for operation in ["approve", "retry", "resume_archive", "reject", "reconcile"] {
        let (app, original, expected) = prepared(operation).await;
        let rows = SharedRows::copy_from(&app).await;
        let mut controller = rows.client().await;
        let mut queue = ObservedQueue {
            observer: &rows.observer,
            generation: expected.revision() + 1,
            operation,
            inner: DeliveryQueue::default(),
        };
        let result = complete(&mut controller, &original, &expected, operation, &mut queue)
            .await
            .unwrap();
        let saved = store::load(
            &rows.observer,
            &input(&original),
            result.snapshot.generation(),
        )
        .await
        .unwrap();
        assert!(saved == result.snapshot);
        assert!(saved.generation() > expected.generation());
        assert_eq!(result.continuation, Ok(operation != "reject"));
        assert_eq!(queue.inner.calls, usize::from(operation != "reject"));
        assert_eq!(audit_count(&rows.observer).await, 1);
        assert_eq!(counts(&rows.observer).await, (0, 0, 0));
        drop(queue);
        rows.cleanup().await;
    }
}

#[tokio::test]
async fn control_send_recovery_preserves_audit_and_refuses_later_generations() {
    for operation in ["retry", "reconcile"] {
        for uncertain in [false, true] {
            let (app, original, expected) = prepared(operation).await;
            let rows = SharedRows::copy_from(&app).await;
            let mut controller = rows.client().await;
            let mut queue = ObservedQueue {
                observer: &rows.observer,
                generation: expected.revision() + 1,
                operation,
                inner: DeliveryQueue {
                    reject: !uncertain,
                    uncertain,
                    ..Default::default()
                },
            };
            let result = complete(&mut controller, &original, &expected, operation, &mut queue)
                .await
                .unwrap();
            assert_eq!(result.continuation, Err(DeliveryError::Send));
            assert_eq!(queue.inner.messages.len(), usize::from(uncertain));
            let before = authority_row(&rows.observer).await;
            queue.inner.reject = false;
            queue.inner.uncertain = false;
            for _ in 0..2 {
                assert_eq!(
                    store::enqueue_delivery(
                        &controller,
                        &input(&original),
                        result.snapshot.generation(),
                        &mut queue,
                    )
                    .await,
                    Ok(true)
                );
                assert_eq!(authority_row(&rows.observer).await, before);
            }
            assert_eq!(
                complete(&mut controller, &original, &expected, operation, &mut queue)
                    .await
                    .err(),
                Some(StoreError::Stale)
            );
            assert_eq!(audit_count(&rows.observer).await, 1);
            let rejected = store::apply_control(
                &mut controller,
                actor(),
                &input(&original),
                &result.snapshot,
                Control::Reject,
            )
            .await
            .unwrap();
            let winner = authority_row(&rows.observer).await;
            assert!(rejected.generation() > result.snapshot.generation());
            assert_eq!(
                store::enqueue_delivery(
                    &controller,
                    &input(&original),
                    result.snapshot.generation(),
                    &mut queue,
                )
                .await,
                Err(DeliveryError::Authority(StoreError::Stale))
            );
            assert_eq!(queue.inner.calls, 3);
            assert_eq!(authority_row(&rows.observer).await, winner);
            assert_eq!(audit_count(&rows.observer).await, 2);
            assert_eq!(counts(&rows.observer).await, (0, 0, 0));
            drop(queue);
            rows.cleanup().await;
        }
    }
}

#[tokio::test]
async fn control_enqueue_refuses_actor_audit_and_update_failures_without_sends() {
    for operation in ["retry", "reconcile"] {
        let (mut app, original, expected) = prepared(operation).await;
        let mut queue = DeliveryQueue::default();
        let before = authority_row(&app).await;
        app.batch_execute("UPDATE users SET role = 'user'")
            .await
            .unwrap();
        assert_eq!(
            complete(&mut app, &original, &expected, operation, &mut queue)
                .await
                .err(),
            Some(StoreError::ActorNotAdmin)
        );
        app.batch_execute("UPDATE users SET role = 'admin'")
            .await
            .unwrap();
        for (table, body, error) in [
            (
                "audit_log",
                "RAISE EXCEPTION 'refused';",
                StoreError::Database,
            ),
            ("audit_log", "RETURN NULL;", StoreError::Database),
            (
                "ingest_jobs",
                "RAISE EXCEPTION 'refused';",
                StoreError::Database,
            ),
            ("ingest_jobs", "RETURN NULL;", StoreError::Stale),
        ] {
            app.batch_execute(&format!(
                "CREATE OR REPLACE FUNCTION pg_temp.refuse_control_send() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN {body} END $$;
                 CREATE TRIGGER refuse_control_send BEFORE INSERT OR UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION pg_temp.refuse_control_send()"
            )).await.unwrap();
            assert_eq!(
                complete(&mut app, &original, &expected, operation, &mut queue)
                    .await
                    .err(),
                Some(error)
            );
            assert_eq!(authority_row(&app).await, before);
            assert_eq!(audit_count(&app).await, 0);
            assert_eq!(queue.calls, 0);
            app.batch_execute(&format!("DROP TRIGGER refuse_control_send ON {table}"))
                .await
                .unwrap();
        }
        assert_eq!(counts(&app).await, (0, 0, 0));
    }
}

#[tokio::test]
async fn cancelled_control_send_keeps_committed_authority_without_repeating_mutation() {
    for operation in ["retry", "reconcile"] {
        let (app, original, expected) = prepared(operation).await;
        let rows = SharedRows::copy_from(&app).await;
        let mut controller = rows.client().await;
        let mut queue = ObservedQueue {
            observer: &rows.observer,
            generation: expected.revision() + 1,
            operation,
            inner: DeliveryQueue {
                pending: true,
                ..Default::default()
            },
        };
        assert!(tokio::time::timeout(
            Duration::from_secs(2),
            complete(&mut controller, &original, &expected, operation, &mut queue,)
        )
        .await
        .is_err());
        assert_eq!(queue.inner.calls, 1);
        // Test-only outcome resolution via the independent healthy connection;
        // a timeout itself does not prove whether a mutation committed.
        let saved = store::load(&rows.observer, &input(&original), queue.generation)
            .await
            .unwrap();
        let before = authority_row(&rows.observer).await;
        queue.inner.pending = false;
        assert_eq!(
            store::enqueue_delivery(
                &controller,
                &input(&original),
                saved.generation(),
                &mut queue,
            )
            .await,
            Ok(true)
        );
        assert_eq!(
            complete(&mut controller, &original, &expected, operation, &mut queue)
                .await
                .err(),
            Some(StoreError::Stale)
        );
        assert_eq!(queue.inner.calls, 2);
        assert_eq!(authority_row(&rows.observer).await, before);
        assert_eq!(audit_count(&rows.observer).await, 1);
        assert_eq!(counts(&rows.observer).await, (0, 0, 0));
        drop(queue);
        rows.cleanup().await;
    }
}
