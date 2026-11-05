//! Inactive app-side submission queue and atomic approval/promotion.
//!
//! The caller must authenticate the submitter, authorize every moderator read
//! and decision, and enforce request/rate limits. These database primitives do
//! not establish authorization and must never receive the CC0 mirror connection.

use super::{canonical_mbid, normalise_submission, SubmissionStatus};
use serde::Serialize;
use tokio_postgres::Client;

pub const MAX_PENDING_SUBMISSIONS: i64 = 50;
const MAX_SUBMISSION_BYTES: usize = 2048;
const MAX_SUBMITTER_BYTES: usize = 128;
const MAX_REASON_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionStoreError {
    InvalidInput,
    /// Missing and terminal rows intentionally share one failure shape.
    NotPending,
    Store,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingSubmission {
    pub id: String,
    pub recording_mbid: String,
    pub video_id: String,
    pub submitted_by: String,
}

/// Uses only migration 0035's app-side tables. The caller owns connecting and
/// driving the connection. Each statement has a 1.5-second server timeout.
pub struct PgSubmissionStore<'a> {
    client: &'a mut Client,
}

impl<'a> PgSubmissionStore<'a> {
    pub fn new(app_client: &'a mut Client) -> Self {
        Self { client: app_client }
    }

    /// Enqueue one normalized ID, not its raw URL or any API metadata. The
    /// submitter identifier must come from the authenticated session, never a
    /// request body. Repeated submissions are separate pending queue entries;
    /// the eventual route must impose abuse/duplicate limits before this call.
    pub async fn submit(
        &mut self,
        recording_mbid: &str,
        raw: &str,
        submitted_by: &str,
    ) -> Result<String, SubmissionStoreError> {
        let (mbid, video_id) = checked_submission(recording_mbid, raw, submitted_by)?;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        let row = tx
            .query_one(
                "INSERT INTO public.video_user_submissions
                 (recording_mbid, video_id, submitted_by)
                 VALUES ($1::text::uuid, $2, $3) RETURNING id::text",
                &[&mbid, &video_id, &submitted_by],
            )
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        let id = row.try_get(0).map_err(|_| SubmissionStoreError::Store)?;
        tx.commit().await.map_err(|_| SubmissionStoreError::Store)?;
        Ok(id)
    }

    /// Moderator-only worklist: at most 50 pending rows, oldest first with an
    /// ID tie-break. Decisions remove rows from this worklist; no offset scan
    /// can skip pending rows as concurrent moderators decide earlier entries.
    pub async fn pending(&mut self) -> Result<Vec<PendingSubmission>, SubmissionStoreError> {
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        let rows = tx
            .query(
                "SELECT id::text, recording_mbid::text, video_id, submitted_by
                 FROM public.video_user_submissions WHERE status = 'pending'
                 ORDER BY created_at, id LIMIT $1",
                &[&MAX_PENDING_SUBMISSIONS],
            )
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        let pending = rows
            .into_iter()
            .map(|row| {
                Ok(PendingSubmission {
                    id: row.try_get(0).map_err(|_| SubmissionStoreError::Store)?,
                    recording_mbid: row.try_get(1).map_err(|_| SubmissionStoreError::Store)?,
                    video_id: row.try_get(2).map_err(|_| SubmissionStoreError::Store)?,
                    submitted_by: row.try_get(3).map_err(|_| SubmissionStoreError::Store)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(|_| SubmissionStoreError::Store)?;
        Ok(pending)
    }

    /// Moderator-only terminal decision. Approval and candidate promotion
    /// commit together; rejection never promotes. Concurrent decisions have
    /// at most one winner. Existing candidate provenance, validation and TTL
    /// are preserved, and newly promoted candidates still require validation.
    /// A lost commit acknowledgement has an unknown outcome: `NotPending` on
    /// replay is NOT proof that this caller's earlier decision won.
    pub async fn decide(
        &mut self,
        submission_id: &str,
        approve: bool,
        reason: Option<&str>,
    ) -> Result<SubmissionStatus, SubmissionStoreError> {
        let id = canonical_mbid(submission_id).ok_or(SubmissionStoreError::InvalidInput)?;
        if reason.is_some_and(|text| !bounded_text(text, MAX_REASON_BYTES)) {
            return Err(SubmissionStoreError::InvalidInput);
        }
        let status = if approve {
            SubmissionStatus::Approved
        } else {
            SubmissionStatus::Rejected
        };
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        let row = tx
            .query_opt(
                "UPDATE public.video_user_submissions
                 SET status = $2, reason = $3, decided_at = now()
                 WHERE id = $1::text::uuid AND status = 'pending'
                 RETURNING recording_mbid::text, video_id",
                &[&id, &status.as_str(), &reason],
            )
            .await
            .map_err(|_| SubmissionStoreError::Store)?
            .ok_or(SubmissionStoreError::NotPending)?;
        if approve {
            let mbid: String = row.try_get(0).map_err(|_| SubmissionStoreError::Store)?;
            let video_id: String = row.try_get(1).map_err(|_| SubmissionStoreError::Store)?;
            tx.execute(
                "INSERT INTO public.video_link_candidates (recording_mbid, video_id, source)
                 VALUES ($1::text::uuid, $2, 'user_submission')
                 ON CONFLICT (recording_mbid, video_id) DO NOTHING",
                &[&mbid, &video_id],
            )
            .await
            .map_err(|_| SubmissionStoreError::Store)?;
        }
        tx.commit().await.map_err(|_| SubmissionStoreError::Store)?;
        Ok(status)
    }
}

fn bounded_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

fn checked_submission(
    recording_mbid: &str,
    raw: &str,
    submitted_by: &str,
) -> Result<(String, String), SubmissionStoreError> {
    if raw.len() > MAX_SUBMISSION_BYTES || !bounded_text(submitted_by, MAX_SUBMITTER_BYTES) {
        return Err(SubmissionStoreError::InvalidInput);
    }
    let mbid = canonical_mbid(recording_mbid).ok_or(SubmissionStoreError::InvalidInput)?;
    let video_id = normalise_submission(raw).map_err(|_| SubmissionStoreError::InvalidInput)?;
    Ok((mbid, video_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBID: &str = "ABCDEF01-2345-6789-ABCD-0123456789AB";

    #[test]
    fn submission_is_canonical_and_retains_only_the_id() {
        assert_eq!(
            checked_submission(MBID, " https://youtu.be/v0000000000 ", "fixture-user").unwrap(),
            (MBID.to_ascii_lowercase(), "v0000000000".into())
        );
        assert_eq!(
            checked_submission(MBID, "v0000000000", "fixture-user").unwrap(),
            (MBID.to_ascii_lowercase(), "v0000000000".into())
        );
    }

    #[test]
    fn submission_refuses_invalid_and_unbounded_inputs_before_sql() {
        for (mbid, raw, user) in [
            ("not-an-mbid", "v0000000000", "user"),
            (MBID, "https://example.com/v0000000000", "user"),
            (MBID, "bad", "user"),
            (MBID, "v0000000000", " "),
            (MBID, "v0000000000", "user\n"),
        ] {
            assert_eq!(
                checked_submission(mbid, raw, user),
                Err(SubmissionStoreError::InvalidInput)
            );
        }
        assert!(checked_submission(MBID, &"x".repeat(MAX_SUBMISSION_BYTES + 1), "user").is_err());
        assert!(checked_submission(MBID, "v0000000000", &"x".repeat(MAX_SUBMITTER_BYTES)).is_ok());
        assert!(
            checked_submission(MBID, "v0000000000", &"x".repeat(MAX_SUBMITTER_BYTES + 1)).is_err()
        );
        assert!(bounded_text(
            &"x".repeat(MAX_REASON_BYTES),
            MAX_REASON_BYTES
        ));
        assert!(!bounded_text(
            &"x".repeat(MAX_REASON_BYTES + 1),
            MAX_REASON_BYTES
        ));
        assert!(!bounded_text("\t", MAX_REASON_BYTES));
        assert!(!bounded_text("", MAX_REASON_BYTES));
    }
}
