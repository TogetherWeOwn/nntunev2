//! Library writes: one row per idempotency key, via Hyperdrive.
//!
//! The `library` table is keyed `UNIQUE(idempotency_key)` so Queue redelivery
//! and Workflow replay converge instead of duplicating. This module owns the
//! parameterized statement and its validation; the worker executes it through
//! the `LIBRARY_DB` Hyperdrive binding (see `wrangler.toml.example`).
//!
//! **Enablement blocker:** no migration creates `library` (or its
//! `UNIQUE(idempotency_key)`) yet — `migrations/0003_library.sql` defines
//! only `genres`/`playlists`, and ADR 0002 directs the follow-up to inspect
//! staging, add normalized tables, and change this writer rather than
//! assume the flat table exists. The Queue consumer stays disabled until
//! that schema lands; if this statement ever runs against a database
//! without the table, Postgres answers a deterministic SQLSTATE (42P01 /
//! 42P10), which [`is_deterministic_sqlstate`] maps to a permanent failure
//! so the job quarantines at once instead of burning its retry budget as
//! "database unavailable".

use crate::jobs::validate_job_id;
use crate::queue::{validate_mbid, LibraryWrite};
use crate::retry::{JobError, ERR_ACCOUNT_DELETING};

/// Hyperdrive binding name: must match `[hyperdrive]` in
/// `wrangler.toml.example`.
pub const LIBRARY_DB_BINDING: &str = "LIBRARY_DB";

/// `$n` parameter order for [`upsert_library_sql`].
pub const UPSERT_PARAM_ORDER: [&str; 5] = [
    "job_id",
    "user_id",
    "idempotency_key",
    "release_mbid",
    "recording_mbids",
];

/// Idempotent library write. `DO UPDATE` (not `DO NOTHING`) so a replay that
/// refined the MusicBrainz match converges on the latest match instead of
/// keeping the first write; same-key/same-content replays are no-ops by
/// construction. `$4` is nullable text, `$5` is `text[]`.
///
/// Account-deletion fence (ADR 0002 rule 3): the row is selected through
/// `writer_fence`, which takes the job owner's `users` row `FOR SHARE`
/// only while `deleting_at IS NULL`. The write therefore cannot
/// (re)populate library state after deletion is scheduled, and the
/// scheduling `UPDATE users` either waits for this statement or fences
/// it. A fenced write affects 0 rows; the consumer maps that through
/// [`upsert_outcome`] to the permanent `account_deleting` failure. The
/// fence joins the same `LIBRARY_DB` database that owns `users` (the
/// fence takes no stance on the still-missing `library` table: with the
/// table absent the statement still fails deterministically before any
/// row is produced).
pub fn upsert_library_sql() -> &'static str {
    "INSERT INTO library (job_id, user_id, idempotency_key, release_mbid, recording_mbids) \
     SELECT $1, $2, $3, $4, $5 \
     FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence \
     ON CONFLICT (idempotency_key) DO UPDATE SET \
     job_id = EXCLUDED.job_id, \
     release_mbid = EXCLUDED.release_mbid, \
     recording_mbids = EXCLUDED.recording_mbids"
}

/// Outcome of a fenced [`upsert_library_sql`] write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// The row landed (inserted or converged onto the idempotency key)
    /// while the owner's fence was open.
    Written,
    /// The write affected 0 rows: the owner's account is being deleted
    /// (or the user row is gone), so the fence refused it.
    AccountDeleting,
}

/// Classify a library upsert by its affected-row count.
///
/// The statement only ever affects 0 rows through the fence: the
/// conflict path is `DO UPDATE` (a converged replay still reports 1),
/// and `$2` is the already-validated job owner, so a missing fence row
/// means `deleting_at` is set (or the account was erased between job
/// acceptance and write). Both are the same permanent outcome.
pub const fn upsert_outcome(affected_rows: u64) -> UpsertOutcome {
    if affected_rows == 0 {
        UpsertOutcome::AccountDeleting
    } else {
        UpsertOutcome::Written
    }
}

/// Map a fenced [`upsert_library_sql`] affected-row count to the consumer
/// result: 0 rows is the permanent `account_deleting` failure, anything
/// else is the committed write. Pure so the 0-row to permanent-code
/// mapping is pinned without a live Env; the edge executor just forwards
/// this (replacing the arm with `Ok(())` — the phantom `Done` — must fail
/// the pin test below).
pub fn upsert_job_result(affected_rows: u64) -> Result<(), JobError> {
    match upsert_outcome(affected_rows) {
        UpsertOutcome::Written => Ok(()),
        UpsertOutcome::AccountDeleting => Err(JobError::permanent(
            ERR_ACCOUNT_DELETING,
            "account is being deleted",
        )),
    }
}

/// One upsert's bound values, in [`UPSERT_PARAM_ORDER`] (`$1`..=`$5`)
/// order. The edge executor binds exactly these values in field order;
/// keeping the projection here lets the order be unit-tested without a
/// database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertParams<'a> {
    pub job_id: &'a str,
    pub user_id: &'a str,
    pub idempotency_key: &'a str,
    pub release_mbid: Option<&'a str>,
    pub recording_mbids: &'a [String],
}

/// Project a validated [`LibraryWrite`] into its bound parameters.
/// Replaying the same write always yields the same parameters, which is
/// what makes the `ON CONFLICT DO UPDATE` replay a no-op.
pub fn upsert_params(write: &LibraryWrite) -> UpsertParams<'_> {
    UpsertParams {
        job_id: &write.job_id,
        user_id: &write.user_id,
        idempotency_key: &write.idempotency_key,
        release_mbid: write.release_mbid.as_deref(),
        recording_mbids: &write.recording_mbids,
    }
}

/// Whether a Postgres SQLSTATE is deterministic for this statement: the
/// same upsert replayed unchanged will fail the same way, so retrying
/// only burns the delivery budget. Covers class 42 (syntax / access rule
/// violation — including 42P01 undefined table and 42P10 invalid
/// `ON CONFLICT` target, i.e. the missing-`library`-table case), class 22
/// (data exception) and class 23 (integrity constraint violation).
/// Anything else (connection class 08, operator intervention 57, no
/// SQLSTATE at all, …) may succeed on redelivery and stays transient.
pub fn is_deterministic_sqlstate(sqlstate: Option<&str>) -> bool {
    matches!(
        sqlstate.and_then(|code| code.get(..2)),
        Some("42" | "22" | "23")
    )
}

/// Check a [`LibraryWrite`] before it touches the database.
pub fn validate_library_write(write: &LibraryWrite) -> Result<(), &'static str> {
    if !validate_job_id(&write.job_id) {
        return Err("job id must be 1..=64 url-safe chars");
    }
    if write.user_id.is_empty() || write.user_id.len() > 128 {
        return Err("user id must be 1..=128 chars");
    }
    if write.idempotency_key.len() > 640
        || !write.idempotency_key.starts_with("ingest:")
        || write.idempotency_key.bytes().filter(|b| *b == b':').count() < 2
    {
        return Err("idempotency key must be an ingest key");
    }
    if let Some(ref mbid) = write.release_mbid {
        if !validate_mbid(mbid) {
            return Err("release mbid must be canonical 8-4-4-4-12 hex");
        }
    }
    if write.recording_mbids.len() > 10_000 {
        return Err("too many recording mbids");
    }
    if write.recording_mbids.iter().any(|m| !validate_mbid(m)) {
        return Err("recording mbids must be canonical 8-4-4-4-12 hex");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::LibraryWrite;

    fn write() -> LibraryWrite {
        LibraryWrite {
            job_id: "job_0123456789abcdef".into(),
            user_id: "user_01".into(),
            idempotency_key: format!("ingest:user_01:{}:upload", "a".repeat(64)),
            release_mbid: Some("f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f".into()),
            recording_mbids: vec!["f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f".into()],
        }
    }

    #[test]
    fn upsert_converges_on_idempotency_key() {
        let sql = upsert_library_sql();
        assert!(sql.contains("ON CONFLICT (idempotency_key)"));
        assert!(sql.contains("DO UPDATE SET"));
        for i in 1..=5 {
            assert!(sql.contains(&format!("${i}")), "missing placeholder ${i}");
        }
        assert_eq!(UPSERT_PARAM_ORDER.len(), 5);
    }

    #[test]
    fn upsert_is_fenced_on_the_owner_row_in_statement() {
        // ADR 0002 rule 3: the write selects its row through a FOR SHARE
        // fence on the owner's users row. Naming the fence parts so
        // dropping the marker check, the lock, or the in-statement join
        // (e.g. degenerating to WHERE EXISTS without locking, which the
        // scheduling UPDATE could slip behind) fails here.
        let sql = upsert_library_sql();
        assert!(
            sql.contains(
                "SELECT $1, $2, $3, $4, $5 FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence"
            ),
            "{sql}"
        );
        // The fence binds the owner parameter ($2), never the job id ($1).
        assert!(!sql.contains("FROM users WHERE id = $1"), "{sql}");
        assert!(!sql.contains("WHERE id = $1 AND deleting_at"), "{sql}");
        // FOR SHARE only: FOR UPDATE would serialize sibling ingest
        // writers for one user; NOWAIT/SKIP could read as "not deleting".
        assert!(!sql.contains("FOR UPDATE"), "{sql}");
        assert!(!sql.contains("NOWAIT"), "{sql}");
        assert!(!sql.contains("SKIP LOCKED"), "{sql}");
        assert!(!sql.contains("deleting_at IS NOT NULL"), "{sql}");
        // Convergence survives the fence: the conflict clause still
        // follows the fenced SELECT, keyed on the idempotency key.
        let fence_at = sql.find("writer_fence").unwrap();
        let conflict_at = sql.find("ON CONFLICT (idempotency_key)").unwrap();
        assert!(fence_at < conflict_at, "{sql}");
    }

    #[test]
    fn upsert_outcome_only_zero_rows_is_the_fence_refusal() {
        assert_eq!(upsert_outcome(0), UpsertOutcome::AccountDeleting);
        assert_eq!(upsert_outcome(1), UpsertOutcome::Written);
    }

    #[test]
    fn upsert_job_result_pins_the_fenced_zero_row_to_permanent_account_deleting() {
        // The edge arm maps through this function, so weakening it to
        // `Ok(())` (the phantom `Done`) must fail here.
        assert!(upsert_job_result(1).is_ok());
        assert!(upsert_job_result(u64::MAX).is_ok());
        let err = upsert_job_result(0).expect_err("0 rows is the fence refusal");
        assert_eq!(err.code, ERR_ACCOUNT_DELETING);
        assert!(
            !err.transient,
            "a fenced writer never queues behind the delete"
        );
    }

    #[test]
    fn validation_accepts_unmatched_and_matched_writes() {
        assert!(validate_library_write(&write()).is_ok());
        let mut unmatched = write();
        unmatched.release_mbid = None;
        unmatched.recording_mbids.clear();
        assert!(validate_library_write(&unmatched).is_ok());
    }

    #[test]
    fn validation_rejects_bad_keys_and_mbids() {
        let mut w = write();
        w.idempotency_key.clear();
        assert!(validate_library_write(&w).is_err());
        w = write();
        w.idempotency_key = "not-our-key".into();
        assert!(validate_library_write(&w).is_err());
        w = write();
        w.release_mbid = Some("not-an-mbid".into());
        assert!(validate_library_write(&w).is_err());
        w = write();
        w.recording_mbids = vec!["zzz".into()];
        assert!(validate_library_write(&w).is_err());
        w = write();
        w.job_id.clear();
        assert!(validate_library_write(&w).is_err());
    }

    #[test]
    fn upsert_params_follow_the_declared_order() {
        let w = write();
        let params = upsert_params(&w);
        // Field order is the `$n` binding order in UPSERT_PARAM_ORDER.
        let bound = [
            params.job_id,
            params.user_id,
            params.idempotency_key,
            params.release_mbid.unwrap(),
        ];
        assert_eq!(
            bound,
            [
                w.job_id.as_str(),
                w.user_id.as_str(),
                w.idempotency_key.as_str(),
                w.release_mbid.as_deref().unwrap(),
            ]
        );
        assert_eq!(params.recording_mbids, w.recording_mbids.as_slice());
        assert_eq!(
            UPSERT_PARAM_ORDER,
            [
                "job_id",
                "user_id",
                "idempotency_key",
                "release_mbid",
                "recording_mbids"
            ]
        );
        // An unmatched write binds NULL release and an empty recording list.
        let mut unmatched = write();
        unmatched.release_mbid = None;
        unmatched.recording_mbids.clear();
        let params = upsert_params(&unmatched);
        assert_eq!(params.release_mbid, None);
        assert!(params.recording_mbids.is_empty());
    }

    #[test]
    fn upsert_params_replay_is_identical() {
        let w = write();
        assert_eq!(upsert_params(&w), upsert_params(&w.clone()));
    }

    #[test]
    fn deterministic_sqlstate_classes_fail_fast() {
        // Missing table / missing unique constraint (the enablement
        // blocker), data exceptions and integrity violations replay
        // identically, so they must not be retried as "unavailable".
        for code in ["42P01", "42P10", "42501", "22001", "22P02", "23505"] {
            assert!(is_deterministic_sqlstate(Some(code)), "{code}");
        }
        // Connection/operator classes and a missing SQLSTATE may succeed
        // on redelivery.
        for code in [Some("08006"), Some("57P03"), Some(""), Some("4"), None] {
            assert!(!is_deterministic_sqlstate(code), "{code:?}");
        }
    }

    #[test]
    fn binding_name_matches_wrangler_example() {
        assert_eq!(LIBRARY_DB_BINDING, "LIBRARY_DB");
    }
}
