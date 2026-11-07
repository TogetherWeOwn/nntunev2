//! Internal source lookup attestation, not a public source endpoint or a new
//! source payload. Only a stream caller that verified a v2 grant may sign this
//! with the existing play-grant key. No user/play identity reaches pool services.

use serde::{Deserialize, Serialize};

use super::{canonical_track_id, valid_source_job, OriginalSource, SourceError, PAYLOAD_FORMAT};

pub const PATH: &str = "/internal/v1/original-source";
pub const MAX_LOOKUP_MS: u64 = 5000;
pub const MAX_GRANT_REMAINING_SECS: u64 = 960;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub track_id: String,
    pub grant_expires_at: u64,
    pub issued_at_ms: u64,
    pub deadline_ms: u64,
}

impl Lookup {
    pub fn valid_at(&self, now_ms: u64) -> bool {
        canonical_track_id(&self.track_id)
            && now_ms >= self.issued_at_ms
            && now_ms < self.deadline_ms
            && self.deadline_ms > self.issued_at_ms
            && self.deadline_ms - self.issued_at_ms <= MAX_LOOKUP_MS
            && self.deadline_ms.div_ceil(1000) <= self.grant_expires_at
            && self.grant_expires_at > now_ms / 1000
            && self.grant_expires_at - now_ms / 1000 <= MAX_GRANT_REMAINING_SECS
    }
}

/// Only an authenticated API service binding may return this. The job identity
/// is selected by API's persisted row, not by a public caller. Bound headers and
/// body while reading, refuse redirects/non-200, never cache or log either.
/// No envelope key, provider identity or HTTP response implementation lives here.
pub struct Payload {
    pub source_job_id: String,
    pub bytes: Vec<u8>,
}

impl Payload {
    pub fn parse(&self, track_id: &str) -> Result<OriginalSource, SourceError> {
        if !valid_source_job(&self.source_job_id) {
            return Err(SourceError::Binding);
        }
        OriginalSource::parse(PAYLOAD_FORMAT, &self.bytes, track_id, &self.source_job_id)
    }
}
