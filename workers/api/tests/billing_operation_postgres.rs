//! Exercise the real operation guard, injecting only disposable connections.
#![cfg(feature = "postgres-tests")]

use nntune_api::handlers::Store;
use nntune_api::worker_entry::session_retry_store_for_postgres_test;
use std::cell::Cell;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;
use tokio_postgres::{Client, Config, NoTls};

async fn connect() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("disposable PostgreSQL is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '3s'")
        .await
        .unwrap();
    client
}

#[tokio::test]
async fn billing_operation_guard_excludes_same_user_not_other_users_or_write_namespace() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let owner = session_retry_store_for_postgres_test(connect().await);
        let contender = session_retry_store_for_postgres_test(connect().await);
        let unrelated = session_retry_store_for_postgres_test(connect().await);
        let mut observer = connect().await;
        let polled = Cell::new(false);
        owner
            .with_billing_operation("guard-user", async {
                assert!(contender
                    .with_billing_operation("guard-user", async {
                        polled.set(true);
                    })
                    .await
                    .is_err());
                assert!(!polled.get(), "busy guard must not start remote work");
                assert_eq!(
                    unrelated
                        .with_billing_operation("other-guard-user", async { 7 })
                        .await
                        .unwrap(),
                    7
                );
                // Reconcile writes use the single-bigint namespace. Holding the
                // operation transaction must not deadlock a nested write there.
                let transaction = observer.transaction().await.unwrap();
                let row = transaction
                    .query_one(
                        "SELECT pg_try_advisory_xact_lock(hashtext($1)::bigint)",
                        &[&"guard-user"],
                    )
                    .await
                    .unwrap();
                assert!(row.get::<_, bool>(0));
                transaction.commit().await.unwrap();
            })
            .await
            .unwrap();
        let next = session_retry_store_for_postgres_test(connect().await);
        next.with_billing_operation("guard-user", async {})
            .await
            .unwrap();
    })
    .await
    .expect("operation guard test exceeded its bound");
}

#[tokio::test]
async fn dropping_billing_operation_future_rolls_back_and_releases_guard() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let owner = session_retry_store_for_postgres_test(connect().await);
        let entered = Cell::new(false);
        let mut operation = Box::pin(owner.with_billing_operation("dropped-guard-user", async {
            entered.set(true);
            std::future::pending::<()>().await;
        }));
        poll_fn(|cx| {
            assert!(operation.as_mut().poll(cx).is_pending());
            if entered.get() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        drop(operation);
        // ROLLBACK is queued on drop. A bounded blocking acquisition waits
        // for that rollback instead of assuming it was already processed.
        let mut observer = connect().await;
        let transaction = observer.transaction().await.unwrap();
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtext('billing_operation'), hashtext($1))",
                &[&"dropped-guard-user"],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        let next = session_retry_store_for_postgres_test(connect().await);
        next.with_billing_operation("dropped-guard-user", async {})
            .await
            .unwrap();
    })
    .await
    .expect("dropped operation guard was not released");
}
