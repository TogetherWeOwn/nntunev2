//! Outbound-rate tests use TEMP shadows or a uniquely named disposable schema;
//! the shared public budget is never read or mutated. No test sleeps; denial
//! is made deterministic with a long cooldown.
use super::*;
use crate::listenbrainz::OutboundClaim;

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

async fn rate_state(db: &Client) -> (i64, bool, i64) {
    let row = db
        .query_one(
            "SELECT next_allowed_at_ms, in_flight, generation \
             FROM pg_temp.listenbrainz_outbound_rate",
            &[],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

/// Deterministically expire the cooldown floor; no test waits on wall time.
async fn expire_floor(db: &Client) {
    db.batch_execute("UPDATE pg_temp.listenbrainz_outbound_rate SET next_allowed_at_ms = 0")
        .await
        .unwrap();
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
    let claim = claim_outbound(&db).await.unwrap();
    assert!(claim.is_some());
    assert_eq!(row_count(&db).await, 1);
    assert!(next_allowed(&db).await >= before + 1000);
    // A long cooldown makes the immediate denial independent of the 1s edge.
    defer_outbound(&db, 60).await.unwrap();
    assert!(next_allowed(&db).await >= before + 60_000);
    assert!(claim_outbound(&db).await.unwrap().is_none());
    assert!(claim_outbound(&db).await.unwrap().is_none());
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
        assert!(claim_outbound(&db).await.unwrap().is_none());
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
    assert!(claim_outbound(&db).await.unwrap().is_none());
    defer_outbound(&db, 1).await.unwrap();
    defer_outbound(&db, u64::MAX).await.unwrap();
    assert_eq!(next_allowed(&db).await, i64::MAX);
    assert!(claim_outbound(&db).await.unwrap().is_none());
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
    assert!(claim_outbound(&db).await.unwrap().is_some());
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
        admitted.is_none(),
        "a competing claim must observe the committed floor"
    );
}

#[tokio::test]
async fn successful_claim_samples_time_after_waiting_for_expired_row_lock() {
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
         INSERT INTO {schema}.listenbrainz_outbound_rate (singleton, next_allowed_at_ms) VALUES (TRUE, 0); \
         SET search_path = {schema}, pg_temp, public"
    ))
    .await
    .unwrap();
    // Commit setup before the explicit transaction so the contender can see
    // the schema and row, rather than finishing early with a lookup error.
    db.batch_execute("BEGIN; SELECT singleton FROM listenbrainz_outbound_rate FOR UPDATE")
        .await
        .unwrap();
    other
        .batch_execute(&format!("SET search_path = {schema}, pg_temp, public"))
        .await
        .unwrap();
    let pending = claim_outbound(&other);
    tokio::pin!(pending);
    // Deliberately hold the expired row longer than the admission interval.
    // The old implementation sampled before the wait and returned an already
    // expired floor; no production/provider access or host sleep is involved.
    let blocked = tokio::time::timeout(Duration::from_millis(1200), &mut pending).await;
    let release_ms = db_now_ms(&db).await;
    db.batch_execute("COMMIT").await.unwrap();
    let waited = blocked.is_err();
    // Never poll a completed future again: preserve an early SQL error as
    // the actual test diagnostic rather than an async-resume panic.
    let admitted = match blocked {
        Err(_) => tokio::time::timeout(Duration::from_secs(3), &mut pending)
            .await
            .unwrap(),
        Ok(result) => result,
    };
    let floor: i64 = db
        .query_one(
            "SELECT next_allowed_at_ms FROM listenbrainz_outbound_rate",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    db.batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    assert!(
        admitted.unwrap().is_some(),
        "claim must succeed after releasing the row"
    );
    assert!(waited, "claim must wait for the held row");
    assert!(
        floor >= release_ms + 1000,
        "successful claim must spend a fresh interval"
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
        .filter(|won| won.is_some())
        .count();
    assert_eq!(wins, 1);
    assert_eq!(row_count(&db).await, 1);
}

#[tokio::test]
async fn busy_row_denies_even_with_expired_floor() {
    let db = rate_client().await;
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    assert!(rate_state(&db).await.1);
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_none());
    // A defer is monotonic cooldown only and never releases the busy mark.
    defer_outbound(&db, 1).await.unwrap();
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_none());
    let (_, busy, generation) = rate_state(&db).await;
    assert!(busy);
    assert_eq!(generation, 1);
    drop(claim);
    // Dropping the claim performs no recovery: the durable busy mark stays.
    assert!(claim_outbound(&db).await.unwrap().is_none());
    assert!(rate_state(&db).await.1);
}

#[tokio::test]
async fn finish_matching_generation_persists_cooldown_and_clears_busy() {
    let db = rate_client().await;
    let before = db_now_ms(&db).await;
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    finish_outbound(&db, claim, 60).await.unwrap();
    let (floor, busy, generation) = rate_state(&db).await;
    assert!(!busy);
    assert_eq!(generation, 1);
    assert!(floor >= before + 60_000);
    assert!(claim_outbound(&db).await.unwrap().is_none());
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_some());
    assert_eq!(rate_state(&db).await.2, 2);
}

#[tokio::test]
async fn finish_never_lowers_existing_cooldown() {
    let db = rate_client().await;
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    defer_outbound(&db, 600).await.unwrap();
    let floor = next_allowed(&db).await;
    finish_outbound(&db, claim, 0).await.unwrap();
    let (after, busy, _) = rate_state(&db).await;
    assert!(!busy);
    assert_eq!(after, floor);
}

#[tokio::test]
async fn stale_generation_cannot_clear_newer_claim() {
    let db = rate_client().await;
    let first = claim_outbound(&db).await.unwrap().unwrap();
    finish_outbound(&db, first, 0).await.unwrap();
    expire_floor(&db).await;
    let second = claim_outbound(&db).await.unwrap().unwrap();
    assert_eq!(rate_state(&db).await.2, 2);
    // Forge the superseded generation: it must not touch the newer claim.
    assert!(finish_outbound(&db, OutboundClaim(1), 600).await.is_err());
    let (floor, busy, generation) = rate_state(&db).await;
    assert!(busy);
    assert_eq!(generation, 2);
    assert!(floor < db_now_ms(&db).await + 300_000);
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_none());
    finish_outbound(&db, second, 0).await.unwrap();
    assert!(!rate_state(&db).await.1);
}

#[tokio::test]
async fn finish_sql_error_leaves_busy_and_denies() {
    let db = rate_client().await;
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    // Any attempt to clear the busy mark now violates the constraint.
    db.batch_execute(
        "ALTER TABLE pg_temp.listenbrainz_outbound_rate
         ADD CONSTRAINT keep_busy CHECK (in_flight)",
    )
    .await
    .unwrap();
    assert!(finish_outbound(&db, claim, 1).await.is_err());
    db.batch_execute("ALTER TABLE pg_temp.listenbrainz_outbound_rate DROP CONSTRAINT keep_busy")
        .await
        .unwrap();
    assert!(rate_state(&db).await.1);
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_none());
    assert_eq!(rate_state(&db).await.2, 1);
}

#[tokio::test]
async fn finish_with_u64_max_saturates_and_clears_busy() {
    let db = rate_client().await;
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    finish_outbound(&db, claim, u64::MAX).await.unwrap();
    let (floor, busy, _) = rate_state(&db).await;
    assert_eq!(floor, i64::MAX);
    assert!(!busy);
    assert!(claim_outbound(&db).await.unwrap().is_none());
}

#[tokio::test]
async fn exhausted_generation_denies_without_overflow() {
    let db = rate_client().await;
    db.batch_execute(
        "INSERT INTO pg_temp.listenbrainz_outbound_rate
         (singleton, next_allowed_at_ms, in_flight, generation)
         VALUES (TRUE, 0, FALSE, 9223372036854775806)",
    )
    .await
    .unwrap();
    let last = claim_outbound(&db).await.unwrap().unwrap();
    assert_eq!(rate_state(&db).await.2, i64::MAX);
    finish_outbound(&db, last, 0).await.unwrap();
    expire_floor(&db).await;
    assert!(claim_outbound(&db).await.unwrap().is_none());
    let (_, busy, generation) = rate_state(&db).await;
    assert!(!busy);
    assert_eq!(generation, i64::MAX);
}

#[tokio::test]
async fn separate_connections_deny_while_busy_then_admit_after_finish() {
    let db = client().await;
    let other = client().await;
    let id: String = db
        .query_one("SELECT replace(gen_random_uuid()::text, '-', '')", &[])
        .await
        .unwrap()
        .get(0);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    let schema = format!("lb_rate_{id}");
    let path = format!("SET search_path = {schema}, pg_temp, public");
    db.batch_execute(&format!(
        "CREATE SCHEMA {schema}; \
         CREATE TABLE {schema}.listenbrainz_outbound_rate \
         (LIKE public.listenbrainz_outbound_rate INCLUDING ALL); \
         {path}"
    ))
    .await
    .unwrap();
    other.batch_execute(&path).await.unwrap();
    let expire = "UPDATE listenbrainz_outbound_rate SET next_allowed_at_ms = 0";
    let claim = claim_outbound(&db).await.unwrap().unwrap();
    db.batch_execute(expire).await.unwrap();
    let denied = claim_outbound(&other).await;
    finish_outbound(&other, claim, 120).await.unwrap();
    let cooling = claim_outbound(&db).await;
    db.batch_execute(expire).await.unwrap();
    let admitted = claim_outbound(&other).await;
    db.batch_execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    assert!(
        denied.unwrap().is_none(),
        "busy must deny across connections"
    );
    assert!(
        cooling.unwrap().is_none(),
        "finish must persist the cooldown"
    );
    assert!(admitted.unwrap().is_some(), "cleared and expired admits");
}
