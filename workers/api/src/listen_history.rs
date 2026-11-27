//! Client-reported listening history, separate from billing and playback leases.

use crate::postgres::{PgConnect, PgQuery, PgTransaction};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::Type;

use crate::handlers::StoreError;

pub const EXPORT_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenEvent {
    Start,
    Complete,
    Skip,
}

impl ListenEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Complete => "complete",
            Self::Skip => "skip",
        }
    }

    pub const fn phase(self) -> i16 {
        match self {
            Self::Start => 0,
            Self::Complete | Self::Skip => 1,
        }
    }
}

pub(crate) fn valid_playback_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenEventBody {
    pub playback_id: String,
    pub track_id: String,
    pub event: ListenEvent,
}

impl ListenEventBody {
    pub(crate) fn valid_identity(&self) -> bool {
        valid_playback_id(&self.playback_id)
            && (1..=128).contains(&self.track_id.len())
            && !self.track_id.chars().any(char::is_control)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListenRow {
    pub playback_id: String,
    pub track_id: String,
    pub event: ListenEvent,
    /// Server acceptance time, not a claim about elapsed streaming seconds.
    pub recorded_at: i64,
}

impl ListenRow {
    pub fn cursor(&self) -> String {
        format!("{}:{}", self.playback_id, self.event.phase())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    Conflict,
    AccountDeleting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportCursor {
    playback_id: String,
    phase: i16,
}

impl ExportCursor {
    pub fn parse(value: Option<&str>) -> Result<Option<Self>, &'static str> {
        let Some(value) = value else { return Ok(None) };
        let (playback_id, phase) = value.split_once(':').ok_or("invalid before_id")?;
        let phase = match phase {
            "0" => 0,
            "1" => 1,
            _ => return Err("invalid before_id"),
        };
        if !valid_playback_id(playback_id) {
            return Err("invalid before_id");
        }
        Ok(Some(Self {
            playback_id: playback_id.into(),
            phase,
        }))
    }
}

// Lock acquisition and history access must be separate statements at READ
// COMMITTED, so the history snapshot starts after the owner lock is granted.
async fn owner_is_deleting(
    tx: &PgTransaction<'_>,
    owner: &str,
) -> Result<Option<bool>, StoreError> {
    tx.query_typed(
        "SELECT deleting_at IS NOT NULL FROM public.users WHERE id = $1 FOR NO KEY UPDATE",
        &[(&owner, Type::TEXT)],
    )
    .await
    .map_err(|_| StoreError)?
    .into_iter()
    .next()
    .map(|row| row.try_get(0).map_err(|_| StoreError))
    .transpose()
}

async fn begin_write(client: &mut impl PgConnect) -> Result<PgTransaction<'_>, StoreError> {
    let tx = client.read_committed().await.map_err(|_| StoreError)?;
    tx.set_local_statement_timeout(crate::vault::TOTAL_TIMEOUT_MS)
        .await
        .map_err(|_| StoreError)?;
    Ok(tx)
}

pub async fn record(
    client: &mut impl PgConnect,
    owner: &str,
    body: &ListenEventBody,
    now: i64,
) -> Result<WriteOutcome, StoreError> {
    if !body.valid_identity() {
        return Err(StoreError);
    }
    let tx = begin_write(client).await?;
    match owner_is_deleting(&tx, owner).await? {
        Some(true) => return Ok(WriteOutcome::AccountDeleting),
        None => return Err(StoreError),
        Some(false) => {}
    }
    let rows = tx
        .query_typed(
            include_str!("listen_history_insert.sql"),
            &[
                (&owner, Type::TEXT),
                (&body.playback_id, Type::TEXT),
                (&body.track_id, Type::TEXT),
                (&body.event.as_str(), Type::TEXT),
                (&body.event.phase(), Type::INT2),
                (&now, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(if rows.is_empty() {
        WriteOutcome::Conflict
    } else {
        WriteOutcome::Written
    })
}

async fn clear_in_transaction(tx: &PgTransaction<'_>, owner: &str) -> Result<(), StoreError> {
    if owner_is_deleting(tx, owner).await?.is_some() {
        tx.query_typed(
            "DELETE FROM public.listen_events WHERE user_id = $1",
            &[(&owner, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    }
    Ok(())
}

pub async fn clear(client: &mut impl PgConnect, owner: &str) -> Result<(), StoreError> {
    let tx = begin_write(client).await?;
    clear_in_transaction(&tx, owner).await?;
    tx.commit().await.map_err(|_| StoreError)
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "listen_history_postgres_tests.rs"]
pub(crate) mod postgres_tests;

pub async fn export(
    client: &impl PgQuery,
    owner: &str,
    before: Option<&ExportCursor>,
) -> Result<Vec<ListenRow>, StoreError> {
    let playback_id = before.map(|value| value.playback_id.as_str());
    let phase = before.map(|value| value.phase);
    let limit = (EXPORT_PAGE_SIZE + 1) as i64;
    let rows = client
        .query_typed(
            include_str!("listen_history_export.sql"),
            &[
                (&owner, Type::TEXT),
                (&playback_id, Type::TEXT),
                (&phase, Type::INT2),
                (&limit, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    rows.iter()
        .map(|row| {
            let event: &str = row.try_get("event").map_err(|_| StoreError)?;
            let event = match event {
                "start" => ListenEvent::Start,
                "complete" => ListenEvent::Complete,
                "skip" => ListenEvent::Skip,
                _ => return Err(StoreError),
            };
            Ok(ListenRow {
                playback_id: row.try_get("playback_id").map_err(|_| StoreError)?,
                track_id: row.try_get("track_id").map_err(|_| StoreError)?,
                event,
                recorded_at: row.try_get("recorded_at").map_err(|_| StoreError)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBACK: &str = "40c325c0-2cee-489d-a8b6-fbe937155671";

    #[test]
    fn export_cursor_uses_only_playback_identity_and_phase() {
        let start = format!("{PLAYBACK}:0");
        let end = format!("{PLAYBACK}:1");
        assert_eq!(ExportCursor::parse(None), Ok(None));
        assert_eq!(ExportCursor::parse(Some(&start)).unwrap().unwrap().phase, 0);
        assert_eq!(ExportCursor::parse(Some(&end)).unwrap().unwrap().phase, 1);
        for value in [
            "",
            "42",
            PLAYBACK,
            &format!("{PLAYBACK}:2"),
            &format!("{PLAYBACK}:0:1"),
        ] {
            assert!(ExportCursor::parse(Some(value)).is_err(), "{value:?}");
        }
    }
}
