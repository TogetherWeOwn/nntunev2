//! Enqueue-time dispatch: build [`QueueMsg`] values with user-bound idempotency.
//!
//! Routes validate the contract, then call the builders here and `send` the
//! result to the `INGEST_QUEUE` binding (see `wrangler.toml.example`). The
//! builders are pure so the idempotency story — same user + same content +
//! same source ref converges on one job — is unit-tested, not mocked.
//!
//! `user_id` is self-asserted until the accounts card authenticates callers;
//! the key format already binds the user so today's keys stay valid when
//! auth lands.

use sha2::{Digest, Sha256};

use crate::jobs::{derive_idempotency_key, JobSource, JobState};
use crate::nzb_intake::estimate_nzb;
use crate::queue::{validate_indexer_ref, QueueMsg};

/// Queue binding name: must match `[[queues.producers]]` in
/// `wrangler.toml.example`.
pub const INGEST_QUEUE_BINDING: &str = "INGEST_QUEUE";
/// Queue name: must match `[[queues.consumers]]` in `wrangler.toml.example`.
pub const INGEST_QUEUE_NAME: &str = "nntune-ingest-jobs";

/// Self-asserted user handles are 1..=128 chars; the accounts card replaces
/// this with the authenticated subject without changing the key format.
pub fn validate_user_id(raw: &str) -> bool {
    !raw.is_empty() && raw.len() <= 128
}

/// Lowercase sha256 hex of canonical content bytes.
pub fn content_sha256_hex(canonical: &[u8]) -> String {
    hex::encode(Sha256::digest(canonical))
}

/// Deterministic job handle: `job_` + 16 hex chars of FNV-1a64 over the
/// idempotency material, so redelivered enqueues converge on one job row.
/// Provisional until Hyperdrive assigns row ids; always URL-safe.
pub fn make_job_id(user_id: &str, content_sha256_hex: &str, source_ref: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in user_id
        .bytes()
        .chain(content_sha256_hex.bytes())
        .chain(source_ref.bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("job_{h:016x}")
}

fn dispatch(
    user_id: &str,
    content_sha: &str,
    source_ref: &str,
    source: JobSource,
) -> Result<QueueMsg, &'static str> {
    if !validate_user_id(user_id) {
        return Err("user id must be 1..=128 chars");
    }
    let idempotency_key = derive_idempotency_key(user_id, content_sha, source_ref)?;
    Ok(QueueMsg {
        job_id: make_job_id(user_id, content_sha, source_ref),
        user_id: user_id.to_string(),
        idempotency_key,
        source,
        state: JobState::Queued,
        attempt: 1,
    })
}

/// Build the enqueue message for `POST /v1/ingest/search`.
///
/// The GUID is unknown until the workflow's `Searching` step runs, so the
/// source ref binds the indexer handle; the workflow re-keys on the GUID
/// before `Parsing`.
pub fn build_search_dispatch(
    user_id: &str,
    query: &str,
    indexer_ref: &str,
) -> Result<QueueMsg, &'static str> {
    let q = query.trim();
    if q.is_empty() || q.len() > 200 {
        return Err("query must be 1..=200 chars");
    }
    if !validate_indexer_ref(indexer_ref) {
        return Err("indexer_ref must be a vault handle");
    }
    let canonical = format!("search:{indexer_ref}:{q}");
    let sha = content_sha256_hex(canonical.as_bytes());
    dispatch(
        user_id,
        &sha,
        &format!("search:{indexer_ref}"),
        JobSource::IndexerSearch,
    )
}

/// Build the enqueue message for `POST /v1/ingest/nzb`.
///
/// Runs the intake guard first so oversize/non-NZB bytes never reach the
/// Queue, let alone provider connections.
pub fn build_upload_dispatch(user_id: &str, nzb_xml: &str) -> Result<QueueMsg, &'static str> {
    estimate_nzb(nzb_xml).map_err(|_| "nzb_xml failed the intake guard")?;
    let sha = content_sha256_hex(nzb_xml.as_bytes());
    dispatch(user_id, &sha, "upload", JobSource::NzbUpload)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "user_01";
    const INDEXER: &str = "vault_idx_01-ab";

    #[test]
    fn search_dispatch_binds_user_content_and_indexer() {
        let a = build_search_dispatch(USER, "dark side", INDEXER).unwrap();
        let b = build_search_dispatch(USER, "dark side", INDEXER).unwrap();
        assert_eq!(a, b);
        assert!(a.idempotency_key.starts_with("ingest:user_01:"));
        assert!(a.job_id.starts_with("job_"));
        assert_eq!(a.state, crate::jobs::JobState::Queued);
        assert_eq!(a.attempt, 1);
        assert_eq!(a.source, crate::jobs::JobSource::IndexerSearch);
    }

    #[test]
    fn search_dispatch_separates_users_and_queries() {
        let a = build_search_dispatch(USER, "dark side", INDEXER).unwrap();
        let other_user = build_search_dispatch("user_02", "dark side", INDEXER).unwrap();
        let other_query = build_search_dispatch(USER, "kind of blue", INDEXER).unwrap();
        assert_ne!(a.idempotency_key, other_user.idempotency_key);
        assert_ne!(a.idempotency_key, other_query.idempotency_key);
        assert_ne!(a.job_id, other_user.job_id);
    }

    #[test]
    fn search_dispatch_rejects_bad_inputs() {
        assert!(build_search_dispatch("", "q", INDEXER).is_err());
        assert!(build_search_dispatch(USER, "", INDEXER).is_err());
        assert!(build_search_dispatch(USER, &"x".repeat(201), INDEXER).is_err());
        assert!(build_search_dispatch(USER, "q", "https://indexer.example?apikey=x").is_err());
        assert!(build_search_dispatch(USER, "q", "").is_err());
    }

    #[test]
    fn upload_dispatch_binds_nzb_bytes() {
        let nzb = crate::fixtures::SMALL_NZB;
        let a = build_upload_dispatch(USER, nzb).unwrap();
        let b = build_upload_dispatch(USER, nzb).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.source, crate::jobs::JobSource::NzbUpload);
        assert!(a.idempotency_key.starts_with("ingest:user_01:"));
        assert!(a.idempotency_key.ends_with(":upload"));
    }

    #[test]
    fn upload_dispatch_refuses_unguarded_bytes() {
        assert!(build_upload_dispatch(USER, "").is_err());
        assert!(build_upload_dispatch(USER, "<html>not nzb</html>").is_err());
        assert!(build_upload_dispatch("", crate::fixtures::SMALL_NZB).is_err());
    }

    #[test]
    fn job_id_is_url_safe_and_stable() {
        let sha = "a".repeat(64);
        let a = make_job_id(USER, &sha, "upload");
        let b = make_job_id(USER, &sha, "upload");
        assert_eq!(a, b);
        assert!(a.starts_with("job_"));
        assert!(crate::jobs::validate_job_id(&a));
        assert_ne!(a, make_job_id("user_02", &sha, "upload"));
    }

    #[test]
    fn binding_names_match_wrangler_example() {
        assert_eq!(INGEST_QUEUE_BINDING, "INGEST_QUEUE");
        assert_eq!(INGEST_QUEUE_NAME, "nntune-ingest-jobs");
    }
}
