//! Content-bound contributions from successful BODY operations, never STAT
//! observations or queue hints. Only trusted pipeline scratch/checkpoints carry
//! these records: the hash detects stale bytes, not forged attribution. This is
//! provenance evidence, not a cache grant or a proof of entitlement.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::dispatch::content_sha256_hex;
use nntune_core::pool_operation::valid_provider_id;

/// Keeps the serialized record within R2's custom-metadata budget. An artifact
/// with more contributors remains usable but has no complete provenance.
pub const MAX_CONTRIBUTORS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolProvenance {
    sha256: String,
    size: u64,
    provider_ids: BTreeSet<String>,
}

impl PoolProvenance {
    pub fn from_body(provider_id: &str, decoded: &[u8]) -> Option<Self> {
        if !valid_provider_id(provider_id) || decoded.is_empty() {
            return None;
        }
        Some(Self {
            sha256: content_sha256_hex(decoded),
            size: decoded.len() as u64,
            provider_ids: [provider_id.to_owned()].into(),
        })
    }

    pub fn matches_identity(&self, sha256: &str, size: u64) -> bool {
        self.size != 0
            && self.size == size
            && self.sha256 == sha256
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !self.provider_ids.is_empty()
            && self.provider_ids.len() <= MAX_CONTRIBUTORS
            && self.provider_ids.iter().all(|id| valid_provider_id(id))
    }

    pub fn matches_bytes(&self, bytes: &[u8]) -> bool {
        self.matches_identity(&content_sha256_hex(bytes), bytes.len() as u64)
    }

    pub fn provider_ids(&self) -> &BTreeSet<String> {
        &self.provider_ids
    }
}

/// Incremental evidence for an assembled file. The caller feeds every actual
/// scratch read in order; a gap, legacy record, changed byte or contributor
/// overflow permanently removes completeness for this assembly.
#[derive(Default)]
pub(crate) struct AssemblyProvenance {
    provider_ids: BTreeSet<String>,
    size: u64,
    incomplete: bool,
}

impl AssemblyProvenance {
    pub fn push(&mut self, proof: Option<&PoolProvenance>, bytes: &[u8]) {
        let Some(proof) = proof.filter(|proof| proof.matches_bytes(bytes)) else {
            self.incomplete = true;
            return;
        };
        self.size = self.size.saturating_add(bytes.len() as u64);
        for id in &proof.provider_ids {
            if self.provider_ids.contains(id) {
                continue;
            }
            if self.provider_ids.len() == MAX_CONTRIBUTORS {
                self.incomplete = true;
                return;
            }
            self.provider_ids.insert(id.clone());
        }
    }

    pub fn finish(self, bytes: &[u8]) -> Option<PoolProvenance> {
        if self.incomplete || self.size != bytes.len() as u64 || self.provider_ids.is_empty() {
            return None;
        }
        Some(PoolProvenance {
            sha256: content_sha256_hex(bytes),
            size: self.size,
            provider_ids: self.provider_ids,
        })
    }
}
