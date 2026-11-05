//! Bounded ahead-of-time validation and app-side PostgreSQL persistence.
//!
//! Runtime activation is deliberately separate: the caller must provide an
//! app-database connection (never `MB_DB`), a credential-bound HTTP client and
//! a trusted Unix clock. No route, schedule or binding is enabled here.

use super::{is_playable, is_valid_video_id, parse_videos_list_response, ValidatedVideo};
use super::{VALIDATION_TTL_S, VIDEOS_LIST_MAX_IDS};
use serde::Serialize;
use std::collections::HashSet;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;
use tokio_postgres::Client;

/// At most four API calls per invocation; failures never retry in the run.
pub const MAX_VALIDATIONS_PER_RUN: usize = 200;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const RUN_DEADLINE: Duration = Duration::from_secs(45);
const REFRESH_AHEAD_S: i64 = 24 * 60 * 60;

/// Safe to report without logging an API body, request URL, key or DB error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    Store,
    Transport,
    HttpStatus(u16),
    InvalidQueue,
    InvalidResponse,
    InvalidTime,
    Deadline,
}

pub struct ValidationResponse {
    pub status: u16,
    pub body: String,
}

/// The adapter calls only the official HTTPS `videos.list` endpoint. It must
/// refuse redirects, cap the body while streaming, and abort fetch on drop.
/// Keys come from the runtime secret binding, never from candidates or logs.
#[allow(async_fn_in_trait)]
pub trait ValidationClient {
    async fn videos_list(&self, ids: &[String]) -> Result<ValidationResponse, ValidationError>;
}

/// One checked successful response, including requested IDs absent from items.
/// Private fields prevent callers constructing an unchecked store update.
#[derive(Clone)]
pub struct BatchValidation {
    requested_ids: Vec<String>,
    videos: Vec<ValidatedVideo>,
    validated_at: i64,
}

impl BatchValidation {
    pub fn requested_ids(&self) -> &[String] {
        &self.requested_ids
    }

    pub fn videos(&self) -> &[ValidatedVideo] {
        &self.videos
    }

    pub fn validated_at(&self) -> i64 {
        self.validated_at
    }

    pub fn expires_at(&self) -> i64 {
        self.validated_at + VALIDATION_TTL_S
    }
}

#[allow(async_fn_in_trait)]
pub trait ValidationStore {
    /// Remove expired API-returned fields even when validation is unavailable.
    async fn purge_expired(&mut self, now: i64) -> Result<u64, ValidationError>;
    /// Return bounded distinct IDs, oldest due first. Only admitted candidates
    /// belong in this store: pending submissions must never enter it.
    async fn due_video_ids(
        &mut self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<String>, ValidationError>;
    /// Apply a whole checked batch atomically; missing IDs are rejected. Do not
    /// overwrite a validation recorded at or after this run's timestamp.
    async fn apply_batch(&mut self, batch: &BatchValidation) -> Result<u64, ValidationError>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub purged_rows: u64,
    pub requested_ids: usize,
    pub batches: usize,
    pub updated_rows: u64,
}

/// Purge before contacting YouTube, then validate at most 200 unique IDs in
/// 50-ID batches. A failed/malformed response leaves that batch unchanged.
/// Earlier commits survive a later failure and are not replayed in this run.
/// The timestamp precedes all requests, so transport latency never extends TTL.
pub async fn refresh_due(
    store: &mut impl ValidationStore,
    client: &impl ValidationClient,
    now: i64,
) -> Result<ValidationReport, ValidationError> {
    if !(1..=i64::MAX - VALIDATION_TTL_S).contains(&now) {
        return Err(ValidationError::InvalidTime);
    }
    with_deadline(RUN_DEADLINE, async {
        let purged_rows = store.purge_expired(now).await?;
        let ids = store.due_video_ids(now, MAX_VALIDATIONS_PER_RUN).await?;
        if ids.len() > MAX_VALIDATIONS_PER_RUN {
            return Err(ValidationError::InvalidQueue);
        }
        let mut seen = HashSet::new();
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect();
        if ids.iter().any(|id| !is_valid_video_id(id)) {
            return Err(ValidationError::InvalidQueue);
        }
        let mut report = ValidationReport {
            purged_rows,
            ..ValidationReport::default()
        };
        for ids in ids.chunks(VIDEOS_LIST_MAX_IDS) {
            let response = client.videos_list(ids).await?;
            if response.status != 200 {
                return Err(ValidationError::HttpStatus(response.status));
            }
            if response.body.len() > MAX_RESPONSE_BYTES {
                return Err(ValidationError::InvalidResponse);
            }
            let videos = parse_videos_list_response(&response.body)
                .map_err(|_| ValidationError::InvalidResponse)?;
            let requested: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let mut returned = HashSet::new();
            if videos.iter().any(|video| {
                !requested.contains(video.video_id.as_str())
                    || !returned.insert(video.video_id.as_str())
                    || video.duration_secs > i32::MAX as u32
            }) {
                return Err(ValidationError::InvalidResponse);
            }
            let batch = BatchValidation {
                requested_ids: ids.to_vec(),
                videos,
                validated_at: now,
            };
            report.updated_rows += store.apply_batch(&batch).await?;
            report.requested_ids += ids.len();
            report.batches += 1;
        }
        Ok(report)
    })
    .await
}

async fn with_deadline<T>(
    budget: Duration,
    operation: impl Future<Output = Result<T, ValidationError>>,
) -> Result<T, ValidationError> {
    #[cfg(target_arch = "wasm32")]
    let deadline = worker::Delay::from(budget);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(budget);
    let mut deadline = std::pin::pin!(deadline);
    let mut operation = std::pin::pin!(operation);
    std::future::poll_fn(|cx| {
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(ValidationError::Deadline));
        }
        operation.as_mut().poll(cx)
    })
    .await
}

/// App-side adapter for migration 0035. The caller owns connecting and driving
/// the connection. It must not pass the CC0 mirror's read-only connection.
/// Each statement has a server-side timeout as well as the runner's deadline;
/// cancelling the waiter alone does not cancel PostgreSQL work.
pub struct PgValidationStore<'a> {
    client: &'a mut Client,
}

impl<'a> PgValidationStore<'a> {
    pub fn new(app_client: &'a mut Client) -> Self {
        Self { client: app_client }
    }
}

const PURGE_EXPIRED_SQL: &str = "\
UPDATE public.video_link_candidates
SET title = NULL, channel_id = NULL, duration_secs = NULL, embeddable = NULL,
    status = 'expired', expires_at = 0, updated_at = now()
WHERE expires_at <= $1
  AND (status IN ('validated', 'rejected') OR title IS NOT NULL
       OR channel_id IS NOT NULL OR duration_secs IS NOT NULL OR embeddable IS NOT NULL)";

const DUE_IDS_SQL: &str = "\
SELECT video_id
FROM public.video_link_candidates
WHERE status IN ('pending', 'expired') OR expires_at <= $1
GROUP BY video_id
ORDER BY min(expires_at), video_id
LIMIT $2";

const APPLY_BATCH_SQL: &str = "\
WITH results AS (
    SELECT * FROM jsonb_to_recordset($1::text::jsonb)
    AS r(video_id text, status text, title text, channel_id text,
         duration_secs integer, embeddable boolean)
)
UPDATE public.video_link_candidates AS c
SET status = r.status, title = r.title, channel_id = r.channel_id,
    duration_secs = r.duration_secs, embeddable = r.embeddable,
    validated_at = $2, expires_at = $3, updated_at = now()
FROM results AS r
WHERE c.video_id = r.video_id
  AND c.validated_at < $2";

#[derive(Serialize)]
struct StoredValidation<'a> {
    video_id: &'a str,
    status: &'static str,
    title: Option<&'a str>,
    channel_id: Option<&'a str>,
    duration_secs: Option<u32>,
    embeddable: Option<bool>,
}

fn stored_updates(batch: &BatchValidation) -> Result<String, ValidationError> {
    let updates: Vec<StoredValidation<'_>> = batch
        .requested_ids
        .iter()
        .map(|id| {
            let video = batch.videos.iter().find(|video| &video.video_id == id);
            StoredValidation {
                video_id: id,
                status: if video.is_some_and(is_playable) {
                    "validated"
                } else {
                    "rejected"
                },
                title: video.map(|video| video.title.as_str()),
                channel_id: video.map(|video| video.channel_id.as_str()),
                duration_secs: video.map(|video| video.duration_secs),
                embeddable: video.map(|video| video.embeddable),
            }
        })
        .collect();
    serde_json::to_string(&updates).map_err(|_| ValidationError::Store)
}

impl ValidationStore for PgValidationStore<'_> {
    async fn purge_expired(&mut self, now: i64) -> Result<u64, ValidationError> {
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| ValidationError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| ValidationError::Store)?;
        let rows = tx
            .execute(PURGE_EXPIRED_SQL, &[&now])
            .await
            .map_err(|_| ValidationError::Store)?;
        tx.commit().await.map_err(|_| ValidationError::Store)?;
        Ok(rows)
    }

    async fn due_video_ids(
        &mut self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<String>, ValidationError> {
        let limit = limit.min(MAX_VALIDATIONS_PER_RUN) as i64;
        let due_before = now.saturating_add(REFRESH_AHEAD_S);
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| ValidationError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| ValidationError::Store)?;
        let rows = tx
            .query(DUE_IDS_SQL, &[&due_before, &limit])
            .await
            .map_err(|_| ValidationError::Store)?;
        let ids = rows
            .into_iter()
            .map(|row| row.try_get(0).map_err(|_| ValidationError::Store))
            .collect::<Result<Vec<String>, _>>()?;
        tx.commit().await.map_err(|_| ValidationError::Store)?;
        Ok(ids)
    }

    async fn apply_batch(&mut self, batch: &BatchValidation) -> Result<u64, ValidationError> {
        let updates = stored_updates(batch)?;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| ValidationError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| ValidationError::Store)?;
        let rows = tx
            .execute(
                APPLY_BATCH_SQL,
                &[&updates, &batch.validated_at, &batch.expires_at()],
            )
            .await
            .map_err(|_| ValidationError::Store)?;
        tx.commit().await.map_err(|_| ValidationError::Store)?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};

    #[test]
    fn stored_rows_distinguish_playable_private_and_missing_without_thumbnails() {
        let videos = parse_videos_list_response(r#"{"items":[
            {"id":"v0000000000","status":{"embeddable":true,"privacyStatus":"public"},
             "snippet":{"title":"visible","channelId":"channel","thumbnails":{"default":{"url":"https://example.invalid/image"}}}},
            {"id":"v0000000001","status":{"embeddable":true,"privacyStatus":"private"}}
        ]}"#).unwrap();
        let batch = BatchValidation {
            requested_ids: (0..3).map(|i| format!("v{i:010}")).collect(),
            videos,
            validated_at: 1_700_000_000,
        };
        let json = stored_updates(&batch).unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(rows[0]["status"], "validated");
        assert_eq!(rows[0]["title"], "visible");
        assert_eq!(rows[1]["status"], "rejected");
        assert_eq!(rows[2]["status"], "rejected");
        assert!(rows[2]["title"].is_null());
        assert!(rows[2]["channel_id"].is_null());
        assert!(rows[2]["duration_secs"].is_null());
        assert!(rows[2]["embeddable"].is_null());
        assert!(!json.contains("thumbnail"));
        assert!(!json.contains("example.invalid"));
    }

    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    #[test]
    fn deadline_drops_an_unfinished_operation() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let operation = async move {
            let _guard = guard;
            std::future::pending::<Result<(), ValidationError>>().await
        };
        let mut future = std::pin::pin!(with_deadline(Duration::from_millis(5), operation));
        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let limit = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
                assert_eq!(result, Err(ValidationError::Deadline));
                break;
            }
            assert!(std::time::Instant::now() < limit, "deadline must fire");
            std::thread::park_timeout(Duration::from_millis(20));
        }
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }
}
