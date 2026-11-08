//! Outbound-rate tests use TEMP shadows or a uniquely named disposable schema;
//! the shared public budget is never read or mutated. No test sleeps; denial
//! is made deterministic with a long cooldown.
use super::*;

async fn rate_client() -> Client {
    let db = client().await;
    db.batch_execute(
        "CREATE TEMP TABLE listenbrainz_outbound_rate
         (LIKE public.listenbrainz_outbound_rate INCLUDING ALL)",
    )
    .await
    .unwrap();
    db
}

async fn db_now_ms(db: &Client) -> i64 {
    db.query_one(
        "SELECT (extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

async fn next_allowed(db: &Client) -> i64 {
    db.query_one(
        "SELECT next_allowed_at_ms FROM pg_temp.listenbrainz_outbound_rate",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

async fn row_count(db: &Client) -> i64 {
    db.query_one(
        "SELECT count(*) FROM pg_temp.listenbrainz_outbound_rate",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test]
async fn absent_row_claims_once_then_denies_with_one_second_floor() {
    let db = rate_client().await;
    assert_eq!(row_count(&db).await, 0);
    let before = db_now_ms(&db).await;
    assert!(claim_outbound(&db).await.unwrap());
    assert_eq!(row_count(&db).await, 1);
    assert!(next_allowed(&db).await >= before + 1000);
    // A long cooldown makes the immediate denial independent of the 1s edge.
    defer_outbound(&db, 60).await.unwrap();
    assert!(next_allowed(&db).await >= before + 60_000);
    assert!(!claim_outbound(&db).await.unwrap());
    assert!(!claim_outbound(&db).await.unwrap());
    assert_eq!(row_count(&db).await, 1);
}

#[tokio::test]
async fn cooldown_floor_survives_shorter_and_out_of_order_defers() {
    let db = rate_client().await;
    let before = db_now_ms(&db).await;
    defer_outbound(&db, 120).await.unwrap();
    assert_eq!(row_count(&db).await, 1);
    let floor = next_allowed(&db).await;
    assert!(floor >= before + 120_000);
    for wait in [1, 0, 30, 60] {
        defer_outbound(&db, wait).await.unwrap();
        assert_eq!(next_allowed(&db).await, floor);
        assert!(!claim_outbound(&db).await.unwrap());
        assert_eq!(next_allowed(&db).await, floor);
    }
    defer_outbound(&db, 600).await.unwrap();
    assert!(next_allowed(&db).await >= before + 600_000);
    assert_eq!(row_count(&db).await, 1);
}

#[tokio::test]
async fn huge_cooldown_saturates_at_i64_max_and_denies() {
    let db = rate_client().await;
    defer_outbound(&db, u64::MAX).await.unwrap();
    assert_eq!(next_allowed(&db).await, i64::MAX);
    assert!(!claim_outbound(&db).await.unwrap());
    defer_outbound(&db, 1).await.unwrap();
    defer_outbound(&db, u64::MAX).await.unwrap();
    assert_eq!(next_allowed(&db).await, i64::MAX);
    assert!(!claim_outbound(&db).await.unwrap());
    assert_eq!(row_count(&db).await, 1);
}

#[tokio::test]
async fn malformed_table_propagates_error_instead_of_allowing() {
    let db = rate_client().await;
    db.batch_execute(
        "ALTER TABLE pg_temp.listenbrainz_outbound_rate
         RENAME COLUMN next_allowed_at_ms TO renamed_away",
    )
    .await
    .unwrap();
    assert!(claim_outbound(&db).await.is_err());
    assert!(defer_outbound(&db, 1).await.is_err());
    assert_eq!(row_count(&db).await, 0);
}

#[tokio::test]
async fn separate_connections_observe_committed_cooldown_after_claim_race() {
    let db = client().await;
    let other = client().await;
    let id: String = db
        .query_one("SELECT replace(gen_random_uuid()::text, '-', '')", &[])
        .await
        .unwrap()
        .get(0);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    let schema = format!("lb_rate_{id}");
    db.batch_execute(&format!(
        "CREATE SCHEMA {schema}; \
         CREATE TABLE {schema}.listenbrainz_outbound_rate \
         (LIKE public.listenbrainz_outbound_rate INCLUDING ALL); \
         SET search_path = {schema}, pg_temp, public"
    ))
    .await
    .unwrap();
    other
        .batch_execute(&format!("SET search_path = {schema}, pg_temp, public"))
        .await
        .unwrap();
    // Hold the absent-row claim uncommitted. The second connection must not
    // admit another request and must recheck the cooldown after our commit.
    db.batch_execute("BEGIN").await.unwrap();
    assert!(claim_outbound(&db).await.unwrap());
    let pending = claim_outbound(&other);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    defer_outbound(&db, 120).await.unwrap();
    db.batch_execute("COMMIT").await.unwrap();
    let admitted = tokio::time::timeout(Duration::from_secs(3), &mut pending)
        .await
        .unwrap()
        .unwrap();
    db.batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    assert!(
        !admitted,
        "a competing claim must observe the committed floor"
    );
}

#[tokio::test]
async fn pipelined_claims_on_absent_row_have_one_winner() {
    let db = rate_client().await;
    let (a, b, c, d) = tokio::join!(
        claim_outbound(&db),
        claim_outbound(&db),
        claim_outbound(&db),
        claim_outbound(&db)
    );
    let wins = [a, b, c, d]
        .into_iter()
        .map(|result| result.unwrap())
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
    assert_eq!(row_count(&db).await, 1);
}
