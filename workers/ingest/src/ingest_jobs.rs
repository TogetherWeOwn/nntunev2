//! `ingest_jobs` writer: admission row + terminal transition (migration 0022).
//!
//! One row per pipeline job, keyed by the deterministic `job_id`
//! (`dispatch::make_job_id`) with `UNIQUE(user_id, idempotency_key)`
//! as the replay guard. The admission route writes `queued` when a job
//! is accepted; the consumer moves it to `done`/`failed` exactly once
//! at terminal settlement. `running` is reserved for a future
//! lease/heartbeat and is never written by either path today.
//!
//! Both statements select through the standard `writer_fence` (the
//! owner's `users` row `FOR SHARE` while `deleting_at IS NULL`, ADR
//! 0002 rule 3), so no job row appears or advances after deletion is
//! scheduled. Failures map through
//! [`is_deterministic_sqlstate`](crate::library::is_deterministic_sqlstate):
//! a missing table or a constraint violation replays identically and
//! quarantines at once instead of burning the delivery budget.
//!
//! Execution (the admission-route upsert and the consumer terminal
//! transition through the shared `LIBRARY_DB` client) lands with the
//! writers-wiring follow-up; this module pins the statements, the
//! parameter orders, and the zero-row classification that wiring uses.

use crate::jobs::validate_job_id;

/// Latest bindable `accepted_at`: mirrors the migration `CHECK`
/// (`accepted_at <= 253402300799`), so a caller clock fault fails here
/// instead of as a 23514 at the database.
pub const MAX_ACCEPTED_AT_SECS: i64 = 253402300799;

/// Longest bindable idempotency key: mirrors the migration `CHECK`
/// (`char_length(idempotency_key) BETWEEN 1 AND 768`).
pub const MAX_ADMIT_KEY_BYTES: usize = 768;

/// `$n` parameter order for [`upsert_ingest_job_sql`]. The admitted
/// status is a SQL literal (`'queued'`), not a parameter.
pub const INGEST_JOB_ADMIT_PARAM_ORDER: [&str; 4] =
    ["id", "user_id", "idempotency_key", "accepted_at"];

/// Admission write: the job row in `queued`. `ON CONFLICT (id) DO
/// NOTHING` — a replay carries the same deterministic id, so every
/// redelivery converges. The conflict target is deliberately the PK,
/// not the `(user_id, idempotency_key)` unique pair: a mismatched id
/// for one key is a pipeline bug, and `DO NOTHING` on the pair would
/// converge the pipeline onto a row for a different job instead of
/// raising the deterministic 23505 that quarantines it. `accepted_at`
/// is BIGINT unix seconds bound by the caller.
pub fn upsert_ingest_job_sql() -> &'static str {
    "INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status) SELECT $1, $2, $3, $4, 'queued' FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (id) DO NOTHING"
}

/// Disambiguate an ignored admission insert using its immutable identity and
/// the stored owner's current deletion fence. Parameter: job `id` ($1). The
/// caller compares both stored fields; a persisted job alone cannot authorize
/// admission after deletion starts or adopt another owner's job.
pub fn select_ingest_job_identity_sql() -> &'static str {
    "SELECT ingest_jobs.user_id, ingest_jobs.idempotency_key FROM ingest_jobs JOIN users ON users.id = ingest_jobs.user_id WHERE ingest_jobs.id = $1 AND users.deleting_at IS NULL FOR SHARE OF users"
}

/// `$n` parameter order for [`transition_ingest_job_sql`].
pub const INGEST_JOB_TRANSITION_PARAM_ORDER: [&str; 3] = ["id", "user_id", "status"];

/// Terminal transition: `queued`/`running` to `done`/`failed`
/// ([`validate_transition_status`]; the schema `CHECK` backstops it).
/// The `IN ('queued', 'running')` predicate makes a converged replay
/// match 0 rows instead of rewriting a terminal status; the caller
/// separates that from a fenced or missing row with
/// [`select_ingest_job_status_sql`] plus
/// [`classify_transition_zero_rows`]. The row match repeats `$2`
/// (owner) so a job id presented under the wrong user matches nothing
/// and fails closed through that same classification.
pub fn transition_ingest_job_sql() -> &'static str {
    "UPDATE ingest_jobs SET status = $3 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE ingest_jobs.id = $1 AND ingest_jobs.user_id = $2 AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running')"
}

/// Fetch the job status after a zero-row [`transition_ingest_job_sql`]
/// write. Parameters: `id` ($1), `user_id` ($2).
pub fn select_ingest_job_status_sql() -> &'static str {
    "SELECT status FROM ingest_jobs WHERE id = $1 AND user_id = $2"
}

/// Outcome of one terminal [`transition_ingest_job_sql`] write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionOutcome {
    /// Zero rows but the row is already terminal: a redelivery
    /// converged on the settled status.
    Converged,
    /// Zero rows and the row is missing or still pre-terminal behind
    /// the fence (account deleting or erased, or the admission write
    /// never landed): permanent, never a retry.
    MissingOrFenced,
}

/// Classify a zero-row terminal transition by the stored status (or its
/// absence). Pure; the consumer owns the select. Not `const` (unlike
/// the bool classifiers): `&str` patterns need `PartialEq`, which is
/// not a const trait.
pub fn classify_transition_zero_rows(status: Option<&str>) -> TransitionOutcome {
    match status {
        Some("done") | Some("failed") => TransitionOutcome::Converged,
        _ => TransitionOutcome::MissingOrFenced,
    }
}

/// Terminal statuses only: the transition caller moves forward, never
/// back to `queued`/`running`.
pub fn validate_transition_status(raw: &str) -> bool {
    matches!(raw, "done" | "failed")
}

/// Check admission values before they touch the database. Bounds mirror
/// migration 0022 (`CHECK`s on `id`, `idempotency_key`, `accepted_at`);
/// the key shape mirrors the ingest key (`ingest:` prefix).
pub fn validate_admit_params(
    job_id: &str,
    user_id: &str,
    idempotency_key: &str,
    accepted_at: i64,
) -> Result<(), &'static str> {
    if !validate_job_id(job_id) {
        return Err("job id must be 1..=64 url-safe chars");
    }
    if user_id.is_empty() || user_id.len() > 128 {
        return Err("user id must be 1..=128 chars");
    }
    if idempotency_key.len() > MAX_ADMIT_KEY_BYTES
        || !idempotency_key.starts_with("ingest:")
        || idempotency_key.bytes().filter(|b| *b == b':').count() < 2
    {
        return Err("idempotency key must be an ingest key of 1..=768 chars");
    }
    if !(0..=MAX_ACCEPTED_AT_SECS).contains(&accepted_at) {
        return Err("accepted_at must be 0..=253402300799 unix seconds");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_upsert_shape_is_pinned() {
        assert_eq!(
            upsert_ingest_job_sql(),
            "INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status) \
             SELECT $1, $2, $3, $4, 'queued' \
             FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
             ON CONFLICT (id) DO NOTHING"
        );
        assert_eq!(
            INGEST_JOB_ADMIT_PARAM_ORDER,
            ["id", "user_id", "idempotency_key", "accepted_at"]
        );
        // The fence binds the owner ($2): dropping the marker check, the
        // lock, or the in-statement join fails here (ADR 0002 rule 3).
        let sql = upsert_ingest_job_sql();
        assert!(
            sql.contains(
                "SELECT $1, $2, $3, $4, 'queued' FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
            ),
            "{sql}"
        );
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        // Admission always queues: the status is a literal, never a
        // caller-supplied value that could admit a job as done.
        assert!(sql.contains("SELECT $1, $2, $3, $4, 'queued'"), "{sql}");
        // Conflict target is the PK, not the (user, key) unique pair: a
        // replay carries the same deterministic id, while a mismatched id
        // for one key is a pipeline bug that must raise 23505 (fail
        // fast), never converge silently on the first id.
        assert!(sql.contains("ON CONFLICT (id) DO NOTHING"), "{sql}");
        assert!(
            !sql.contains("ON CONFLICT (user_id, idempotency_key)"),
            "{sql}"
        );
    }

    #[test]
    fn terminal_transition_shape_is_pinned() {
        assert_eq!(
            transition_ingest_job_sql(),
            "UPDATE ingest_jobs SET status = $3 \
             FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
             WHERE ingest_jobs.id = $1 AND ingest_jobs.user_id = $2 AND ingest_jobs.user_id = writer_fence.id AND ingest_jobs.status IN ('queued', 'running')"
        );
        assert_eq!(
            INGEST_JOB_TRANSITION_PARAM_ORDER,
            ["id", "user_id", "status"]
        );
        // Terminal moves only forward: queued/running rows transition, a
        // converged replay matches nothing (0 rows, classified below).
        let sql = transition_ingest_job_sql();
        assert!(
            sql.contains("ingest_jobs.status IN ('queued', 'running')"),
            "{sql}"
        );
        assert_eq!(
            select_ingest_job_status_sql(),
            "SELECT status FROM ingest_jobs WHERE id = $1 AND user_id = $2"
        );
    }

    #[test]
    fn transition_status_is_done_or_failed_only() {
        assert!(validate_transition_status("done"));
        assert!(validate_transition_status("failed"));
        for bad in [
            "", "queued", "running", "DONE", "deleted", "done ", " failed",
        ] {
            assert!(!validate_transition_status(bad), "{bad}");
        }
    }

    #[test]
    fn transition_zero_rows_classify_by_stored_status() {
        // A terminal row means the replay converged; anything else (still
        // queued/running behind a fence, or no row at all) is the
        // missing-or-fenced outcome the consumer fails permanently.
        assert_eq!(
            classify_transition_zero_rows(Some("done")),
            TransitionOutcome::Converged
        );
        assert_eq!(
            classify_transition_zero_rows(Some("failed")),
            TransitionOutcome::Converged
        );
        assert_eq!(
            classify_transition_zero_rows(Some("queued")),
            TransitionOutcome::MissingOrFenced
        );
        assert_eq!(
            classify_transition_zero_rows(Some("running")),
            TransitionOutcome::MissingOrFenced
        );
        assert_eq!(
            classify_transition_zero_rows(None),
            TransitionOutcome::MissingOrFenced
        );
    }

    #[test]
    fn admission_params_are_validated_before_they_touch_the_database() {
        let key = format!("ingest:user_01:{}:upload", "a".repeat(64));
        assert!(validate_admit_params("job_0123456789abcdef", "user_01", &key, 1700000000).is_ok());
        assert!(validate_admit_params("", "user_01", &key, 1700000000).is_err());
        assert!(validate_admit_params("job_1", "", &key, 1700000000).is_err());
        assert!(validate_admit_params("job_1", &"u".repeat(129), &key, 1700000000).is_err());
        assert!(validate_admit_params("job_1", "user_01", "not-our-key", 1700000000).is_err());
        assert!(validate_admit_params("job_1", "user_01", "ingest:abc", 1700000000).is_err());
        assert!(validate_admit_params("job_1", "user_01", &"k".repeat(769), 1700000000).is_err());
        assert!(validate_admit_params("job_1", "user_01", &key, -1).is_err());
        assert!(validate_admit_params("job_1", "user_01", &key, 253402300800).is_err());
        assert!(validate_admit_params("job_1", "user_01", &key, 0).is_ok());
        assert!(validate_admit_params("job_1", "user_01", &key, MAX_ACCEPTED_AT_SECS).is_ok());
    }
}
