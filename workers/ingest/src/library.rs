//! Library writes: one row per idempotency key, via Hyperdrive.
//!
//! The `library` table is keyed `UNIQUE(idempotency_key)` so Queue redelivery
//! and Workflow replay converge instead of duplicating. This module owns the
//! parameterized statement and its validation; the worker executes it through
//! the `LIBRARY_DB` Hyperdrive binding (see `wrangler.toml.example`).

use crate::jobs::validate_job_id;
use crate::queue::{validate_mbid, LibraryWrite};

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
pub fn upsert_library_sql() -> &'static str {
    "INSERT INTO library (job_id, user_id, idempotency_key, release_mbid, recording_mbids) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (idempotency_key) DO UPDATE SET \
     job_id = EXCLUDED.job_id, \
     release_mbid = EXCLUDED.release_mbid, \
     recording_mbids = EXCLUDED.recording_mbids"
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
    fn binding_name_matches_wrangler_example() {
        assert_eq!(LIBRARY_DB_BINDING, "LIBRARY_DB");
    }
}
