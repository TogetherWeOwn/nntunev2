//! Account-deletion persistence contract for ADR 0002 rule 3.
//!
//! These parameterized statements define the data-model boundary only; HTTP
//! routes and execution are implemented by later slices. Scheduling atomically
//! marks the user and creates (or returns) the unique account operation. Cancel
//! and executor start race on the same `state = 'scheduled'` row: their
//! compare-and-set writes are mutually exclusive, so only one can win.

/// The cancellable account-deletion grace period defined by ADR 0002 rule 3.
pub const GRACE_SECONDS: i64 = 86_400;

/// Atomically fence the user and create, or return, the unique account operation.
///
/// `$1` is the TEXT user id. The operation's 24-hour deadline is based on the
/// database clock, and a retry returns the existing operation id without
/// extending its deadline. A zero-row result means the user does not exist.
pub const SCHEDULE_ACCOUNT_DELETION_SQL: &str = r#"
WITH fenced_user AS (
    UPDATE users
    SET deleting_at = COALESCE(deleting_at, now())
    WHERE id = $1
    RETURNING id
)
INSERT INTO delete_operations (
    user_id, scope, container_id, cursor, state, erase_after, started_at
)
SELECT id, 'account', NULL, '{}'::jsonb, 'scheduled',
       now() + interval '24 hours', now()
FROM fenced_user
ON CONFLICT (user_id) WHERE scope = 'account'
DO UPDATE SET id = delete_operations.id
RETURNING id
"#;

/// Take a transaction-scoped lock on the account owner row before cancellation.
///
/// `$1` is the TEXT user id. The caller must keep this lock while executing the
/// cancellation statement, so schedule and cancel acquire user before operation.
pub const LOCK_ACCOUNT_DELETION_USER_SQL: &str = r#"
SELECT id FROM users WHERE id = $1 FOR UPDATE
"#;

/// Cancel only the owner's still-scheduled operation and clear its user fence.
///
/// `$1` is the operation UUID and `$2` is the TEXT user id. The caller must first
/// execute `LOCK_ACCOUNT_DELETION_USER_SQL` for `$2` in the same transaction,
/// then verify the operation exists and belongs to that user before mapping a
/// zero-row result. Deleting the scheduled row is the cancellation compare-and-set;
/// if executor start has changed its state to `running`, this statement returns
/// no row and leaves the fence in place (ADR 0002 rule 3). With that prior
/// owner-scoped check, zero rows means the ADR's 409 outcome.
pub const CANCEL_ACCOUNT_DELETION_SQL: &str = r#"
WITH cancelled AS (
    DELETE FROM delete_operations
    WHERE id = $1 AND user_id = $2 AND state = 'scheduled' AND scope = 'account'
    RETURNING user_id
)
UPDATE users
SET deleting_at = NULL
FROM cancelled
WHERE users.id = cancelled.user_id
RETURNING users.id
"#;

/// Start erasure only after the grace deadline; this CAS competes with cancel.
///
/// `$1` is the operation UUID. The caller treats a zero-row result as a lost
/// race or an operation whose deadline has not elapsed (ADR 0002 rule 3).
pub const START_ACCOUNT_ERASURE_SQL: &str = r#"
UPDATE delete_operations
SET state = 'running', heartbeat_at = now()
WHERE id = $1 AND scope = 'account' AND state = 'scheduled' AND erase_after <= now()
RETURNING id
"#;

/// Return an operation only to its owning user (`$1` operation UUID, `$2` user id).
pub const OPERATION_STATUS_SQL: &str = r#"
SELECT id, scope, container_id, cursor, state, erase_after,
       heartbeat_at, consecutive_failures, last_error, started_at
FROM delete_operations
WHERE id = $1 AND user_id = $2
"#;

/// Result of the account-deletion cancellation compare-and-set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The scheduled row was claimed by executor start (or was not cancellable).
    ErasureStarted,
    /// Cancellation removed the scheduled operation and cleared its fence.
    Cancelled,
}

impl CancelOutcome {
    /// HTTP status for the later cancel-deletion route.
    pub const fn http_status(self) -> u16 {
        match self {
            Self::ErasureStarted => 409,
            Self::Cancelled => 200,
        }
    }
}

/// Map the affected-row count of the cancellation CAS to its stable outcome.
pub const fn cancel_outcome(affected_rows: u64) -> CancelOutcome {
    if affected_rows == 0 {
        CancelOutcome::ErasureStarted
    } else {
        CancelOutcome::Cancelled
    }
}

// --- Writer fence (ADR 0002 rule 3) --------------------------------------------
//
// Every writer that can create or restore user-owned state takes
// `SELECT ... FOR SHARE` on the owner's `users` row and rejects a non-NULL
// `deleting_at`, inside the same statement/transaction as its write, so the
// scheduling `UPDATE users SET deleting_at = ...` (which needs the row lock)
// either waits for an in-flight writer or fences every later one. Auth/session
// writers needed to cancel during grace are exempt only while the account
// operation is `state = 'scheduled'`; the executor's own cursor/state writes
// are exempt outright. Everything else fails closed with the stable
// `account_deleting` code (HTTP 409) and never queues behind the delete.

/// Stable wire/job code for a write rejected by the account-deletion fence.
pub const ACCOUNT_DELETING_CODE: &str = "account_deleting";

/// Probe the fence state of one user (`$1` TEXT user id) without locking.
///
/// Writers use this only to classify a fenced write that affected zero rows
/// (row vanished vs. fence closed); it is never the fence itself. Zero rows
/// means the user no longer exists.
pub const ACCOUNT_DELETING_PROBE_SQL: &str = r#"
SELECT (deleting_at IS NOT NULL) AS deleting FROM users WHERE id = $1
"#;

/// Lock a non-deleting user row `FOR SHARE` for the rest of the transaction.
///
/// `$1` is the TEXT user id. Returns one row only when the fence is open;
/// zero rows means the user is deleting (or gone) and the writer must fail
/// with [`ACCOUNT_DELETING_CODE`]. Single-statement writers embed the same
/// subquery (`FROM (SELECT id FROM users WHERE id = $n AND deleting_at IS
/// NULL FOR SHARE) AS writer_fence`) instead of running this separately, so
/// the fence and the write stay atomic under transaction-mode pooling.
pub const WRITER_FENCE_USER_SQL: &str = r#"
SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE
"#;

/// Outcome of a fenced write after combining the write's affected-row count
/// with an [`ACCOUNT_DELETING_PROBE_SQL`] classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FencedWriteOutcome {
    /// The write landed; the fence was open for its whole statement.
    Written,
    /// The fence is closed: deletion is scheduled for this account.
    AccountDeleting,
    /// No row was written and the fence is open (target row vanished or
    /// never existed); callers map this to their usual not-found outcome.
    NoRow,
}

/// Classify a fenced write. `account_deleting` is the probe result
/// (`None` when the user row no longer exists); it is consulted only when
/// the write affected zero rows, so a landed write is never reclassified.
pub const fn classify_fenced_write(
    affected_rows: u64,
    account_deleting: Option<bool>,
) -> FencedWriteOutcome {
    if affected_rows > 0 {
        FencedWriteOutcome::Written
    } else if matches!(account_deleting, Some(true)) {
        FencedWriteOutcome::AccountDeleting
    } else {
        FencedWriteOutcome::NoRow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalize(value: &str) -> String {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    }

    #[test]
    fn writer_fence_locks_only_a_non_deleting_user_row() {
        // The fence must take FOR SHARE (conflicts with the scheduling
        // UPDATE, not with sibling writers) and must return no row once
        // deleting_at is set; either weakening lets a write commit behind
        // the marker.
        let sql = normalize(WRITER_FENCE_USER_SQL);
        assert_eq!(
            sql, "selectidfromuserswhereid=$1anddeleting_atisnullforshare",
            "{sql}"
        );
        assert!(!sql.contains("forupdate"), "{sql}");
        assert!(!sql.contains("nowait"), "{sql}");
    }

    #[test]
    fn probe_reads_the_marker_without_locking() {
        // The classifier probe must never take the fence itself: a locking
        // probe would queue behind the delete it is only classifying.
        let sql = normalize(ACCOUNT_DELETING_PROBE_SQL);
        assert_eq!(
            sql, "select(deleting_atisnotnull)asdeletingfromuserswhereid=$1",
            "{sql}"
        );
        assert!(!sql.contains("forshare"), "{sql}");
        assert!(!sql.contains("forupdate"), "{sql}");
    }

    #[test]
    fn fenced_write_classification_is_stable() {
        // A landed write is never reclassified by a later probe.
        for probe in [Some(true), Some(false), None] {
            assert_eq!(
                classify_fenced_write(1, probe),
                FencedWriteOutcome::Written,
                "{probe:?}"
            );
        }
        // Zero rows + a set marker is the typed fence rejection; zero rows
        // with the fence open (or the user gone) stays not-found.
        assert_eq!(
            classify_fenced_write(0, Some(true)),
            FencedWriteOutcome::AccountDeleting
        );
        assert_eq!(
            classify_fenced_write(0, Some(false)),
            FencedWriteOutcome::NoRow
        );
        assert_eq!(classify_fenced_write(0, None), FencedWriteOutcome::NoRow);
        assert_eq!(ACCOUNT_DELETING_CODE, "account_deleting");
    }
}
