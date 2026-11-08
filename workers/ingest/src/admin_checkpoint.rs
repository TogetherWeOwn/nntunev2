//! Strict safe-checkpoint wire format for the admin PostgreSQL authority.
//! This codec grants no execution authority: the store must bind row identity,
//! compare revision plus the whole envelope, validate transitions and recheck
//! original-derived probe geometry. Legacy KV decoding is deliberately unchanged.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::{
    admin_gates,
    jobs::{JobCheckpoint, JobSource, JobState, MAX_ATTEMPTS},
    library::valid_admin_key,
    probe::MAX_PROBE_SEGMENTS,
    queue::validate_mbid,
};

pub const MAX_CHECKPOINT_BYTES: usize = 16_384;
pub const MAX_BATCHES: usize = crate::nzb_intake::MAX_SEGMENTS.div_ceil(MAX_PROBE_SEGMENTS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    Invalid,
    Exhausted,
}

/// Expected safe identity from the selected row, never from a queue body.
/// The owner-free runtime uses an empty user identity; this does not grant
/// authority from the uploader account. Original linkage is checked by storage.
pub struct Identity<'a> {
    pub job_id: &'a str,
    pub user_id: &'a str,
    pub idempotency_key: &'a str,
}

/// No Debug or blanket Deserialize: only strict, bounded decoding constructs a
/// persisted snapshot. Revision is a SQL column, not part of the JSON envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    revision: i64,
    generation: i64,
    checkpoint: JobCheckpoint,
}

impl Snapshot {
    pub fn decode(
        encoded: &str,
        revision: i64,
        identity: &Identity<'_>,
    ) -> Result<Self, CheckpointError> {
        if encoded.len() > MAX_CHECKPOINT_BYTES || revision <= 0 {
            return Err(CheckpointError::Invalid);
        }
        let StrictValue(value) =
            serde_json::from_str(encoded).map_err(|_| CheckpointError::Invalid)?;
        let envelope = object(&value, &["schema_version", "generation", "checkpoint"], &[])?;
        if envelope["schema_version"].as_u64() != Some(1) {
            return Err(CheckpointError::Invalid);
        }
        let generation = envelope["generation"]
            .as_i64()
            .filter(|g| (1..=revision).contains(g))
            .ok_or(CheckpointError::Invalid)?;
        let checkpoint = decode_checkpoint(&envelope["checkpoint"], identity)?;
        let snapshot = Self {
            revision,
            generation,
            checkpoint,
        };
        // PostgreSQL JSONB adds a space after each colon/comma. Bound that
        // representation too, not merely the compact incoming JSON string.
        snapshot.encode()?;
        Ok(snapshot)
    }

    /// For a new admission only, never a replacement for a missing/invalid
    /// historical checkpoint or a reset of an existing revision. The caller
    /// must persist this together with the original/job transaction.
    pub fn initial(
        checkpoint: JobCheckpoint,
        identity: &Identity<'_>,
    ) -> Result<Self, CheckpointError> {
        if checkpoint.state != JobState::Queued
            || checkpoint.attempt != 1
            || checkpoint.failure_code.is_some()
            || checkpoint.failure_reason.is_some()
        {
            return Err(CheckpointError::Invalid);
        }
        let value = serde_json::to_value(&checkpoint).map_err(|_| CheckpointError::Invalid)?;
        decode_checkpoint(&value, identity)?;
        let snapshot = Self {
            revision: 1,
            generation: 1,
            checkpoint,
        };
        snapshot.encode()?;
        Ok(snapshot)
    }

    pub fn checkpoint(&self) -> &JobCheckpoint {
        &self.checkpoint
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// Checked counters only; neither a legal-transition check nor a CAS.
    /// Manual control/reconciliation rotates generation to the new revision;
    /// ordinary stage persistence retains it. Storage must commit atomically.
    pub fn successor(
        &self,
        checkpoint: JobCheckpoint,
        manual: bool,
        identity: &Identity<'_>,
    ) -> Result<Self, CheckpointError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(CheckpointError::Exhausted)?;
        let value = serde_json::to_value(&checkpoint).map_err(|_| CheckpointError::Invalid)?;
        decode_checkpoint(&value, identity)?;
        if checkpoint.job_id != self.checkpoint.job_id
            || checkpoint.user_id != self.checkpoint.user_id
            || checkpoint.idempotency_key != self.checkpoint.idempotency_key
        {
            return Err(CheckpointError::Invalid);
        }
        let snapshot = Self {
            revision,
            generation: if manual { revision } else { self.generation },
            checkpoint,
        };
        snapshot.encode()?;
        Ok(snapshot)
    }

    pub fn encode(&self) -> Result<String, CheckpointError> {
        let value = serde_json::json!({
            "schema_version": 1, "generation": self.generation, "checkpoint": self.checkpoint,
        });
        let encoded = serde_json::to_string(&value).map_err(|_| CheckpointError::Invalid)?;
        if encoded.len() + separator_spaces(&value) > MAX_CHECKPOINT_BYTES {
            return Err(CheckpointError::Invalid);
        }
        Ok(encoded)
    }

    pub fn status(&self) -> &'static str {
        match self.checkpoint.state {
            JobState::Queued => "queued",
            JobState::NeedsReview => "needs_review",
            JobState::NeedsArchiveRead => "needs_archive_read",
            JobState::Rejected => "rejected",
            JobState::Done => "done",
            JobState::Failed => "failed",
            _ => "running",
        }
    }
}

fn object<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>, CheckpointError> {
    let map = value.as_object().ok_or(CheckpointError::Invalid)?;
    if required.iter().any(|k| !map.contains_key(*k))
        || map
            .keys()
            .any(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()))
    {
        return Err(CheckpointError::Invalid);
    }
    Ok(map)
}

fn decode_checkpoint(
    value: &Value,
    identity: &Identity<'_>,
) -> Result<JobCheckpoint, CheckpointError> {
    let fields = object(
        value,
        &[
            "job_id",
            "user_id",
            "idempotency_key",
            "source",
            "state",
            "attempt",
            "repair_rounds",
            "probe_tally",
            "par2_set_id",
            "missing_blocks",
            "failure_code",
            "failure_reason",
            "provider_id",
            "transcode_inputs",
            "transcode_next",
        ],
        &["admin_release_mbid"],
    )?;
    if !fields["probe_tally"].is_null() {
        let tally = object(
            &fields["probe_tally"],
            &["total_batches", "missing_by_batch"],
            &[],
        )?;
        let total = tally["total_batches"]
            .as_u64()
            .filter(|n| (1..=MAX_BATCHES as u64).contains(n))
            .ok_or(CheckpointError::Invalid)?;
        let counts = tally["missing_by_batch"]
            .as_object()
            .ok_or(CheckpointError::Invalid)?;
        if counts.len() > total as usize
            || counts.iter().any(|(index, count)| {
                index
                    .parse::<u64>()
                    .ok()
                    .is_none_or(|n| n >= total || n.to_string() != *index)
                    || count.as_u64().is_none_or(|n| n > MAX_PROBE_SEGMENTS as u64)
            })
        {
            return Err(CheckpointError::Invalid);
        }
    }
    let cp: JobCheckpoint =
        serde_json::from_value(value.clone()).map_err(|_| CheckpointError::Invalid)?;
    if cp.source != JobSource::AdminUpload
        || cp.job_id != identity.job_id
        || cp.user_id != identity.user_id
        || cp.idempotency_key != identity.idempotency_key
        || cp.job_id.len() != 47
        || !cp.job_id.starts_with("adm_")
        || !crate::jobs::validate_job_id(&cp.job_id)
        || cp.user_id.len() > 128
        || !cp
            .user_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || !valid_admin_key(&cp.idempotency_key)
        || !(1..=MAX_ATTEMPTS).contains(&cp.attempt)
        || cp.repair_rounds != 0
        || cp.par2_set_id.is_some()
        || cp.missing_blocks != 0
        || cp.provider_id.is_some()
        || !cp.transcode_inputs.is_empty()
        || cp.transcode_next != 0
        || cp
            .admin_release_mbid
            .as_deref()
            .is_some_and(|id| !validate_mbid(id))
        || cp.failure_code.as_deref().is_some_and(|code| {
            !matches!(
                code,
                "payload_missing"
                    | "parse_failed"
                    | "match_failed"
                    | "probe_failed"
                    | "write_failed"
            )
        })
        || cp
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason != "retry budget exhausted")
        || cp.admin_release_mbid.is_some() != cp.probe_tally.is_some()
    {
        return Err(CheckpointError::Invalid);
    }
    match cp.state {
        JobState::Queued | JobState::Parsing | JobState::Matching
            if cp.admin_release_mbid.is_none() => {}
        JobState::Probing if cp.admin_release_mbid.is_some() => {}
        JobState::Writing | JobState::Done => {
            let mut writing = cp.clone();
            writing.state = JobState::Writing;
            if !admin_gates::publish_ready(&writing) {
                return Err(CheckpointError::Invalid);
            }
        }
        JobState::NeedsReview
        | JobState::NeedsArchiveRead
        | JobState::Rejected
        | JobState::Failed => {}
        _ => return Err(CheckpointError::Invalid),
    }
    Ok(cp)
}

fn separator_spaces(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            map.len()
                + map.len().saturating_sub(1)
                + map.values().map(separator_spaces).sum::<usize>()
        }
        Value::Array(items) => {
            items.len().saturating_sub(1) + items.iter().map(separator_spaces).sum::<usize>()
        }
        _ => 0,
    }
}

// Value alone silently drops duplicate keys. Reject them recursively before
// allowlist/type validation, including duplicate numeric probe batch indices.
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("safe checkpoint JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(v)) = seq.next_element()? {
                    values.push(v);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((key, StrictValue(v))) = map.next_entry::<String, StrictValue>()? {
                    if values.insert(key, v).is_some() {
                        return Err(de::Error::custom("duplicate checkpoint key"));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        d.deserialize_any(StrictVisitor)
    }
}

#[cfg(test)]
#[path = "admin_checkpoint_tests.rs"]
mod tests;
