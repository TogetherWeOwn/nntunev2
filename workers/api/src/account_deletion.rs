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
/// extending its deadline.
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

/// Cancel only the owner's still-scheduled operation and clear its user fence.
///
/// `$1` is the operation UUID and `$2` is the TEXT user id. Deleting the
/// scheduled row is the cancellation compare-and-set; if executor start has
/// changed its state to `running`, this statement returns no row and leaves the
/// fence in place (ADR 0002 rule 3).
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
