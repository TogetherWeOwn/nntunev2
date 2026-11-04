//! Confirmed account observations, not proof of one account serving a whole job.
//! A job may span accounts, retries, repaired/adopted scratch and old checkpoints.
//! Never project this set or the queue's legacy hint into a singular grant.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const MAX_OBSERVED_PROVIDERS: usize = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolObservations {
    pub provider_ids: BTreeSet<String>,
    pub truncated: bool,
}

impl PoolObservations {
    /// Invalid/unattributed responses are contract errors and are never recorded.
    /// A long-lived/retried job cannot grow its checkpoint without a ceiling.
    pub fn record(&mut self, provider_id: &str) -> Result<(), &'static str> {
        if !nntune_core::pool_operation::valid_provider_id(provider_id) {
            return Err("invalid serving account");
        }
        if self.provider_ids.contains(provider_id) {
            return Ok(());
        }
        if self.provider_ids.len() >= MAX_OBSERVED_PROVIDERS {
            self.truncated = true;
        } else {
            self.provider_ids.insert(provider_id.into());
        }
        Ok(())
    }

    pub fn merge(&mut self, other: &Self) {
        self.truncated |= other.truncated;
        for id in &other.provider_ids {
            // Sources are recorded here or recovered from our checkpoint. A
            // malformed recovered ID is not promoted into trusted attribution.
            if self.record(id).is_err() {
                self.truncated = true;
            }
        }
    }
}
