//! Disposable Worker-only controls for the dormant owned reconciliation store.
use super::{billing_budget, MirrorBinding, PgStore, Store, StoreError};
use crate::billing_reconcile::{PaidInvoiceRow, ReconcileOutcome, ReconciledRow};
use crate::billing_transaction_sql::ReconcileProbe;
pub(super) const CONNECTION: &str =
    "host=127.0.0.1 user=postgres password=ci-throwaway dbname=nntune_billing_ack sslmode=disable";
async fn fixture(setup: bool) -> Result<(), StoreError> {
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            if setup {
                // Serialize our own fixture setups and refuse every existing public
                // relation: the migrations also alter or drop other billing tables.
                client.batch_execute("BEGIN").await.map_err(|_| StoreError)?;
                let safe: bool = client.query_one(
                    "SELECT pg_try_advisory_xact_lock(781349051) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public')", &[]
                ).await.map_err(|_| StoreError)?.get(0);
                if !safe { return Err(StoreError); }
                for migration in [
                    include_str!("../../../migrations/0002_auth_vault.sql"),
                    include_str!("../../../migrations/0021_billing_schema.sql"),
                    include_str!("../../../migrations/0026_plan_reseed_no_trial.sql"),
                    include_str!("../../../migrations/0029_paid_invoice_history.sql"),
                    include_str!("../../../migrations/0059_paid_invoice_covered_periods.sql"),
                ] {
                    client.batch_execute(migration).await.map_err(|_| StoreError)?;
                }
                client.batch_execute("CREATE FUNCTION public.worker_billing_cancel_barrier() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.stripe_invoice_id = 'in_worker_cancel' THEN PERFORM pg_advisory_xact_lock(781349052); END IF; RETURN NEW; END $$; CREATE TRIGGER worker_billing_cancel_barrier BEFORE INSERT ON public.paid_invoice_history FOR EACH ROW EXECUTE FUNCTION public.worker_billing_cancel_barrier()").await.map_err(|_| StoreError)?;
                // A sequence advance survives rollback, unlike a witness row.
                // Observe the preceding writes inside the failing transaction.
                client.batch_execute(r#"
                    CREATE SEQUENCE public.worker_billing_late_insert;
                    CREATE FUNCTION public.worker_billing_note_late_insert() RETURNS trigger
                    LANGUAGE plpgsql AS $$ BEGIN
                        IF EXISTS (SELECT 1 FROM public.subscriptions s
                                   WHERE s.user_id = NEW.user_id
                                     AND s.stripe_subscription_id = NEW.stripe_subscription_id
                                     AND s.paid_invoice_id = NEW.stripe_invoice_id)
                           AND EXISTS (SELECT 1 FROM public.paid_invoice_history h
                                       WHERE h.user_id = NEW.user_id
                                         AND h.stripe_subscription_id = NEW.stripe_subscription_id
                                         AND h.stripe_invoice_id = 'in_worker_first') THEN
                            PERFORM nextval('public.worker_billing_late_insert');
                        END IF;
                        RETURN NEW;
                    END $$;
                    CREATE TRIGGER worker_billing_late_insert BEFORE INSERT
                    ON public.paid_invoice_history FOR EACH ROW
                    WHEN (NEW.stripe_invoice_id = 'in_worker_second' AND NEW.amount_cents = -1)
                    EXECUTE FUNCTION public.worker_billing_note_late_insert();
                "#).await.map_err(|_| StoreError)?;
                client.batch_execute("INSERT INTO public.users (id,email,created_at) VALUES ('worker_fault_user','worker-fault@example.test',1786032000); COMMIT").await.map_err(|_| StoreError)?;
            } else {
                let row = client.query_one("SELECT (SELECT count(*) FROM public.subscriptions), (SELECT count(*) FROM public.paid_invoice_history)", &[]).await.map_err(|_| StoreError)?;
                if row.get::<_, i64>(0) != 0 || row.get::<_, i64>(1) != 0 { return Err(StoreError); }
            }
            Ok(())
        }),
        |_| {},
    ).await
}
async fn expire_after_writes(
    outcome: &ReconcileOutcome,
    probe: ReconcileProbe,
) -> Result<(), StoreError> {
    use futures_util::future::{select, Either};
    use std::time::Duration;
    let expired = std::rc::Rc::new(std::cell::Cell::new(false));
    let store = PgStore {
        env: None,
        trace_id: "worker-deadline-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: Some(expired.clone()),
        cancel_token_observer: None,
    };
    let server_watchdog = matches!(probe, ReconcileProbe::ServerWatchdog);
    let mut operation =
        Box::pin(store.store_reconcile_owned("worker_fault_user".into(), outcome.clone(), probe));
    let observe = billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |client| {
            Box::pin(async move {
                for _ in 0..40 {
                    let rows = client.query(
                        "SELECT a.pid FROM pg_catalog.pg_stat_activity a
                         WHERE a.application_name = 'owned_billing_reconcile_test'
                           AND a.state = 'idle in transaction'
                           AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid
                               AND l.locktype = 'advisory' AND l.classid = 0 AND l.objid = 781349053 AND l.granted)
                           AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid = a.pid
                               AND l.relation = 'public.subscriptions'::regclass AND l.mode = 'ShareRowExclusiveLock' AND l.granted)",
                        &[],
                    ).await.map_err(|_| StoreError)?;
                    if rows.len() > 1 {
                        return Err(StoreError);
                    }
                    if let Some(row) = rows.first() {
                        return Ok(row.get::<_, i32>(0));
                    }
                    worker::Delay::from(Duration::from_millis(50)).await;
                }
                Err(StoreError)
            })
        },
        |_| {},
    );
    let pid = match select(operation.as_mut(), Box::pin(observe)).await {
        Either::Left(_) => return Err(StoreError),
        Either::Right((result, _)) => result?,
    };
    if server_watchdog {
        // Keep the socket owner alive but stop polling its driver and timers.
        // No client close, COMMIT or CancelRequest can explain this cleanup.
        wait_for_transaction_cleanup(pid, 140).await?;
        drop(operation);
        if expired.get() {
            return Err(StoreError);
        }
    } else {
        // Require the operation to remain pending beyond PostgreSQL's normal
        // five-second statement limit; then the Worker's bounded work must end it.
        if let Either::Left(_) = select(
            operation.as_mut(),
            Box::pin(worker::Delay::from(Duration::from_secs(5))),
        )
        .await
        {
            return Err(StoreError);
        }
        if operation.await.is_ok() || !expired.get() {
            return Err(StoreError);
        }
        wait_for_transaction_cleanup(pid, 40).await?;
    }
    fixture(false).await
}

async fn wait_for_transaction_cleanup(pid: i32, samples: u32) -> Result<(), StoreError> {
    use std::time::Duration;

    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            for _ in 0..samples {
                let present: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1) OR EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1)", &[&pid]).await.map_err(|_| StoreError)?.get(0);
                if !present { return Ok(()); }
                worker::Delay::from(Duration::from_millis(50)).await;
            }
            Err(StoreError)
        }),
        |_| {},
    ).await?;
    fixture(false).await
}
fn test_outcome() -> ReconcileOutcome {
    let history = |invoice: &str, amount| PaidInvoiceRow {
        stripe_invoice_id: invoice.into(),
        stripe_subscription_id: "sub_worker_fault".into(),
        plan_id: "plus".into(),
        period_start_unix: 1_786_032_000,
        period_end_unix: 1_786_032_100,
        amount_cents: amount,
        currency: "usd".into(),
        paid_at_unix: 1_786_032_000,
        covered_periods: vec![(1_786_032_000, 1_786_032_100)],
    };
    ReconcileOutcome {
        row: ReconciledRow {
            stripe_subscription_id: "sub_worker_fault".into(),
            stripe_customer_id: "cus_worker_fault".into(),
            stripe_item_id: "si_worker_fault".into(),
            stripe_price_id: "price_worker_fault".into(),
            plan_id: "plus".into(),
            status: "active".into(),
            current_period_end_unix: Some(1_786_032_100),
            cancel_at_period_end: false,
            paid_through_unix: Some(1_786_032_100),
            paid_invoice_id: Some("in_worker_second".into()),
            grace_until_unix: None,
        },
        history: vec![
            history("in_worker_first", 800),
            history("in_worker_second", 800),
        ],
        fail_closed: None,
        explicit_stale: false,
        read_plan: None,
        read_anchor: None,
    }
}
pub(super) const RESTRICTED_HISTORY_TRACE: &str = "worker-restricted-history-test";

pub(crate) async fn test_restricted_history(replay: bool) -> Result<(), StoreError> {
    let store = PgStore {
        env: None,
        trace_id: RESTRICTED_HISTORY_TRACE.into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let mut outcome = test_outcome();
    if replay {
        outcome.read_plan = Some(outcome.row.plan_id.clone());
        outcome.read_anchor = outcome.row.paid_invoice_id.clone();
    }
    store
        .store_reconcile_owned("worker_fault_user".into(), outcome, ReconcileProbe::None)
        .await
}

pub(crate) async fn test_reconcile_raw_update() -> Result<(), &'static str> {
    use futures_util::future::{select, Either};
    use std::{cell::Cell, rc::Rc};

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-raw-update-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let user = "worker_fault_user";
    store
        .store_reconcile_owned(user.into(), test_outcome(), ReconcileProbe::None)
        .await
        .map_err(|_| "raw update: seed through dormant store failed")?;
    let mut outcome = test_outcome();
    outcome.read_plan = Some(outcome.row.plan_id.clone());
    outcome.read_anchor = outcome.row.paid_invoice_id.clone();
    outcome.history.truncate(1);
    outcome.history[0].stripe_invoice_id = "in_worker_raw_update".into();

    let phase = Rc::new(Cell::new("raw update: open writer transaction"));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |writer| Box::pin(async move {
            writer.batch_execute("BEGIN").await.map_err(|_| StoreError)?;
            phase.set("raw update: advance the existing subscription");
            let committed: String = writer.query_one(
                "UPDATE public.subscriptions AS s
                 SET paid_through_at = paid_through_at + interval '1 hour', cancel_at_period_end = true
                 WHERE user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'
                 RETURNING to_jsonb(s)::text", &[],
            ).await.map_err(|_| StoreError)?.get(0);

            let mut reconcile = Box::pin(store.store_reconcile_owned(user.into(), outcome, ReconcileProbe::None));
            // Do not release the writer until the exact table lock is observed waiting on it.
            phase.set("raw update: Worker table lock did not wait on the writer");
            match select(reconcile.as_mut(), Box::pin(wait_for_raw_writer(writer))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((observed, _)) => observed?,
            }
            phase.set("raw update: preceding writer did not commit");
            writer.batch_execute("COMMIT").await.map_err(|_| StoreError)?;
            phase.set("raw update: reconciliation did not accept the now-stale outcome");
            reconcile.await?;

            phase.set("raw update: subscription snapshot or attributed history differed");
            let row = writer.query_one(
                "SELECT (SELECT to_jsonb(s)::text FROM public.subscriptions s),
                        (SELECT bool_and(user_id = 'worker_fault_user'
                                         AND stripe_subscription_id = 'sub_worker_fault'
                                         AND plan_id = 'plus' AND amount_cents = 800 AND currency = 'usd')
                             AND array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                 = ARRAY['in_worker_first', 'in_worker_raw_update', 'in_worker_second']
                         FROM public.paid_invoice_history)", &[],
            ).await.map_err(|_| StoreError)?;
            if row.get::<_, Option<String>>(0).as_deref() != Some(committed.as_str())
                || row.get::<_, Option<bool>>(1) != Some(true)
            {
                return Err(StoreError);
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

async fn wait_for_raw_writer(writer: &tokio_postgres::Client) -> Result<(), StoreError> {
    for _ in 0..40 {
        let blocked: bool = writer
            .query_one(
                "SELECT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_locks l
                 WHERE l.relation = 'public.subscriptions'::regclass
                   AND l.mode = 'ShareRowExclusiveLock' AND NOT l.granted
                   AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid)))",
                &[],
            )
            .await
            .map_err(|_| StoreError)?
            .get(0);
        if blocked {
            return Ok(());
        }
        worker::Delay::from(std::time::Duration::from_millis(50)).await;
    }
    Err(StoreError)
}

// Exercise a genuinely absent key, not a DELETE/reinsert of an existing target.
pub(crate) async fn test_reconcile_after_raw_insert(commit: bool) -> Result<(), &'static str> {
    use futures_util::future::{select, Either};
    use std::{cell::Cell, rc::Rc};

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-after-raw-insert-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let phase = Rc::new(Cell::new("raw INSERT: connect writer"));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |writer| Box::pin(async move {
            phase.set("raw INSERT: seed contender with an initially empty target");
            writer.batch_execute(
                "INSERT INTO public.users (id, email, created_at)
                 VALUES ('worker_insert_user', 'worker-insert@example.test', 1786032000)",
            ).await.map_err(|_| StoreError)?;
            let before = parent_delete_snapshot(writer, None).await?;
            if before.1.is_some() || before.2.is_some() {
                return Err(StoreError);
            }
            writer.batch_execute("BEGIN; SET LOCAL statement_timeout = '5s'")
                .await.map_err(|_| StoreError)?;
            let inserted: String = writer.query_one(
                "INSERT INTO public.subscriptions AS s
                    (user_id, stripe_subscription_id, stripe_customer_id,
                     stripe_subscription_item_id, stripe_price_id, plan_id, status)
                 VALUES ('worker_insert_user', 'sub_worker_fault', 'cus_worker_insert',
                         'si_worker_insert', 'price_worker_fault', 'plus', 'active')
                 RETURNING jsonb_build_array(to_jsonb(s))::text", &[],
            ).await.map_err(|_| StoreError)?.get(0);

            phase.set("raw INSERT: Worker table lock did not wait on the writer");
            let refused = Rc::new(Cell::new(false));
            let mut reconcile = Box::pin(store.store_reconcile_owned(
                "worker_fault_user".into(), test_outcome(), ReconcileProbe::ObserveTargetRefusal(refused.clone()),
            ));
            match select(reconcile.as_mut(), Box::pin(wait_for_raw_writer(writer))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((observed, _)) => observed?,
            }
            phase.set("raw INSERT: preceding writer did not finish");
            writer.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
                .await.map_err(|_| StoreError)?;
            phase.set("raw INSERT: wrong reconciliation result after the writer finished");
            if reconcile.await.is_ok() == commit || refused.get() != commit {
                return Err(StoreError);
            }

            phase.set("raw INSERT: wrong durable owner or history");
            if commit {
                if parent_delete_snapshot(writer, None).await? != (before.0, Some(inserted), None) {
                    return Err(StoreError);
                }
            } else {
                let valid: bool = writer.query_one(
                    "SELECT (SELECT count(*) = 1 AND bool_and(user_id = 'worker_fault_user'
                                      AND stripe_subscription_id = 'sub_worker_fault'
                                      AND status = 'active' AND paid_invoice_id = 'in_worker_second')
                             FROM public.subscriptions)
                        AND (SELECT count(*) = 2 AND bool_and(user_id = 'worker_fault_user'
                                      AND stripe_subscription_id = 'sub_worker_fault'
                                      AND plan_id = 'plus' AND amount_cents = 800 AND currency = 'usd')
                                 AND array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                     = ARRAY['in_worker_first', 'in_worker_second']
                             FROM public.paid_invoice_history)", &[],
                ).await.map_err(|_| StoreError)?.get::<_, Option<bool>>(0).unwrap_or(false);
                if !valid || parent_delete_snapshot(writer, None).await?.0 != before.0 {
                    return Err(StoreError);
                }
            }
            phase.set("raw INSERT: billing lock survived completion");
            let clear: bool = writer.query_one(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks
                   WHERE relation = 'public.subscriptions'::regclass AND database =
                       (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()))", &[],
            ).await.map_err(|_| StoreError)?.get(0);
            if !clear {
                return Err(StoreError);
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

pub(crate) async fn test_reconcile_before_raw_insert(commit: bool) -> Result<(), &'static str> {
    use crate::billing_transaction_sql::PRECOMMIT_GATE;
    use futures_util::future::{join, select, Either};
    use std::{cell::Cell, rc::Rc};
    use tokio_postgres::error::SqlState;

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-before-raw-insert-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let phase = Rc::new(Cell::new(
        "Worker-first INSERT: connect independent observer",
    ));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |db| Box::pin(async move {
            phase.set("Worker-first INSERT: seed contender with an empty target");
            db.batch_execute("INSERT INTO public.users (id, email, created_at)
                              VALUES ('worker_insert_user', 'worker-insert@example.test', 1786032000)")
                .await.map_err(|_| StoreError)?;
            let before = parent_delete_snapshot(db, None).await?;
            if before.1.is_some() || before.2.is_some() {
                return Err(StoreError);
            }
            phase.set("Worker-first INSERT: Worker did not reach pre-COMMIT gate with its billing writes and exclusion");
            db.query_one("SELECT pg_advisory_lock($1)", &[&PRECOMMIT_GATE])
                .await.map_err(|_| StoreError)?;
            let injected = Rc::new(Cell::new(false));
            let probe = ReconcileProbe::BeforeInsertCommit((!commit).then(|| injected.clone()));
            let mut reconcile = Box::pin(store.store_reconcile_owned(
                "worker_fault_user".into(), test_outcome(), probe,
            ));
            let worker = match select(reconcile.as_mut(), Box::pin(wait_lock_cycle(|| async {
                let row = db.query_opt(
                    "SELECT l.pid FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_stat_activity a USING (pid)
                     WHERE l.relation = 'public.subscriptions'::regclass AND l.granted
                       AND l.mode = 'ShareRowExclusiveLock'
                       AND a.application_name = 'owned_billing_reconcile_test'
                       AND a.wait_event_type = 'Lock' AND a.wait_event = 'advisory'
                       AND a.query = 'SELECT pg_advisory_xact_lock($1)'
                       AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid))", &[],
                ).await.map_err(|_| StoreError)?;
                Ok(row.map(|row| row.get::<_, i32>(0)))
            }))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((worker, _)) => worker?,
            };

            let writer_pid = Rc::new(Cell::new(None::<i32>));
            let record_pid = writer_pid.clone();
            let mut insert = Box::pin(billing_budget::with_owned_connection(
                "127.0.0.1", 5432, CONNECTION,
                |_| Box::pin(async { Ok(()) }),
                move |raw| Box::pin(async move {
                    raw.batch_execute("BEGIN; SET LOCAL statement_timeout = '5s'")
                        .await.map_err(|_| StoreError)?;
                    record_pid.set(Some(raw.query_one("SELECT pg_backend_pid()", &[])
                        .await.map_err(|_| StoreError)?.get(0)));
                    let inserted = raw.query_one(
                        "INSERT INTO public.subscriptions AS s
                            (user_id, stripe_subscription_id, stripe_customer_id,
                             stripe_subscription_item_id, stripe_price_id, plan_id, status)
                         VALUES ('worker_insert_user', 'sub_worker_fault', 'cus_worker_insert',
                                 'si_worker_insert', 'price_worker_fault', 'plus', 'active')
                         RETURNING jsonb_build_array(to_jsonb(s))::text", &[],
                    ).await;
                    match inserted {
                        Ok(row) if !commit => {
                            raw.batch_execute("COMMIT").await.map_err(|_| StoreError)?;
                            Ok(Some(row.get::<_, String>(0)))
                        }
                        Err(error) if commit && error.as_db_error().is_some_and(|err|
                            err.code() == &SqlState::UNIQUE_VIOLATION
                                && err.constraint() == Some("subscriptions_stripe_subscription_id_key")) => {
                            raw.batch_execute("ROLLBACK").await.map_err(|_| StoreError)?;
                            Ok(None)
                        }
                        _ => Err(StoreError),
                    }
                }),
                |_| {},
            ));
            phase.set("Worker-first INSERT: exact writer did not wait on Worker exclusion");
            let writer = match select(insert.as_mut(), Box::pin(wait_lock_cycle(|| async {
                let Some(pid) = writer_pid.get() else { return Ok(None); };
                let waiting: bool = db.query_one(
                    "SELECT $2 = ANY(pg_blocking_pids($1)) AND EXISTS (
                        SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1
                          AND relation = 'public.subscriptions'::regclass
                          AND mode = 'RowExclusiveLock' AND NOT granted)", &[&pid, &worker],
                ).await.map_err(|_| StoreError)?.get(0);
                Ok(waiting.then_some(pid))
            }))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((writer, _)) => writer?,
            };
            phase.set("Worker-first INSERT: a plain reader did not see the original empty snapshot");
            if parent_delete_snapshot(db, None).await? != before {
                return Err(StoreError);
            }
            let released: bool = db.query_one("SELECT pg_advisory_unlock($1)", &[&PRECOMMIT_GATE])
                .await.map_err(|_| StoreError)?.get(0);
            if !released {
                return Err(StoreError);
            }
            phase.set("Worker-first INSERT: wrong Worker result or raw INSERT did not finish as expected");
            let (reconciled, inserted) = join(reconcile, insert).await;
            let inserted = inserted?;
            if reconciled.is_ok() != commit || injected.get() == commit {
                return Err(StoreError);
            }
            phase.set("Worker-first INSERT: wrong durable owner/history or retained billing locks");
            let after = parent_delete_snapshot(db, None).await?;
            let valid: bool = db.query_one(
                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid IN ($1, $2)
                    AND relation IN ('public.users'::regclass, 'public.subscriptions'::regclass,
                                     'public.paid_invoice_history'::regclass))
                    AND (NOT $3 OR (
                        (SELECT count(*) = 1 AND bool_and(user_id = 'worker_fault_user'
                             AND stripe_subscription_id = 'sub_worker_fault' AND status = 'active'
                             AND paid_invoice_id = 'in_worker_second') FROM public.subscriptions)
                        AND (SELECT count(*) = 2 AND bool_and(user_id = 'worker_fault_user'
                             AND stripe_subscription_id = 'sub_worker_fault' AND plan_id = 'plus'
                             AND amount_cents = 800 AND currency = 'usd')
                             AND array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                 = ARRAY['in_worker_first', 'in_worker_second']
                             FROM public.paid_invoice_history)))",
                &[&worker, &writer, &commit],
            ).await.map_err(|_| StoreError)?.get::<_, Option<bool>>(0).unwrap_or(false);
            if !valid || after.0 != before.0 || (!commit && (after.1 != inserted || after.2.is_some())) {
                return Err(StoreError);
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

pub(crate) async fn test_reconcile_raw_replace(commit: bool) -> Result<(), &'static str> {
    use futures_util::future::{select, Either};
    use std::{cell::Cell, rc::Rc};

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-raw-replace-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let user = "worker_fault_user";
    store
        .store_reconcile_owned(user.into(), test_outcome(), ReconcileProbe::None)
        .await
        .map_err(|_| "raw replacement: seed through dormant store failed")?;
    let mut outcome = test_outcome();
    outcome.explicit_stale = true;
    outcome.history.truncate(1);
    outcome.history[0].stripe_invoice_id = "in_worker_raw_replace".into();

    let phase = Rc::new(Cell::new("raw replacement: seed second owner"));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |writer| Box::pin(async move {
            writer.batch_execute(
                "INSERT INTO public.users (id, email, created_at)
                 VALUES ('worker_replacement_user', 'worker-replacement@example.test', 1786032000); BEGIN",
            ).await.map_err(|_| StoreError)?;
            let original: String = writer.query_one(
                "SELECT to_jsonb(s)::text FROM public.subscriptions s", &[],
            ).await.map_err(|_| StoreError)?.get(0);
            let history_sql = "SELECT jsonb_agg(to_jsonb(h) ORDER BY stripe_invoice_id)::text
                               FROM public.paid_invoice_history h
                               WHERE stripe_invoice_id <> 'in_worker_raw_replace'";
            let history: String = writer.query_one(history_sql, &[]).await.map_err(|_| StoreError)?.get(0);

            phase.set("raw replacement: delete and reinsert the global key");
            if writer.execute(
                "DELETE FROM public.subscriptions
                 WHERE user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'", &[],
            ).await.map_err(|_| StoreError)? != 1 {
                return Err(StoreError);
            }
            let replacement: String = writer.query_one(
                "INSERT INTO public.subscriptions AS s
                    (user_id, stripe_subscription_id, stripe_customer_id,
                     stripe_subscription_item_id, stripe_price_id, plan_id, status)
                 VALUES ('worker_replacement_user', 'sub_worker_fault', 'cus_worker_replacement',
                         'si_worker_replacement', 'price_worker_fault', 'plus', 'canceled')
                 RETURNING to_jsonb(s)::text", &[],
            ).await.map_err(|_| StoreError)?.get(0);

            phase.set("raw replacement: Worker table lock did not wait on the writer");
            let mut reconcile = Box::pin(store.store_reconcile_owned(user.into(), outcome, ReconcileProbe::None));
            match select(reconcile.as_mut(), Box::pin(wait_for_raw_writer(writer))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((observed, _)) => observed?,
            }
            phase.set("raw replacement: preceding writer did not finish");
            writer.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" }).await.map_err(|_| StoreError)?;
            phase.set("raw replacement: wrong reconciliation result after writer finished");
            if reconcile.await.is_ok() == commit {
                return Err(StoreError);
            }

            phase.set("raw replacement: durable subscription or history differed");
            let observed: String = writer.query_one(
                "SELECT to_jsonb(s)::text FROM public.subscriptions s", &[],
            ).await.map_err(|_| StoreError)?.get(0);
            let observed_history: String = writer.query_one(history_sql, &[]).await.map_err(|_| StoreError)?.get(0);
            let invoice = writer.query_opt(
                "SELECT user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'
                        AND plan_id = 'plus' AND amount_cents = 800 AND currency = 'usd'
                 FROM public.paid_invoice_history WHERE stripe_invoice_id = 'in_worker_raw_replace'", &[],
            ).await.map_err(|_| StoreError)?;
            if observed != if commit { replacement } else { original }
                || observed_history != history
                || invoice.map(|row| row.get::<_, bool>(0)) != if commit { None } else { Some(true) }
            {
                return Err(StoreError);
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

// An empty stale outcome takes exclusion without acquiring a history FK lock.
// This isolates the cascade's child-table wait from the separate parent cycle.
pub(crate) async fn test_reconcile_before_parent_delete(commit: bool) -> Result<(), &'static str> {
    use crate::billing_transaction_sql::PRECOMMIT_GATE;
    use futures_util::future::{join, select, Either};
    use std::{cell::Cell, rc::Rc};

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-before-parent-delete-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let user = "worker_fault_user";
    let mut outcome = test_outcome();
    store
        .store_reconcile_owned(user.into(), outcome.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "parent delete: seed through dormant store failed")?;
    outcome.explicit_stale = true;
    outcome.history.clear();

    let phase = Rc::new(Cell::new("parent delete: connect independent observer"));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |db| Box::pin(async move {
            phase.set("parent delete: snapshot and hold the Worker before COMMIT");
            let before = parent_delete_snapshot(db, None).await?;
            if before.0.is_none() || before.1.is_none() || before.2.is_none() {
                return Err(StoreError);
            }
            // Keep samples in autocommit so pg_stat_activity is refreshed.
            db.query_one("SELECT pg_advisory_lock($1)", &[&PRECOMMIT_GATE])
                .await.map_err(|_| StoreError)?;
            let mut reconcile = Box::pin(store.store_reconcile_owned(user.into(), outcome, ReconcileProbe::BeforeCommit));
            let worker = match select(reconcile.as_mut(), Box::pin(wait_lock_cycle(|| async {
                let row = db.query_opt(
                    "SELECT l.pid FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_stat_activity a USING (pid)
                     WHERE l.relation = 'public.subscriptions'::regclass AND l.granted
                       AND l.mode = 'ShareRowExclusiveLock'
                       AND a.application_name = 'owned_billing_reconcile_test'
                       AND a.wait_event_type = 'Lock' AND a.wait_event = 'advisory'
                       AND a.query = 'SELECT pg_advisory_xact_lock($1)'
                       AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid))", &[],
                ).await.map_err(|_| StoreError)?;
                Ok(row.map(|row| row.get::<_, i32>(0)))
            }))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((worker, _)) => worker?,
            };

            let deleter_pid = Rc::new(Cell::new(None::<i32>));
            let record_pid = deleter_pid.clone();
            let mut delete = Box::pin(billing_budget::with_owned_connection(
                "127.0.0.1", 5432, CONNECTION,
                |_| Box::pin(async { Ok(()) }),
                move |raw| Box::pin(async move {
                    raw.batch_execute("BEGIN; SET LOCAL statement_timeout = '5s'")
                        .await.map_err(|_| StoreError)?;
                    record_pid.set(Some(raw.query_one("SELECT pg_backend_pid()", &[])
                        .await.map_err(|_| StoreError)?.get(0)));
                    if raw.execute("DELETE FROM public.users WHERE id = $1", &[&user])
                        .await.map_err(|_| StoreError)? != 1
                    {
                        return Err(StoreError);
                    }
                    raw.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
                        .await.map_err(|_| StoreError)
                }),
                |_| {},
            ));
            phase.set("parent delete: cascade did not wait on the Worker's subscription exclusion");
            let deleter = match select(delete.as_mut(), Box::pin(wait_lock_cycle(|| async {
                let Some(pid) = deleter_pid.get() else { return Ok(None); };
                let waiting: bool = db.query_one(
                    "SELECT $2 = ANY(pg_blocking_pids($1)) AND EXISTS (
                        SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1
                          AND relation = 'public.subscriptions'::regclass
                          AND mode = 'RowExclusiveLock' AND NOT granted)", &[&pid, &worker],
                ).await.map_err(|_| StoreError)?.get(0);
                Ok(waiting.then_some(pid))
            }))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((deleter, _)) => deleter?,
            };

            phase.set("parent delete: plain read did not preserve the committed snapshot while waiting");
            if parent_delete_snapshot(db, None).await? != before {
                return Err(StoreError);
            }
            let released: bool = db.query_one("SELECT pg_advisory_unlock($1)", &[&PRECOMMIT_GATE])
                .await.map_err(|_| StoreError)?.get(0);
            if !released {
                return Err(StoreError);
            }
            phase.set("parent delete: Worker or raw delete failed after release");
            let (reconciled, deleted) = join(reconcile, delete).await;
            reconciled?;
            deleted?;
            phase.set("parent delete: partial cascade, changed rollback snapshot or retained lock");
            let after = parent_delete_snapshot(db, None).await?;
            let locked: bool = db.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid IN ($1, $2)
                   AND relation IN ('public.users'::regclass, 'public.subscriptions'::regclass,
                                    'public.paid_invoice_history'::regclass))", &[&worker, &deleter],
            ).await.map_err(|_| StoreError)?.get(0);
            if locked || after != if commit { (None, None, None) } else { before } {
                return Err(StoreError);
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

async fn parent_delete_snapshot(
    db: &tokio_postgres::Client,
    omit_invoice: Option<&str>,
) -> Result<(Option<String>, Option<String>, Option<String>), StoreError> {
    let row = db.query_one(
        "SELECT (SELECT jsonb_agg(to_jsonb(u) ORDER BY id)::text FROM public.users u),
                (SELECT jsonb_agg(to_jsonb(s) ORDER BY stripe_subscription_id)::text FROM public.subscriptions s),
                (SELECT jsonb_agg(to_jsonb(h) ORDER BY stripe_invoice_id)::text
                   FROM public.paid_invoice_history h WHERE stripe_invoice_id IS DISTINCT FROM $1::text)",
        &[&omit_invoice],
    ).await.map_err(|_| StoreError)?;
    let snapshot = || Ok((row.try_get(0)?, row.try_get(1)?, row.try_get(2)?));
    snapshot().map_err(|_: tokio_postgres::Error| StoreError)
}

// A preceding parent DELETE already holds the subscription table via its FK cascade.
// The same nonempty stale outcome must classify differently once that writer finishes.
pub(crate) async fn test_reconcile_after_parent_delete(commit: bool) -> Result<(), &'static str> {
    use futures_util::future::{select, Either};
    use std::{cell::Cell, rc::Rc};

    setup_ack_fixture().await?;
    let store = PgStore {
        env: None,
        trace_id: "worker-after-parent-delete-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let user = "worker_fault_user";
    store
        .store_reconcile_owned(user.into(), test_outcome(), ReconcileProbe::None)
        .await
        .map_err(|_| "parent-first delete: seed through dormant store failed")?;
    let mut outcome = test_outcome();
    let invoice = "in_worker_parent_first";
    outcome.explicit_stale = true;
    outcome.history.truncate(1);
    outcome.history[0].stripe_invoice_id = invoice.into();
    let mut empty = outcome.clone();
    empty.history.clear();

    let phase = Rc::new(Cell::new("parent-first delete: connect raw writer"));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |writer| Box::pin(async move {
            phase.set("parent-first delete: parent, subscription or original history not seeded");
            let before = parent_delete_snapshot(writer, None).await?;
            if before.0.is_none() || before.1.is_none() || before.2.is_none() {
                return Err(StoreError);
            }
            phase.set("parent-first delete: raw transaction did not cascade");
            writer.batch_execute("BEGIN").await.map_err(|_| StoreError)?;
            if writer.execute("DELETE FROM public.users WHERE id = $1", &[&user])
                .await.map_err(|_| StoreError)? != 1
                || parent_delete_snapshot(writer, None).await? != (None, None, None)
            {
                return Err(StoreError);
            }
            let stale_refused = Rc::new(Cell::new(false));
            let probe = ReconcileProbe::ObserveStaleRefusal(stale_refused.clone());
            let mut reconcile = Box::pin(store.store_reconcile_owned(user.into(), outcome, probe));
            phase.set("parent-first delete: Worker table lock did not wait on the exact writer");
            let worker = match select(reconcile.as_mut(), Box::pin(wait_lock_cycle(|| async {
                let row = writer.query_opt(
                    "SELECT l.pid FROM pg_catalog.pg_locks l
                     WHERE l.relation = 'public.subscriptions'::regclass AND NOT l.granted
                       AND l.mode = 'ShareRowExclusiveLock'
                       AND pg_backend_pid() = ANY(pg_blocking_pids(l.pid))", &[],
                ).await.map_err(|_| StoreError)?;
                Ok(row.map(|row| row.get::<_, i32>(0)))
            }))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((worker, _)) => worker?,
            };
            phase.set("parent-first delete: preceding writer did not finish");
            writer.batch_execute(if commit { "COMMIT" } else { "ROLLBACK" })
                .await.map_err(|_| StoreError)?;
            phase.set("parent-first delete: wrong result for nonempty stale outcome");
            if reconcile.await.is_ok() == commit {
                return Err(StoreError);
            }
            phase.set("parent-first delete: wrong classifier refusal for nonempty stale outcome");
            if stale_refused.get() != commit {
                return Err(StoreError);
            }

            phase.set("parent-first delete: Worker retained billing locks");
            let locked: bool = writer.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1
                   AND relation IN ('public.users'::regclass, 'public.subscriptions'::regclass,
                                    'public.paid_invoice_history'::regclass))", &[&worker],
            ).await.map_err(|_| StoreError)?.get(0);
            if locked {
                return Err(StoreError);
            }
            if commit {
                phase.set("parent-first delete: committed cascade or absent/empty no-op changed durable rows");
                if parent_delete_snapshot(writer, None).await? != (None, None, None) {
                    return Err(StoreError);
                }
                store.store_reconcile_owned(user.into(), empty, ReconcileProbe::None).await?;
                if parent_delete_snapshot(writer, None).await? != (None, None, None) {
                    return Err(StoreError);
                }
            } else {
                phase.set("parent-first delete: restored rows or new invoice attribution differed");
                let new_invoice = writer.query_opt(
                    "SELECT user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'
                            AND plan_id = 'plus' AND amount_cents = 800 AND currency = 'usd'
                            AND period_start = to_timestamp(1786032000)
                            AND period_end = to_timestamp(1786032100)
                            AND paid_at = to_timestamp(1786032000)
                     FROM public.paid_invoice_history WHERE stripe_invoice_id = $1", &[&invoice],
                ).await.map_err(|_| StoreError)?;
                if parent_delete_snapshot(writer, Some(invoice)).await? != before
                    || new_invoice.map(|row| row.get::<_, bool>(0)) != Some(true)
                {
                    return Err(StoreError);
                }
            }
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

// The raw transaction first holds a row lock that is compatible with table
// exclusion. Its later write must wait for the Worker, which waits on that row.
pub(crate) async fn test_reconcile_lock_cycle(row_update: bool) -> Result<(), &'static str> {
    use futures_util::future::{join, select, Either};
    use std::{cell::Cell, rc::Rc};
    use tokio_postgres::error::SqlState;

    let (lock_row, worker_statement, raw_write) = if row_update {
        (
            "SELECT pg_backend_pid() FROM public.subscriptions
             WHERE user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault' FOR UPDATE",
            "INSERT INTO public.subscriptions %",
            "UPDATE public.subscriptions SET stripe_price_id = 'price_raw_cycle'
             WHERE user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'",
        )
    } else {
        (
            "SELECT pg_backend_pid() FROM public.users WHERE id = 'worker_fault_user' FOR UPDATE",
            "INSERT INTO public.paid_invoice_history %",
            "DELETE FROM public.users WHERE id = 'worker_fault_user'",
        )
    };
    setup_ack_fixture().await?;
    let user = "worker_fault_user";
    let store = PgStore {
        env: None,
        trace_id: "worker-lock-cycle-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let mut outcome = test_outcome();
    store
        .store_reconcile_owned(user.into(), outcome.clone(), ReconcileProbe::None)
        .await
        .map_err(|_| "lock cycle: seed through dormant store failed")?;
    outcome.read_plan = Some(outcome.row.plan_id.clone());
    outcome.read_anchor = outcome.row.paid_invoice_id.clone();
    outcome.row.cancel_at_period_end = true;
    outcome.history.truncate(1);
    outcome.history[0].stripe_invoice_id = if row_update {
        "in_worker_row_update_cycle"
    } else {
        "in_worker_parent_cycle"
    }
    .into();
    // Distinct values catch a period start / payment timestamp swap.
    outcome.history[0].paid_at_unix += 10;
    let expected_history = outcome.history[0].clone();

    let phase = Rc::new(Cell::new("lock cycle: connect disposable writer"));
    let diagnostic = phase.clone();
    let writer_pid = Rc::new(Cell::new(None::<i32>));
    let worker_pid = Rc::new(Cell::new(None::<i32>));
    let writer = writer_pid.clone();
    let worker = worker_pid.clone();
    let observe = billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |db| {
            Box::pin(async move {
                let writer = wait_lock_cycle(|| async { Ok(writer.get()) }).await?;
                let pid = wait_lock_cycle(|| async {
                    let row = db
                        .query_opt(
                            "SELECT l.pid FROM pg_catalog.pg_locks l
                     JOIN pg_catalog.pg_stat_activity a USING (pid)
                     WHERE l.relation = 'public.subscriptions'::regclass
                       AND l.mode = 'ShareRowExclusiveLock' AND l.granted
                       AND a.application_name = 'owned_billing_reconcile_test'
                       AND a.query LIKE $2 AND a.wait_event_type = 'Lock'
                       AND $1 = ANY(pg_blocking_pids(l.pid))",
                            &[&writer, &worker_statement],
                        )
                        .await
                        .map_err(|_| StoreError)?;
                    Ok(row.map(|r| r.get::<_, i32>(0)))
                })
                .await?;
                worker.set(Some(pid));
                wait_lock_cycle(|| async {
                    let cycle: bool = db.query_one(
                    "SELECT $2 = ANY(pg_blocking_pids($1)) AND $1 = ANY(pg_blocking_pids($2))
                       AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid = $1
                         AND relation = 'public.subscriptions'::regclass
                         AND mode = 'RowExclusiveLock' AND NOT granted)", &[&writer, &pid],
                ).await.map_err(|_| StoreError)?.get(0);
                    Ok(cycle.then_some(()))
                })
                .await
            })
        },
        |_| {},
    );

    let run = billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |raw| {
            Box::pin(async move {
                phase.set("lock cycle: snapshot and lock fixture row");
                let before = lock_cycle_snapshot(raw, &expected_history).await?;
                let deadlocks_before = lock_cycle_deadlocks(raw).await?;
                raw.batch_execute("BEGIN; SET LOCAL statement_timeout = '5s'")
                    .await
                    .map_err(|_| StoreError)?;
                let writer: i32 = raw
                    .query_one(lock_row, &[])
                    .await
                    .map_err(|_| StoreError)?
                    .get(0);
                writer_pid.set(Some(writer));
                phase
                    .set("lock cycle: Worker statement did not wait on the raw writer while holding exclusion");
                let mut reconcile = Box::pin(store.store_reconcile_owned(
                    user.into(),
                    outcome,
                    ReconcileProbe::None,
                ));
                let worker = match select(
                    reconcile.as_mut(),
                    Box::pin(wait_lock_cycle(|| async { Ok(worker_pid.get()) })),
                )
                .await
                {
                    Either::Right((observed, _)) => observed?,
                    Either::Left(_) => return Err(StoreError),
                };
                phase.set("lock cycle: expected exactly one writer to abort");
                let raw_attempt = async {
                    let written = raw.execute(raw_write, &[]).await;
                    // Never persist the raw write, even when the Worker is the victim.
                    raw.batch_execute("ROLLBACK")
                        .await
                        .map_err(|_| StoreError)?;
                    match written {
                        Ok(1) => Ok(true),
                        Err(e) if e.code() == Some(&SqlState::T_R_DEADLOCK_DETECTED) => Ok(false),
                        _ => Err(StoreError),
                    }
                };
                let (written, reconciled) = join(raw_attempt, reconcile).await;
                let committed = reconciled.is_ok();
                if written? == committed {
                    return Err(StoreError);
                }
                // The hosted smoke gives this pair a fresh database. With both
                // waits observed, a successful raw write and one server-counted
                // deadlock identify the Worker as the victim, not a lock timeout.
                phase.set("lock cycle: PostgreSQL did not report exactly one deadlock");
                let deadlocks_after = wait_lock_cycle(|| async {
                    let count = lock_cycle_deadlocks(raw).await?;
                    Ok((count != deadlocks_before).then_some(count))
                })
                .await?;
                if deadlocks_after != deadlocks_before + 1 {
                    return Err(StoreError);
                }
                phase.set("lock cycle: partial write, changed history or retained lock");
                let after = lock_cycle_snapshot(raw, &expected_history).await?;
                let locks: bool = raw.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid IN ($1, $2) AND relation = 'public.subscriptions'::regclass)",
                &[&writer, &worker],
            ).await.map_err(|_| StoreError)?.get(0);
                if before.0 != after.0
                    || before.1 != after.1
                    || before.2
                    || before.3.is_some()
                    || after.2 != committed
                    || after.3 != committed.then_some(true)
                    || locks
                {
                    return Err(StoreError);
                }
                Ok(())
            })
        },
        |_| {},
    );
    let (observed, ran) = join(observe, run).await;
    ran.map_err(|_| diagnostic.get())?;
    observed.map_err(|_| {
        "lock cycle: raw write did not wait on Worker exclusion while the Worker statement waited"
    })
}

async fn wait_lock_cycle<T, F: std::future::Future<Output = Result<Option<T>, StoreError>>>(
    mut sample: impl FnMut() -> F,
) -> Result<T, StoreError> {
    for _ in 0..80 {
        if let Some(value) = sample().await? {
            return Ok(value);
        }
        worker::Delay::from(std::time::Duration::from_millis(25)).await;
    }
    Err(StoreError)
}

async fn lock_cycle_deadlocks(db: &tokio_postgres::Client) -> Result<i64, StoreError> {
    db.query_one(
        "SELECT deadlocks FROM pg_catalog.pg_stat_database WHERE datname = current_database()",
        &[],
    )
    .await
    .and_then(|row| row.try_get(0))
    .map_err(|_| StoreError)
}

async fn lock_cycle_snapshot(
    db: &tokio_postgres::Client,
    expected_history: &PaidInvoiceRow,
) -> Result<(String, String, bool, Option<bool>), StoreError> {
    let row = db.query_one(
        "SELECT (SELECT jsonb_agg(to_jsonb(s) - 'cancel_at_period_end' ORDER BY stripe_subscription_id)::text
                  FROM public.subscriptions s),
                (SELECT jsonb_agg(to_jsonb(h) ORDER BY stripe_invoice_id)::text FROM public.paid_invoice_history h
                  WHERE stripe_invoice_id <> $4),
                (SELECT cancel_at_period_end FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'),
                (SELECT user_id = 'worker_fault_user' AND stripe_subscription_id = 'sub_worker_fault'
                     AND plan_id = 'plus' AND amount_cents = 800 AND currency = 'usd'
                     AND period_start = to_timestamp($1::bigint)
                     AND period_end = to_timestamp($2::bigint)
                     AND paid_at = to_timestamp($3::bigint)
                  FROM public.paid_invoice_history WHERE stripe_invoice_id = $4)",
        &[
            &expected_history.period_start_unix,
            &expected_history.period_end_unix,
            &expected_history.paid_at_unix,
            &expected_history.stripe_invoice_id,
        ],
    ).await.map_err(|_| StoreError)?;
    (|| {
        Ok((
            row.try_get(0)?,
            row.try_get(1)?,
            row.try_get(2)?,
            row.try_get(3)?,
        ))
    })()
    .map_err(|_: tokio_postgres::Error| StoreError)
}

// Exercise history refusal, late rollback and replay through the shared store.
async fn late_fault_and_retry() -> Result<(), &'static str> {
    let store = PgStore {
        env: None,
        trace_id: "worker-fault-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: None,
        cancel_token_observer: None,
    };
    let mut absent = test_outcome();
    absent.explicit_stale = true;
    absent.row.paid_invoice_id = None;
    absent.history.truncate(1);
    absent.history[0].stripe_invoice_id = "in_worker_absent".into();
    for _ in 0..2 {
        if store
            .store_reconcile_outcome("worker_fault_user", &absent)
            .await
            .is_ok()
        {
            return Err("explicit stale accepted history for an absent subscription");
        }
        fixture(false)
            .await
            .map_err(|_| "absent subscription refusal changed committed state")?;
    }
    let mut empty = absent.clone();
    empty.history.clear();
    store
        .store_reconcile_outcome("worker_fault_user", &empty)
        .await
        .map_err(|_| "explicit stale with absent subscription and empty history failed")?;
    fixture(false)
        .await
        .map_err(|_| "empty absent subscription no-op changed committed state")?;

    let mut outcome = test_outcome();
    outcome.history[1].currency = "eur".into();
    outcome.history[1].amount_cents = -1;
    if store
        .store_reconcile_outcome("worker_fault_user", &outcome)
        .await
        .is_ok()
    {
        return Err("late history error was not returned");
    }
    let (reached, empty) = late_fault_state()
        .await
        .map_err(|_| "late history evidence query failed")?;
    if !reached {
        return Err("invalid second insert was not reached after both earlier writes");
    }
    if !empty {
        return Err("late history error did not roll back");
    }
    outcome.history[1].amount_cents = 800;
    store
        .store_reconcile_outcome("worker_fault_user", &outcome)
        .await
        .map_err(|_| "corrected retry failed")?;
    let before = fault_snapshot()
        .await
        .map_err(|_| "corrected retry state invalid")?;
    before.carry_read_provenance(&mut outcome);
    store
        .store_reconcile_outcome("worker_fault_user", &outcome)
        .await
        .map_err(|_| "identical replay failed")?;
    let after = fault_snapshot().await.map_err(|_| "replay state invalid")?;
    if before != after {
        return Err("replay changed committed state");
    }
    outcome.history[0].stripe_invoice_id = "in_worker_fresh_first".into();
    outcome.history[1].stripe_invoice_id = "in_worker_fresh_second".into();
    outcome.history[1].stripe_subscription_id = "sub_worker_other".into();
    for explicit_stale in [false, true] {
        outcome.explicit_stale = explicit_stale;
        if store
            .store_reconcile_outcome("worker_fault_user", &outcome)
            .await
            .is_ok()
        {
            return Err("history for a different subscription was accepted");
        }
        if fault_snapshot()
            .await
            .map_err(|_| "history attribution refusal changed committed state")?
            != before
        {
            return Err("history attribution refusal changed committed state");
        }
        transaction_refuses_misattributed_history(outcome.clone()).await?;
        if fault_snapshot()
            .await
            .map_err(|_| "transaction attribution refusal changed committed state")?
            != before
        {
            return Err("transaction attribution refusal changed committed state");
        }
    }
    // The exact failed outcome can append history once the target is owned.
    store
        .store_reconcile_outcome("worker_fault_user", &absent)
        .await
        .map_err(|_| "owned target did not recover the refused stale history")?;
    let recovered = fault_snapshot_with_recovery(true)
        .await
        .map_err(|_| "row-6 recovery did not append the expected history")?;
    if recovered.subscription != before.subscription {
        return Err("row-6 recovery changed the existing subscription");
    }
    if recovered.existing_history != before.existing_history {
        return Err("row-6 recovery changed existing invoice history");
    }
    store
        .store_reconcile_outcome("worker_fault_user", &absent)
        .await
        .map_err(|_| "row-6 recovery replay failed")?;
    if fault_snapshot_with_recovery(true)
        .await
        .map_err(|_| "row-6 recovery replay state invalid")?
        != recovered
    {
        return Err("row-6 recovery replay changed committed state");
    }
    Ok(())
}

async fn transaction_refuses_misattributed_history(
    outcome: ReconcileOutcome,
) -> Result<(), &'static str> {
    use crate::billing_transaction_sql::apply_reconcile_transaction;

    let (accepted, transaction_ended) = billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |client| Box::pin(billing_budget::bootstrap_transaction(client)),
        move |client| {
            Box::pin(async move {
                // Assign an XID so we can distinguish explicit rollback from
                // cleanup on disconnect. Do not route through the store guard.
                client
                    .batch_execute("SELECT pg_current_xact_id()")
                    .await
                    .map_err(|_| StoreError)?;
                let accepted = apply_reconcile_transaction(
                    client,
                    "worker_fault_user".into(),
                    outcome,
                    ReconcileProbe::None,
                )
                .await
                .is_ok();
                let row = client
                    .query_one("SELECT pg_current_xact_id_if_assigned() IS NULL", &[])
                    .await
                    .map_err(|_| StoreError)?;
                Ok((accepted, row.get::<_, bool>(0)))
            })
        },
        |_| {},
    )
    .await
    .map_err(|_| "transaction attribution control setup or rollback query failed")?;
    if accepted {
        return Err("transaction accepted history for a different subscription");
    }
    if !transaction_ended {
        return Err("transaction attribution refusal did not roll back");
    }
    Ok(())
}

async fn late_fault_state() -> Result<(bool, bool), StoreError> {
    billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        |client| {
            Box::pin(async move {
                let row = client
                    .query_one(
                        "SELECT is_called,
                                NOT EXISTS (SELECT 1 FROM public.subscriptions)
                                AND NOT EXISTS (SELECT 1 FROM public.paid_invoice_history)
                         FROM public.worker_billing_late_insert",
                        &[],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                Ok((row.get(0), row.get(1)))
            })
        },
        |_| {},
    )
    .await
}

#[derive(PartialEq, Eq)]
struct FaultSnapshot {
    subscription: String,
    existing_history: String,
    recovered_history: Option<String>,
    plan: String,
    anchor: Option<String>,
}

impl FaultSnapshot {
    fn carry_read_provenance(&self, outcome: &mut ReconcileOutcome) {
        outcome.read_plan = Some(self.plan.clone());
        outcome.read_anchor = self.anchor.clone();
    }
}

async fn fault_snapshot() -> Result<FaultSnapshot, StoreError> {
    fault_snapshot_with_recovery(false).await
}

async fn fault_snapshot_with_recovery(recovered: bool) -> Result<FaultSnapshot, StoreError> {
    billing_budget::with_owned_connection(
        "127.0.0.1",
        5432,
        CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| {
            Box::pin(async move {
                let row = client
                    .query_one(
                        "SELECT (SELECT count(*) = 1 AND bool_and(user_id = 'worker_fault_user'
                                                AND stripe_subscription_id = 'sub_worker_fault'
                                                AND paid_invoice_id = 'in_worker_second')
                                 FROM public.subscriptions),
                                (SELECT bool_and(user_id = 'worker_fault_user'
                                                AND stripe_subscription_id = 'sub_worker_fault'
                                                AND amount_cents = 800)
                                      AND array_agg(stripe_invoice_id ORDER BY stripe_invoice_id)
                                          = CASE WHEN $1 THEN ARRAY['in_worker_absent', 'in_worker_first', 'in_worker_second']
                                                 ELSE ARRAY['in_worker_first', 'in_worker_second'] END
                                      AND array_agg(currency ORDER BY stripe_invoice_id)
                                          = CASE WHEN $1 THEN ARRAY['usd', 'usd', 'eur']
                                                 ELSE ARRAY['usd', 'eur'] END
                                 FROM public.paid_invoice_history),
                                (SELECT jsonb_agg(to_jsonb(s) ORDER BY stripe_subscription_id)::text
                                 FROM public.subscriptions s),
                                (SELECT jsonb_agg(to_jsonb(h) ORDER BY stripe_invoice_id)::text
                                 FROM public.paid_invoice_history h
                                 WHERE stripe_invoice_id <> 'in_worker_absent'),
                                (SELECT to_jsonb(h)::text FROM public.paid_invoice_history h
                                 WHERE stripe_invoice_id = 'in_worker_absent'),
                                (SELECT plan_id FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'),
                                (SELECT paid_invoice_id FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault')",
                        &[&recovered],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if row.get::<_, Option<bool>>(0) != Some(true)
                    || row.get::<_, Option<bool>>(1) != Some(true)
                {
                    return Err(StoreError);
                }
                Ok(FaultSnapshot {
                    subscription: row.get(2),
                    existing_history: row.get(3),
                    recovered_history: row.get(4),
                    plan: row.get(5),
                    anchor: row.get(6),
                })
            })
        },
        |_| {},
    )
    .await
}

#[path = "billing_exact_target_test.rs"]
mod exact_target;

pub(crate) async fn test_reconcile_deadline() -> Result<(), &'static str> {
    fixture(true)
        .await
        .map_err(|_| "billing fixture setup failed")?;
    let outcome = test_outcome();
    expire_after_writes(&outcome, ReconcileProbe::WorkerDeadline)
        .await
        .map_err(|_| "billing deadline after writes control failed")?;
    expire_after_writes(&outcome, ReconcileProbe::ServerWatchdog)
        .await
        .map_err(|_| "billing server watchdog did not clean up abandoned writes")?;
    late_fault_and_retry().await?;
    exact_target::check().await
}

async fn cancel_in_flight(outcome: ReconcileOutcome) -> Result<(), &'static str> {
    use futures_util::future::{select, Either};
    use std::time::Duration;

    let phase = std::rc::Rc::new(std::cell::Cell::new(
        "cancellation observer connection failed",
    ));
    let diagnostic = phase.clone();
    billing_budget::with_owned_connection(
        "127.0.0.1", 5432, CONNECTION,
        |_| Box::pin(async { Ok(()) }),
        move |client| Box::pin(async move {
            phase.set("cancellation observer lock failed");
            client.batch_execute("SELECT pg_advisory_lock(781349052)").await.map_err(|_| StoreError)?;
            let cancel_token = std::rc::Rc::new(std::cell::RefCell::new(None));
            let store = PgStore {
                env: None,
                trace_id: "worker-cancel-test".into(),
                mirror_binding: MirrorBinding::Unconfigured,
                work_deadline_observer: None,
                cancel_token_observer: Some(cancel_token.clone()),
            };
            super::billing_cancel_control::within_blocked_window(|| async {
            let mut operation = Box::pin(store.store_reconcile_outcome("worker_fault_user", &outcome));
            let mut waiting_pid = None;
            phase.set("cancellation wait observation failed");
            for _ in 0..40 {
                let observation = Box::pin(async {
                    let rows = client.query(
                        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = 'owned_billing_reconcile_test' AND wait_event_type = 'Lock' AND wait_event = 'advisory' AND pg_backend_pid() = ANY(pg_blocking_pids(pid))", &[]
                    ).await.map_err(|_| StoreError)?;
                    if rows.len() > 1 { return Err(StoreError); }
                    let pid = rows.first().map(|row| row.get::<_, i32>(0));
                    if pid.is_none() { worker::Delay::from(Duration::from_millis(50)).await; }
                    Ok(pid)
                });
                match select(operation.as_mut(), observation).await {
                    Either::Left((result, _)) => {
                        phase.set(if result.is_ok() { "cancellation operation completed early" } else { "cancellation operation failed early" });
                        return Err(StoreError);
                    }
                    Either::Right((result, _)) => {
                        if let Some(pid) = result? { waiting_pid = Some(pid); break; }
                    }
                }
            }
            phase.set("cancellation advisory wait not observed");
            let pid = waiting_pid.ok_or(StoreError)?;
            match select(operation.as_mut(), Box::pin(super::billing_cancel_control::observe_no_cancel(client, pid))).await {
                Either::Left(_) => return Err(StoreError),
                Either::Right((result, _)) => result.map_err(|_| StoreError)?,
            }
            let token = cancel_token.borrow_mut().take().ok_or(StoreError)?;
            drop(operation);
            super::billing_cancel_control::send_cancel(&token).await.map_err(|_| StoreError)?;
            let mut disappeared = false;
            let mut last_state = None;
            for _ in 0..40 {
                phase.set("cancellation backend observation query failed or timed out");
                let row = client.query_opt(
                    "SELECT state, wait_event_type, wait_event, EXTRACT(EPOCH FROM (clock_timestamp() - xact_start))::bigint FROM pg_catalog.pg_stat_activity WHERE pid = $1", &[&pid]
                ).await.map_err(|_| StoreError)?;
                let Some(row) = row else { disappeared = true; break; };
                last_state = Some((row.get::<_, Option<String>>(0), row.get::<_, Option<String>>(1), row.get::<_, Option<String>>(2), row.get::<_, Option<i64>>(3)));
                phase.set("cancellation backend remained present during observation");
                worker::Delay::from(Duration::from_millis(50)).await;
            }
            if !disappeared {
                phase.set(match last_state {
                    Some((Some(state), Some(kind), Some(event), _))
                        if state == "active" && kind == "Lock" && event == "advisory" =>
                    {
                        "cancellation backend remained blocked on advisory lock"
                    }
                    Some((Some(state), _, _, _)) if state == "idle in transaction" => {
                        "cancellation backend remained idle in transaction"
                    }
                    _ => "cancellation backend remained present in another state",
                });
                return Err(StoreError);
            }
            phase.set("cancellation rollback row check failed");
            let row = client.query_one("SELECT (SELECT count(*) FROM public.subscriptions WHERE stripe_subscription_id = 'sub_worker_fault'), (SELECT count(*) FROM public.paid_invoice_history WHERE stripe_subscription_id = 'sub_worker_fault')", &[]).await.map_err(|_| StoreError)?;
            if row.get::<_, i64>(0) != 0 || row.get::<_, i64>(1) != 0 { return Err(StoreError); }
            Ok(())
            }, StoreError).await?;
            phase.set("cancellation observer unlock failed");
            client.batch_execute("SELECT pg_advisory_unlock(781349052)").await.map_err(|_| StoreError)?;
            Ok(())
        }),
        |_| {},
    ).await.map_err(|_| diagnostic.get())
}

pub(crate) async fn test_reconcile_cancel() -> Result<(), &'static str> {
    fixture(true)
        .await
        .map_err(|_| "billing fixture setup failed")?;
    let mut outcome = test_outcome();
    outcome.history[1].stripe_invoice_id = "in_worker_cancel".into();
    outcome.row.paid_invoice_id = Some("in_worker_cancel".into());
    cancel_in_flight(outcome).await
}

pub(crate) async fn setup_ack_fixture() -> Result<(), &'static str> {
    fixture(true)
        .await
        .map_err(|_| "billing fixture setup failed")
}

// The relay withholds a successful COMMIT response from the dormant store.
// Retry the same invoice delivery using an exact read of the committed row.
pub(crate) async fn test_reconcile_ack_retry() -> Result<(), &'static str> {
    let expired = std::rc::Rc::new(std::cell::Cell::new(false));
    let store = PgStore {
        env: None,
        trace_id: "worker-commit-ack-loss-test".into(),
        mirror_binding: MirrorBinding::Unconfigured,
        work_deadline_observer: Some(expired.clone()),
        cancel_token_observer: None,
    };
    let mut outcome = test_outcome();
    outcome.history[1].currency = "eur".into();
    if store
        .store_reconcile_outcome("worker_fault_user", &outcome)
        .await
        .is_ok()
    {
        return Err("COMMIT acknowledgement loss unexpectedly succeeded");
    }
    if expired.get() {
        return Err("COMMIT acknowledgement loss was masked by the Worker work deadline");
    }
    let committed = fault_snapshot()
        .await
        .map_err(|_| "COMMIT acknowledgement loss stored incorrect state")?;
    committed.carry_read_provenance(&mut outcome);
    let retry = PgStore {
        trace_id: "worker-fault-test".into(),
        work_deadline_observer: None,
        ..store
    };
    for step in ["billing retry failed", "billing replay failed"] {
        retry
            .store_reconcile_outcome("worker_fault_user", &outcome)
            .await
            .map_err(|_| step)?;
        if fault_snapshot().await.map_err(|_| step)? != committed {
            return Err("billing retry or replay changed committed state");
        }
    }
    Ok(())
}
