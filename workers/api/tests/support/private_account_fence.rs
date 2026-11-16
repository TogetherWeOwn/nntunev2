//! Shared account-row fence controls for future private-session mutations.
use super::{blocker_observer::observe_blocked, connect, disposable_config, SCHEMA};
use nntune_api::account_deletion::PRIVATE_SESSION_ACCOUNT_FENCE_SQL as FENCE;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio_postgres::Client;

async fn fence(db: &Client, account: &str) -> Result<usize, tokio_postgres::Error> {
    Ok(db.query(FENCE, &[&account]).await?.len())
}

#[tokio::test]
async fn private_account_fence_serializes_and_refuses_closed_accounts() {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").expect("disposable database URL");
    let config = disposable_config(&url).expect("scratch database configuration");
    let schema = format!(
        "private_fence_{}_{}",
        std::process::id(),
        SCHEMA.fetch_add(1, Ordering::Relaxed)
    );
    let admin = connect(&config, "").await;
    admin
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let holder = connect(&config, &schema).await;
    let waiter = connect(&config, &schema).await;
    let observer = connect(&config, &schema).await;

    let result = tokio::time::timeout(Duration::from_secs(10), async {
        observer.batch_execute("CREATE TABLE users (id text PRIMARY KEY, deleting_at timestamptz); INSERT INTO users VALUES ('a', NULL), ('b', NULL), ('deleting', now())").await?;
        holder.batch_execute("BEGIN; SET LOCAL lock_timeout = '3s'").await?;
        waiter.batch_execute("BEGIN; SET LOCAL lock_timeout = '3s'").await?;
        let holder_pid: i32 = holder.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        let waiter_pid: i32 = waiter.query_one("SELECT pg_backend_pid()", &[]).await?.get(0);
        let held = fence(&holder, "a").await?;
        let absent = fence(&holder, "missing").await?;
        let deleting = fence(&holder, "deleting").await?;
        let independent = fence(&waiter, "b").await?;

        // Drive the actual waiting statement and identify its specific blocker,
        // rather than inferring serialization from elapsed time.
        let account = "a";
        let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[&account];
        let pending = waiter.query(FENCE, params);
        tokio::pin!(pending);
        observe_blocked(&observer, holder_pid, waiter_pid, pending.as_mut()).await?;
        holder.batch_execute("ROLLBACK").await.map_err(|error| error.to_string())?;
        let acquired = pending.await.map_err(|error| error.to_string())?.len();
        Ok::<_, Box<dyn std::error::Error>>((held, absent, deleting, independent, acquired))
    }).await;

    // End both transactions before dropping the isolated schema, including on
    // error or timeout; assertions happen only after cleanup.
    let _ = holder.batch_execute("ROLLBACK").await;
    let _ = waiter.batch_execute("ROLLBACK").await;
    let cleanup = admin
        .batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await;
    cleanup.expect("drop private fence fixture");
    let counts = result
        .expect("bounded fence control")
        .expect("fence control");
    assert_eq!(counts, (1, 0, 0, 1, 1));
}
