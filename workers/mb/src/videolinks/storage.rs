//! Bounded app-side candidate admission and fresh-link reads.
//!
//! These primitives do not activate a source reader or public route. The caller
//! must supply the app database, a trusted clock, and trusted external-source
//! provenance. User submissions require a separate atomic moderation path.

use super::{dedupe_candidates, is_valid_video_id, Candidate, VideoSource, VALIDATION_TTL_S};
use serde::Serialize;
use std::collections::HashSet;
use tokio_postgres::Client;

pub const MAX_CANDIDATES_PER_WRITE: usize = 200;
pub const MAX_RECORDINGS_PER_READ: usize = 50;
pub const MAX_LINKS_PER_RECORDING: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateStoreError {
    InvalidInput,
    InvalidTime,
    Store,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FreshVideoLink {
    pub candidate: Candidate,
    pub title: Option<String>,
    pub channel_id: Option<String>,
    pub duration_secs: Option<i32>,
    pub validated_at: i64,
    pub expires_at: i64,
}

/// Uses migration 0035 in the app database, never the CC0 mirror's `MB_DB`.
/// The caller owns connecting and driving the PostgreSQL connection.
pub struct PgCandidateStore<'a> {
    client: &'a mut Client,
}

impl<'a> PgCandidateStore<'a> {
    pub fn new(app_client: &'a mut Client) -> Self {
        Self { client: app_client }
    }

    /// Admit at most 200 externally sourced candidates atomically. Duplicate
    /// admission only upgrades provenance; it never resets validation or TTL.
    /// This is not an untrusted submission endpoint: `UserSubmission` is refused
    /// even if a caller asserts approval. No URL or source payload is persisted.
    pub async fn admit_external_candidates(
        &mut self,
        candidates: &[Candidate],
    ) -> Result<u64, CandidateStoreError> {
        let candidates = checked_candidates(candidates)?;
        if candidates.is_empty() {
            return Ok(0);
        }
        let json = admission_payload(&candidates)?;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        let rows = tx
            .execute(ADMIT_SQL, &[&json])
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        tx.commit().await.map_err(|_| CandidateStoreError::Store)?;
        Ok(rows)
    }

    /// Return at most five fresh playable links per recording, for at most 50
    /// recording MBIDs. Expired/future/never-validated rows are filtered in SQL,
    /// before their metadata is fetched. The independent validation-age check
    /// prevents a corrupt or overlong `expires_at` from extending the 30-day TTL.
    /// Reads neither contact YouTube nor refresh/purge metadata.
    pub async fn fresh_links(
        &mut self,
        recording_mbids: &[String],
        now: i64,
    ) -> Result<Vec<FreshVideoLink>, CandidateStoreError> {
        if now <= 0 {
            return Err(CandidateStoreError::InvalidTime);
        }
        let mbids = checked_recordings(recording_mbids)?;
        if mbids.is_empty() {
            return Ok(Vec::new());
        }
        let oldest = now - VALIDATION_TTL_S;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        tx.batch_execute("SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        let rows = tx
            .query(
                FRESH_LINKS_SQL,
                &[&now, &mbids, &oldest, &MAX_LINKS_PER_RECORDING],
            )
            .await
            .map_err(|_| CandidateStoreError::Store)?;
        let links = rows
            .into_iter()
            .map(|row| {
                let source: String = row.try_get(2).map_err(|_| CandidateStoreError::Store)?;
                let source = match source.as_str() {
                    "mb_url" => VideoSource::MbUrl,
                    "wikidata" => VideoSource::Wikidata,
                    "listenbrainz" => VideoSource::ListenBrainz,
                    "user_submission" => VideoSource::UserSubmission,
                    _ => return Err(CandidateStoreError::Store),
                };
                Ok(FreshVideoLink {
                    candidate: Candidate {
                        recording_mbid: row.try_get(0).map_err(|_| CandidateStoreError::Store)?,
                        video_id: row.try_get(1).map_err(|_| CandidateStoreError::Store)?,
                        source,
                    },
                    title: row.try_get(3).map_err(|_| CandidateStoreError::Store)?,
                    channel_id: row.try_get(4).map_err(|_| CandidateStoreError::Store)?,
                    duration_secs: row.try_get(5).map_err(|_| CandidateStoreError::Store)?,
                    validated_at: row.try_get(6).map_err(|_| CandidateStoreError::Store)?,
                    expires_at: row.try_get(7).map_err(|_| CandidateStoreError::Store)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(|_| CandidateStoreError::Store)?;
        Ok(links)
    }
}

fn canonical_mbid(mbid: &str) -> Result<String, CandidateStoreError> {
    super::canonical_mbid(mbid).ok_or(CandidateStoreError::InvalidInput)
}

fn checked_candidates(candidates: &[Candidate]) -> Result<Vec<Candidate>, CandidateStoreError> {
    if candidates.len() > MAX_CANDIDATES_PER_WRITE {
        return Err(CandidateStoreError::InvalidInput);
    }
    let candidates = candidates
        .iter()
        .map(|candidate| {
            if !is_valid_video_id(&candidate.video_id)
                || candidate.source == VideoSource::UserSubmission
            {
                return Err(CandidateStoreError::InvalidInput);
            }
            Ok(Candidate {
                recording_mbid: canonical_mbid(&candidate.recording_mbid)?,
                video_id: candidate.video_id.clone(),
                source: candidate.source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(dedupe_candidates(candidates))
}

#[derive(Serialize)]
struct StoredCandidate<'a> {
    recording_mbid: &'a str,
    video_id: &'a str,
    source: &'static str,
}

fn admission_payload(candidates: &[Candidate]) -> Result<String, CandidateStoreError> {
    let rows: Vec<_> = candidates
        .iter()
        .map(|candidate| StoredCandidate {
            recording_mbid: &candidate.recording_mbid,
            video_id: &candidate.video_id,
            source: candidate.source.as_str(),
        })
        .collect();
    serde_json::to_string(&rows).map_err(|_| CandidateStoreError::Store)
}

fn checked_recordings(mbids: &[String]) -> Result<Vec<String>, CandidateStoreError> {
    if mbids.len() > MAX_RECORDINGS_PER_READ {
        return Err(CandidateStoreError::InvalidInput);
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for mbid in mbids {
        let mbid = canonical_mbid(mbid)?;
        if seen.insert(mbid.clone()) {
            out.push(mbid);
        }
    }
    Ok(out)
}

const ADMIT_SQL: &str = "\
INSERT INTO public.video_link_candidates AS existing (recording_mbid, video_id, source)
SELECT c.recording_mbid::uuid, c.video_id, c.source
FROM jsonb_to_recordset($1::text::jsonb) AS c(recording_mbid text, video_id text, source text)
WHERE true
ON CONFLICT (recording_mbid, video_id) DO UPDATE
SET source = EXCLUDED.source, updated_at = now()
WHERE array_position(ARRAY['mb_url','wikidata','listenbrainz','user_submission'], EXCLUDED.source)
    < array_position(ARRAY['mb_url','wikidata','listenbrainz','user_submission'], existing.source)";

const FRESH_LINKS_SQL: &str = "\
SELECT c.recording_mbid::text, c.video_id, c.source, c.title, c.channel_id,
       c.duration_secs, c.validated_at, c.expires_at
FROM unnest($2::text[]) AS requested(mbid)
CROSS JOIN LATERAL (
    SELECT * FROM public.video_link_candidates
    WHERE recording_mbid = requested.mbid::uuid
      AND status = 'validated' AND embeddable IS TRUE
      AND validated_at > 0 AND validated_at <= $1 AND validated_at > $3
      AND expires_at > $1
    ORDER BY array_position(ARRAY['mb_url','wikidata','listenbrainz','user_submission'], source),
             validated_at DESC, video_id
    LIMIT $4
) AS c
ORDER BY c.recording_mbid,
         array_position(ARRAY['mb_url','wikidata','listenbrainz','user_submission'], c.source),
         c.validated_at DESC, c.video_id";

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(source: VideoSource) -> Candidate {
        Candidate {
            recording_mbid: "ABCDEF01-2345-6789-ABCD-0123456789AB".into(),
            video_id: "v0000000000".into(),
            source,
        }
    }

    #[test]
    fn admission_uses_database_source_labels() {
        let candidates: Vec<_> = [
            VideoSource::MbUrl,
            VideoSource::Wikidata,
            VideoSource::ListenBrainz,
        ]
        .into_iter()
        .map(candidate)
        .collect();
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&admission_payload(&candidates).unwrap()).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row["source"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["mb_url", "wikidata", "listenbrainz"]
        );
    }

    #[test]
    fn admission_canonicalizes_before_deduplication() {
        let first = candidate(VideoSource::ListenBrainz);
        let mut best = candidate(VideoSource::MbUrl);
        best.recording_mbid.make_ascii_lowercase();
        assert_eq!(checked_candidates(&[first, best.clone()]).unwrap(), [best]);
    }

    #[test]
    fn admission_refuses_submission_bypass_invalid_input_and_oversized_batches() {
        assert_eq!(
            checked_candidates(&[candidate(VideoSource::UserSubmission)]),
            Err(CandidateStoreError::InvalidInput)
        );
        let mut bad = candidate(VideoSource::Wikidata);
        bad.video_id = "invalid".into();
        assert!(checked_candidates(&[bad]).is_err());
        for mbid in [
            "",
            "not-an-mbid",
            "abcdef012345-6789-abcd-0123456789ab",
            "abcdef01-2345-6789-abcd-0123456789ag",
        ] {
            assert!(canonical_mbid(mbid).is_err());
        }
        let candidates = vec![candidate(VideoSource::MbUrl); MAX_CANDIDATES_PER_WRITE];
        assert_eq!(checked_candidates(&candidates).unwrap().len(), 1);
        assert!(checked_candidates(&vec![
            candidate(VideoSource::MbUrl);
            MAX_CANDIDATES_PER_WRITE + 1
        ])
        .is_err());
    }

    #[test]
    fn read_inputs_are_canonical_distinct_and_bounded_before_deduplication() {
        let mbid = candidate(VideoSource::MbUrl).recording_mbid;
        assert_eq!(
            checked_recordings(&[mbid.clone(), mbid.to_ascii_lowercase()]).unwrap(),
            [mbid.to_ascii_lowercase()]
        );
        assert!(checked_recordings(&vec![mbid; MAX_RECORDINGS_PER_READ + 1]).is_err());
        assert!(checked_recordings(&["invalid".into()]).is_err());
        assert!(checked_recordings(&[]).unwrap().is_empty());
    }
}
