//! Edge-case acceptance pins for scheduled account deletion.
//!
//! Complements `account_deletion_acceptance`: these cases pin the model
//! constraints that a text-preserving regression could otherwise break --
//! column defaults, no cascade/destructive migration, no deadline
//! extension on idempotent re-schedule, cancel ordering, the erasure-start
//! heartbeat, and the owner-scoped status column list. They read
//! migration/SQL text and exercise only pure outcome mapping; they do not
//! contact a database or network.

use nntune_api::account_deletion::{
    self, CancelOutcome, CANCEL_ACCOUNT_DELETION_SQL, LOCK_ACCOUNT_DELETION_USER_SQL,
    OPERATION_STATUS_SQL, SCHEDULE_ACCOUNT_DELETION_SQL, START_ACCOUNT_ERASURE_SQL,
};
use std::fs;
use std::path::PathBuf;

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn account_deletion_migration() -> (PathBuf, String) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let mut matches = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", root.display()))
        .map(|entry| entry.expect("migration directory entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_account_deletion.sql"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    assert_eq!(
        matches.len(),
        1,
        "expected one *_account_deletion.sql migration"
    );
    let path = matches.pop().unwrap();
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    (path, normalize(&text))
}

#[test]
fn migration_is_forward_only_with_pinned_column_defaults() {
    let (path, sql) = account_deletion_migration();

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("migration file name");
    let sequence: u32 = file_name[..4]
        .parse()
        .unwrap_or_else(|_| panic!("migration must keep a 4-digit sequence prefix: {file_name}"));
    assert!(
        sequence > 5,
        "account-deletion migration must follow the retained 0005 migration: {file_name}"
    );

    assert!(
        sql.contains("iduuidprimarykeydefaultgen_random_uuid()"),
        "operation ids are database-generated UUIDs: {sql}"
    );
    assert!(
        sql.contains("cursorjsonbnotnulldefault'{}'::jsonb"),
        "cursor defaults to an empty JSON object and is never null: {sql}"
    );
    assert!(
        sql.contains("statetextnotnulldefault'pending'"),
        "non-account operations default to pending: {sql}"
    );
    assert!(
        sql.contains("heartbeat_attimestamptznotnulldefaultnow()"),
        "heartbeat is stamped at insert: {sql}"
    );
    assert!(
        sql.contains("consecutive_failuressmallintnotnulldefault0")
            && sql.contains("check(consecutive_failures>=0)"),
        "failure counter starts at zero and cannot go negative: {sql}"
    );
    assert!(sql.contains("last_errortextnull"), "{sql}");
    assert!(sql.contains("started_attimestamptznotnull"), "{sql}");
    assert!(sql.contains("container_idtextnull"), "{sql}");
    assert!(sql.contains("erase_aftertimestamptznull"), "{sql}");

    assert!(
        !sql.contains("ondeletecascade"),
        "user deletion must never cascade past the rule-3 procedure: {sql}"
    );
    assert!(
        !sql.contains("droptable")
            && !sql.contains("dropcolumn")
            && !sql.contains("truncate")
            && !sql.contains("deletefrom"),
        "migration is additive only, with no destructive statement: {sql}"
    );
}

#[test]
fn schedule_retry_cannot_extend_the_existing_deadline() {
    let sql = normalize(SCHEDULE_ACCOUNT_DELETION_SQL);
    assert!(
        sql.contains("user_id,scope,container_id,cursor,state,erase_after,started_at"),
        "insert column order is pinned: {sql}"
    );
    assert!(
        sql.contains("'account',null,'{}'::jsonb,'scheduled'"),
        "account rows carry no container and an empty cursor: {sql}"
    );
    let conflict_tail = sql
        .split("onconflict")
        .nth(1)
        .expect("schedule must handle the per-user account conflict");
    assert!(
        !conflict_tail.contains("erase_after=") && !conflict_tail.contains("now()+"),
        "a schedule retry must not move erase_after: {conflict_tail}"
    );
    assert!(
        sql.contains("$1") && !sql.contains("$2") && !sql.contains("deletefrom"),
        "schedule binds only the user id and deletes nothing: {sql}"
    );
    assert_eq!(
        account_deletion::GRACE_SECONDS,
        24 * 60 * 60,
        "grace constant must stay in sync with the SQL 24-hour interval"
    );
    assert!(sql.contains("interval'24hours'"), "{sql}");
}

#[test]
fn cancel_deletes_the_scheduled_row_before_clearing_the_fence() {
    let sql = normalize(CANCEL_ACCOUNT_DELETION_SQL);
    assert!(
        sql.contains("withcancelledas(deletefromdelete_operations"),
        "cancellation compare-and-set is the delete of the scheduled row: {sql}"
    );
    assert!(
        sql.contains("returninguser_id") && sql.contains("returningusers.id"),
        "cancel returns the deleted owner then the unfenced user: {sql}"
    );
    let delete_at = sql.find("deletefrom").expect("cancel deletes a row");
    let fence_at = sql
        .find("setdeleting_at=null")
        .expect("cancel clears the fence");
    assert!(
        delete_at < fence_at,
        "the fence clear must stay downstream of the row delete: {sql}"
    );
    assert!(
        !sql.contains("setstate=") && !sql.contains("$3"),
        "cancel never resurrects state and binds only operation and user: {sql}"
    );
}

#[test]
fn erasure_start_stamps_heartbeat_and_matches_scheduled_only() {
    let sql = normalize(START_ACCOUNT_ERASURE_SQL);
    assert!(
        sql.contains("setstate='running',heartbeat_at=now()"),
        "executor start advances state and heartbeat together: {sql}"
    );
    assert!(sql.contains("returningid"), "{sql}");
    assert!(
        !sql.contains("erase_after<now()"),
        "an operation at exactly its deadline is eligible to start: {sql}"
    );
    assert!(
        !sql.contains("statein(") && !sql.contains("'pending'") && !sql.contains("'failed'"),
        "start must not sweep pending/failed rows, scheduled only: {sql}"
    );
    assert!(
        sql.contains("$1") && !sql.contains("$2") && !sql.contains("deletefrom"),
        "start binds only the operation id and deletes nothing: {sql}"
    );
}

#[test]
fn operation_status_reports_every_lifecycle_column_without_locking() {
    let sql = normalize(OPERATION_STATUS_SQL);
    assert!(
        sql.contains(
            "selectid,scope,container_id,cursor,state,erase_after,heartbeat_at,consecutive_failures,last_error,started_atfromdelete_operations"
        ),
        "status reports the full lifecycle column list: {sql}"
    );
    assert!(
        !sql.contains("state='") && !sql.contains("statein("),
        "status stays visible in every state, including running/failed: {sql}"
    );
    assert!(
        !sql.contains("forupdate") && !sql.contains("$3"),
        "status is a non-locking read binding only operation and user: {sql}"
    );
}

#[test]
fn cancellation_lock_waits_for_the_single_owner_row() {
    let sql = normalize(LOCK_ACCOUNT_DELETION_USER_SQL);
    assert!(
        !sql.contains("nowait") && !sql.contains("skiplocked"),
        "cancellation lock must wait, never skip past a competing writer: {sql}"
    );
    assert!(
        sql.contains("$1") && !sql.contains("$2"),
        "lock binds only the owner id: {sql}"
    );
}

#[test]
fn cancel_outcome_boundary_is_zero_rows_only_and_sql_handles_are_distinct() {
    assert_eq!(
        account_deletion::cancel_outcome(0),
        CancelOutcome::ErasureStarted
    );
    assert_eq!(
        account_deletion::cancel_outcome(1),
        CancelOutcome::Cancelled
    );
    assert_eq!(
        account_deletion::cancel_outcome(2),
        CancelOutcome::Cancelled
    );
    assert_eq!(
        account_deletion::cancel_outcome(u64::MAX),
        CancelOutcome::Cancelled
    );

    let statements = [
        ("schedule", SCHEDULE_ACCOUNT_DELETION_SQL),
        ("lock", LOCK_ACCOUNT_DELETION_USER_SQL),
        ("cancel", CANCEL_ACCOUNT_DELETION_SQL),
        ("start", START_ACCOUNT_ERASURE_SQL),
        ("status", OPERATION_STATUS_SQL),
    ];
    for (index, (name, sql)) in statements.iter().enumerate() {
        assert!(!sql.trim().is_empty(), "{name} SQL must not be empty");
        for (other_name, other_sql) in &statements[index + 1..] {
            assert_ne!(
                normalize(sql),
                normalize(other_sql),
                "{name} and {other_name} SQL must not collapse into one statement"
            );
        }
    }
}
