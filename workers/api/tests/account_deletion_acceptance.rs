//! Test-only contract pins for scheduled account deletion.
//!
//! These checks read migration/SQL text and exercise only pure outcome
//! mapping. They do not contact a database or network. The implementation
//! belongs in the API module and a forward-only migration.

use nntune_api::account_deletion::{
    self, CancelOutcome, CANCEL_ACCOUNT_DELETION_SQL, OPERATION_STATUS_SQL,
    SCHEDULE_ACCOUNT_DELETION_SQL, START_ACCOUNT_ERASURE_SQL,
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

fn creates_users_table(sql: &str) -> bool {
    let sql = sql
        .to_ascii_lowercase()
        .replace('"', "")
        .replace('`', "")
        .replace('[', "")
        .replace(']', "")
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace(',', " , ")
        .replace(';', " ; ");
    let tokens = sql.split_whitespace().collect::<Vec<_>>();

    (0..tokens.len()).any(|index| {
        if tokens[index] != "create" {
            return false;
        }
        let mut table = index + 1;
        if matches!(
            tokens.get(table),
            Some(&"temp" | &"temporary" | &"unlogged")
        ) {
            table += 1;
        }
        if tokens.get(table) != Some(&"table") {
            return false;
        }
        table += 1;
        if tokens.get(table..table + 3) == Some(["if", "not", "exists"].as_slice()) {
            table += 3;
        }
        tokens
            .get(table)
            .is_some_and(|name| name.rsplit('.').next() == Some("users"))
    })
}

fn account_deletion_migration() -> (PathBuf, String, String) {
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
    (path, text.clone(), normalize(&text))
}

#[test]
fn migration_evolves_users_and_defines_restrictive_delete_operations() {
    let (_, raw_sql, sql) = account_deletion_migration();
    let statements = sql.split(';').collect::<Vec<_>>();
    assert!(
        statements.iter().any(|statement| {
            statement.contains("altertableusers")
                && statement.contains("addcolumndeleting_attimestamptznull")
        }),
        "users.deleting_at must be an additive nullable column: {sql}"
    );
    assert!(
        !creates_users_table(&raw_sql),
        "auth users table must be retained"
    );

    let delete_operations = statements
        .iter()
        .find(|statement| {
            statement.contains("createtabledelete_operations")
                || statement.contains("createtableifnotexistsdelete_operations")
        })
        .expect("migration must create delete_operations");
    assert!(delete_operations.contains("user_idtextnotnull"), "{sql}");
    assert!(
        delete_operations.contains("referencesusers(id)ondeleterestrict"),
        "operation rows must restrict user deletion: {sql}"
    );
    assert!(
        delete_operations.contains("scopetextnotnull")
            && delete_operations.contains("check(scopein('account','library','playlist'))"),
        "scope is constrained to supported operation kinds: {sql}"
    );
    assert!(
        delete_operations
            .contains("check(statein('scheduled','pending','running','failed','done'))"),
        "state is constrained to supported lifecycle states: {sql}"
    );
    assert!(
        delete_operations.contains("check((scope='account')=(erase_afterisnotnull))"),
        "only account operations have an erasure deadline: {sql}"
    );
    assert!(
        delete_operations.contains("check((scope='account')=(container_idisnull))"),
        "only account operations omit a container: {sql}"
    );
    assert!(
        statements.iter().any(|statement| {
            statement.contains("createuniqueindex")
                && statement.contains("ondelete_operations(user_id)wherescope='account'")
        }),
        "one partial unique account operation per user: {sql}"
    );
    assert!(
        statements.iter().any(|statement| {
            statement.contains("createuniqueindex")
                && statement.contains(
                    "ondelete_operations(user_id,scope,container_id)wherescopein('library','playlist')"
                )
        }),
        "one partial unique operation per user/container scope: {sql}"
    );
}

#[test]
fn schedule_sql_sets_fence_and_idempotently_returns_existing_operation() {
    let sql = normalize(SCHEDULE_ACCOUNT_DELETION_SQL);
    assert!(sql.contains("updateusers"), "{sql}");
    assert!(sql.contains("setdeleting_at"), "{sql}");
    assert!(sql.contains("insertintodelete_operations"), "{sql}");
    assert!(sql.contains("'account'"), "{sql}");
    assert!(sql.contains("'scheduled'"), "{sql}");
    assert!(sql.contains("now()+interval'24hours'"), "{sql}");
    assert!(
        sql.contains("onconflict") && sql.contains("returningid"),
        "repeat scheduling returns the existing operation id: {sql}"
    );
}

#[test]
fn cancel_sql_is_owner_scoped_compare_and_set_and_clears_fence() {
    let sql = normalize(CANCEL_ACCOUNT_DELETION_SQL);
    assert!(
        sql.contains("whereid=$1anduser_id=$2andstate='scheduled'"),
        "cancellation must win only for this owner's scheduled operation: {sql}"
    );
    assert!(sql.contains("updateusers"), "{sql}");
    assert!(sql.contains("setdeleting_at=null"), "{sql}");
}

#[test]
fn erasure_start_sql_is_deadline_guarded_compare_and_set() {
    let sql = normalize(START_ACCOUNT_ERASURE_SQL);
    assert!(
        sql.contains("state='scheduled'anderase_after<=now()"),
        "a scheduled operation starts only after its deadline: {sql}"
    );
    assert!(sql.contains("setstate='running'"), "{sql}");
}

#[test]
fn operation_status_sql_is_owner_scoped() {
    let sql = normalize(OPERATION_STATUS_SQL);
    assert!(
        sql.contains("user_id=$2"),
        "status lookup must bind the owner: {sql}"
    );
}

#[test]
fn cancellation_outcomes_and_grace_period_are_stable() {
    assert_eq!(account_deletion::GRACE_SECONDS, 86_400);
    assert_eq!(
        account_deletion::cancel_outcome(0),
        CancelOutcome::ErasureStarted
    );
    assert_eq!(CancelOutcome::ErasureStarted.http_status(), 409);
    assert_eq!(
        account_deletion::cancel_outcome(1),
        CancelOutcome::Cancelled
    );
    assert_eq!(CancelOutcome::Cancelled.http_status(), 200);
}
