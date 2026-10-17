//! Test-first acceptance pins for ingest writes racing account deletion.
//!
//! No live database is contacted. The statement must lock and check the owner
//! in the same transaction as publication so a scheduled deletion cannot be
//! followed by a recreated ingest-owned row.

use nntune_ingest::library::upsert_library_sql;

#[test]
fn ingest_write_locks_active_owner_before_publishing() {
    let sql = upsert_library_sql();
    assert!(
        sql.contains("FOR SHARE") && sql.contains("users") && sql.contains("deleting_at IS NULL"),
        "ingest publication must lock the user and reject non-NULL deleting_at: {sql}"
    );
    assert!(
        sql.contains("account_deleting")
            && sql.contains("operation_id")
            && sql.contains("retry_after"),
        "fenced ingest writes must surface 409 account_deleting with operation_id and retry_after"
    );
}

#[test]
fn active_account_ingest_write_remains_idempotent() {
    let sql = upsert_library_sql();
    assert!(
        sql.contains("deleting_at IS NULL"),
        "NULL fence must permit active users: {sql}"
    );
    assert!(
        sql.contains("ON CONFLICT (idempotency_key) DO UPDATE SET"),
        "active account retries must preserve existing idempotent upsert behavior: {sql}"
    );
}
