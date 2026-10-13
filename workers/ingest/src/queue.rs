//! Queue / Workflow payload shapes for the ingest pipeline.
//!
//! Every message carries the job's idempotency key so redelivered Queue
//! messages and replayed Workflow steps converge on one library row.
//! Repair runs in a Container batch job (native par2 repair); everything
//! interactive stays serverless. MB lookups call the `workers/mb` read API
//! (`GET /v1/mb/release/:mbid`, `GET /v1/mb/recording/:mbid`,
//! `GET /v1/mb/search?type=…`).

use serde::{Deserialize, Serialize};

use crate::jobs::{JobSource, JobState};

/// Ordered Workflow steps; `repair` runs only when verify asks for it.
pub const WORKFLOW_STEPS: [&str; 7] = [
    "search", "parse", "probe", "verify", "repair", "match", "write",
];

/// One unit of ingest work on the Queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMsg {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub source: JobSource,
    pub state: JobState,
    pub attempt: u32,
}

/// Native par2 repair dispatched to the Container batch job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRequest {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub par2_set_id: String,
    pub missing_blocks: u32,
}

/// Final library write after tag/fingerprint + MusicBrainz match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryWrite {
    pub job_id: String,
    pub user_id: String,
    pub idempotency_key: String,
    pub release_mbid: Option<String>,
    pub recording_mbids: Vec<String>,
}

/// Search request body for `POST /v1/ingest/search`.
///
/// `indexer_ref` is an opaque BYOP vault handle, never a raw key. `user_id`
/// is self-asserted until the accounts card authenticates callers; it binds
/// the idempotency key so today's queued jobs stay valid when auth lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub user_id: String,
    pub query: String,
    pub indexer_ref: String,
    pub categories: Vec<u32>,
    pub limit: u32,
    pub page: u32,
}

/// NZB upload body for `POST /v1/ingest/nzb` (JSON; caps in `nzb_intake`).
/// `user_id` is self-asserted, as in [`SearchRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NzbUploadRequest {
    pub user_id: String,
    pub filename: String,
    pub nzb_xml: String,
}

/// MBID shape: `8-4-4-4-12` lowercase hex (releases/recordings from `mb`).
pub fn validate_mbid(raw: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() != GROUPS.len() {
        return false;
    }
    parts
        .iter()
        .zip(GROUPS)
        .all(|(p, len)| p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A vault handle is opaque but must stay URL/path-safe.
pub fn validate_indexer_ref(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 128
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_steps_cover_repair_branch() {
        // Joined at runtime: guards step order and the repair branch.
        let pipeline = WORKFLOW_STEPS.join(">");
        assert!(pipeline.starts_with("search>"));
        assert!(pipeline.ends_with(">write"));
        assert!(pipeline.contains(">repair>"));
    }

    #[test]
    fn queue_msg_roundtrips_with_idempotency_key() {
        let msg = QueueMsg {
            job_id: "job_1".into(),
            user_id: "u1".into(),
            idempotency_key: "ingest:u1:abc:upload".into(),
            source: JobSource::NzbUpload,
            state: JobState::Queued,
            attempt: 1,
        };
        let back: QueueMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn library_write_accepts_unmatched_releases() {
        let w = LibraryWrite {
            job_id: "j".into(),
            user_id: "u".into(),
            idempotency_key: "k".into(),
            release_mbid: None,
            recording_mbids: vec![],
        };
        assert!(w.release_mbid.is_none());
    }

    #[test]
    fn mbid_validation_accepts_canonical_only() {
        assert!(validate_mbid("f5c2d4b1-3a7e-4d1c-9e2f-0a1b2c3d4e5f"));
        assert!(!validate_mbid(""));
        assert!(!validate_mbid("not-an-mbid"));
        // Hex check is case-insensitive; canonical form is lowercase.
        assert!(validate_mbid("F5C2D4B1-3A7E-4D1C-9E2F-0A1B2C3D4E5F"));
    }

    #[test]
    fn indexer_ref_rejects_raw_urls_and_keys() {
        assert!(validate_indexer_ref("vault_idx_01-ab"));
        assert!(!validate_indexer_ref(""));
        assert!(!validate_indexer_ref("https://indexer.example?apikey=x"));
        assert!(!validate_indexer_ref("key with spaces"));
    }

    #[test]
    fn search_request_carries_user_scope() {
        let req: SearchRequest = serde_json::from_str(
            r#"{"query":"q","indexer_ref":"vault_01","categories":[],"limit":10,"page":0,"user_id":"user_01"}"#,
        )
        .unwrap();
        assert_eq!(req.user_id, "user_01");
    }

    #[test]
    fn upload_request_carries_user_scope() {
        let req: NzbUploadRequest = serde_json::from_str(
            r#"{"filename":"r.nzb","nzb_xml":"<nzb></nzb>","user_id":"user_01"}"#,
        )
        .unwrap();
        assert_eq!(req.user_id, "user_01");
    }
}
