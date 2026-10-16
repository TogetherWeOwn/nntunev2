//! Durable ingest checkpoint storage in the `INGEST_STATE` KV namespace.

use worker::{Env, Result};

use crate::jobs::{validate_job_id, JobCheckpoint};

pub const STATE_KV_BINDING: &str = "INGEST_STATE";
pub const CHECKPOINT_TTL_SECS: u64 = 60 * 60 * 24 * 30;

fn checkpoint_key(job_id: &str) -> Option<String> {
    validate_job_id(job_id).then(|| format!("ingest:checkpoint:{job_id}"))
}

pub async fn get(env: &Env, job_id: &str) -> Result<Option<JobCheckpoint>> {
    let key =
        checkpoint_key(job_id).ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .get(&key)
        .json::<JobCheckpoint>()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

pub async fn put(env: &Env, checkpoint: &JobCheckpoint) -> Result<()> {
    let key = checkpoint_key(&checkpoint.job_id)
        .ok_or_else(|| worker::Error::RustError("invalid job id".into()))?;
    let store = env.kv(STATE_KV_BINDING)?;
    store
        .put(&key, serde_json::to_string(checkpoint)?)?
        .expiration_ttl(CHECKPOINT_TTL_SECS)
        .execute()
        .await
        .map_err(|e| worker::Error::RustError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_keys_are_scoped_and_job_id_validated() {
        assert_eq!(
            checkpoint_key("job_01-a").as_deref(),
            Some("ingest:checkpoint:job_01-a")
        );
        assert!(checkpoint_key("../secret").is_none());
        assert!(checkpoint_key("").is_none());
    }
}
