//! Disposable Worker-only controls for the dormant owned reconciliation store.
use super::{billing_budget, MirrorBinding, PgStore, Store, StoreError};
use crate::billing_reconcile::{PaidInvoiceRow, ReconcileOutcome, ReconciledRow};
use crate::billing_transaction_sql::ReconcileProbe;
const CONNECTION: &str =
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
    if probe == ReconcileProbe::ServerWatchdog {
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
    if recovered[0] != before[0] {
        return Err("row-6 recovery changed the existing subscription");
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

async fn fault_snapshot() -> Result<[String; 2], StoreError> {
    fault_snapshot_with_recovery(false).await
}

async fn fault_snapshot_with_recovery(recovered: bool) -> Result<[String; 2], StoreError> {
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
                                 FROM public.paid_invoice_history h)",
                        &[&recovered],
                    )
                    .await
                    .map_err(|_| StoreError)?;
                if row.get::<_, Option<bool>>(0) != Some(true)
                    || row.get::<_, Option<bool>>(1) != Some(true)
                {
                    return Err(StoreError);
                }
                Ok([row.get(2), row.get(3)])
            })
        },
        |_| {},
    )
    .await
}

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
    late_fault_and_retry().await
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

// The relay withholds a successful COMMIT response from the test-feature
// prototype. Observe the committed rows before retrying the same outcome.
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
