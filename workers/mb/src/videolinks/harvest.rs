//! Bounded read-only candidate pages from the CC0 MusicBrainz mirror.
//!
//! No live source API, app-side write, route or schedule is activated here.
//! A cursor belongs to an unchanged recording selection and mirror snapshot;
//! callers must restart after an import and admit pages idempotently.

use super::{canonical_mbid, dedupe_candidates, extract_video_id, Candidate, VideoSource};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;

pub const MAX_RECORDINGS_PER_HARVEST: usize = 50;
pub const MAX_MIRROR_ROWS_PER_PAGE: usize = 200;
const MAX_SOURCE_URL_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarvestError {
    InvalidInput,
    Store,
}

/// Last consumed raw edge, not the last successfully parsed candidate. This
/// allows a page containing no usable URLs to advance without starving later IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorCursor {
    pub recording_mbid: String,
    pub url_mbid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorPage {
    pub candidates: Vec<Candidate>,
    pub scanned_rows: usize,
    pub next_cursor: Option<MirrorCursor>,
}

/// Reads W6b's public mirror tables only. The caller connects/drives the mirror
/// connection separately from the app-side candidate store's connection.
pub struct PgMirrorSource<'a> {
    client: &'a mut Client,
}

impl<'a> PgMirrorSource<'a> {
    pub fn new(mirror_client: &'a mut Client) -> Self {
        Self {
            client: mirror_client,
        }
    }

    /// Read at most 200 raw edges (+ one lookahead) for at most 50 recordings.
    /// Follow own video-marked URLs and either side of music-video pairs, but
    /// always associate candidates with the requested recording. Other hosts,
    /// malformed IDs and URLs over 2048 bytes are consumed without admission.
    pub async fn candidate_page(
        &mut self,
        recording_mbids: &[String],
        after: Option<&MirrorCursor>,
    ) -> Result<MirrorPage, HarvestError> {
        let (mbids, after) = checked_selection(recording_mbids, after)?;
        if mbids.is_empty() {
            return Ok(page_from_edges(Vec::new()));
        }
        let after_recording = after.as_ref().map(|c| c.recording_mbid.as_str());
        let after_url = after.as_ref().map(|c| c.url_mbid.as_str());
        let limit = (MAX_MIRROR_ROWS_PER_PAGE + 1) as i64;
        let max_url_bytes = MAX_SOURCE_URL_BYTES as i32;
        let tx = self
            .client
            .transaction()
            .await
            .map_err(|_| HarvestError::Store)?;
        tx.batch_execute("SET TRANSACTION READ ONLY; SET LOCAL statement_timeout = '1500ms'")
            .await
            .map_err(|_| HarvestError::Store)?;
        let rows = tx
            .query(
                MIRROR_PAGE_SQL,
                &[&mbids, &after_recording, &after_url, &limit, &max_url_bytes],
            )
            .await
            .map_err(|_| HarvestError::Store)?;
        let edges = rows
            .into_iter()
            .map(|row| {
                Ok(MirrorEdge {
                    key: MirrorCursor {
                        recording_mbid: row.try_get(0).map_err(|_| HarvestError::Store)?,
                        url_mbid: row.try_get(1).map_err(|_| HarvestError::Store)?,
                    },
                    url: row.try_get(2).map_err(|_| HarvestError::Store)?,
                })
            })
            .collect::<Result<Vec<_>, HarvestError>>()?;
        tx.commit().await.map_err(|_| HarvestError::Store)?;
        Ok(page_from_edges(edges))
    }
}

fn checked_selection(
    mbids: &[String],
    after: Option<&MirrorCursor>,
) -> Result<(Vec<String>, Option<MirrorCursor>), HarvestError> {
    if mbids.len() > MAX_RECORDINGS_PER_HARVEST {
        return Err(HarvestError::InvalidInput);
    }
    let mut mbids = mbids
        .iter()
        .map(|mbid| canonical_mbid(mbid).ok_or(HarvestError::InvalidInput))
        .collect::<Result<Vec<_>, _>>()?;
    mbids.sort();
    mbids.dedup();
    let after = after
        .map(|cursor| {
            let recording_mbid =
                canonical_mbid(&cursor.recording_mbid).ok_or(HarvestError::InvalidInput)?;
            if mbids.binary_search(&recording_mbid).is_err() {
                return Err(HarvestError::InvalidInput);
            }
            Ok(MirrorCursor {
                recording_mbid,
                url_mbid: canonical_mbid(&cursor.url_mbid).ok_or(HarvestError::InvalidInput)?,
            })
        })
        .transpose()?;
    Ok((mbids, after))
}

struct MirrorEdge {
    key: MirrorCursor,
    url: Option<String>,
}

fn page_from_edges(mut edges: Vec<MirrorEdge>) -> MirrorPage {
    let has_more = edges.len() > MAX_MIRROR_ROWS_PER_PAGE;
    edges.truncate(MAX_MIRROR_ROWS_PER_PAGE);
    let next_cursor = has_more.then(|| edges.last().expect("nonempty bounded page").key.clone());
    let scanned_rows = edges.len();
    let candidates = edges
        .into_iter()
        .filter_map(|edge| {
            let url = edge.url?;
            if url.len() > MAX_SOURCE_URL_BYTES {
                return None;
            }
            Some(Candidate {
                recording_mbid: edge.key.recording_mbid,
                video_id: extract_video_id(&url)?,
                source: VideoSource::MbUrl,
            })
        })
        .collect();
    MirrorPage {
        candidates: dedupe_candidates(candidates),
        scanned_rows,
        next_cursor,
    }
}

const MIRROR_PAGE_SQL: &str = "\
WITH requested AS (SELECT unnest($1::text[])::uuid AS mbid),
edges AS (
    SELECT r.mbid AS recording_mbid, ru.url_mbid
    FROM requested r JOIN public.mb_recording_url ru ON ru.recording_mbid = r.mbid
    WHERE ru.is_video
    UNION
    SELECT r.mbid, ru.url_mbid
    FROM requested r
    JOIN public.mb_recording_music_video mv ON mv.recording_a_mbid = r.mbid
    JOIN public.mb_recording_url ru ON ru.recording_mbid = mv.recording_b_mbid
    WHERE ru.is_video
    UNION
    SELECT r.mbid, ru.url_mbid
    FROM requested r
    JOIN public.mb_recording_music_video mv ON mv.recording_b_mbid = r.mbid
    JOIN public.mb_recording_url ru ON ru.recording_mbid = mv.recording_a_mbid
    WHERE ru.is_video
), page AS (
    SELECT recording_mbid, url_mbid FROM edges
    WHERE $2::text IS NULL
       OR (recording_mbid, url_mbid) > ($2::text::uuid, $3::text::uuid)
    ORDER BY recording_mbid, url_mbid LIMIT $4
)
SELECT p.recording_mbid::text, p.url_mbid::text,
       CASE WHEN octet_length(u.url) <= $5 THEN u.url ELSE NULL END
FROM page p LEFT JOIN public.mb_url u ON u.mbid = p.url_mbid
ORDER BY p.recording_mbid, p.url_mbid";

#[cfg(test)]
mod tests {
    use super::*;

    const MBID: &str = "abcdef01-2345-6789-abcd-0123456789ab";

    fn edge(i: usize, url: Option<String>) -> MirrorEdge {
        MirrorEdge {
            key: MirrorCursor {
                recording_mbid: MBID.into(),
                url_mbid: format!("00000000-0000-0000-0000-{i:012x}"),
            },
            url,
        }
    }

    #[test]
    fn selection_canonicalizes_and_refuses_invalid_or_unrelated_cursors() {
        let cursor = MirrorCursor {
            recording_mbid: MBID.to_uppercase(),
            url_mbid: MBID.to_uppercase(),
        };
        let (mbids, after) =
            checked_selection(&[MBID.into(), MBID.to_uppercase()], Some(&cursor)).unwrap();
        assert_eq!(mbids, [MBID]);
        assert_eq!(after.unwrap().url_mbid, MBID);
        assert!(checked_selection(&[], Some(&cursor)).is_err());
        assert!(checked_selection(&["invalid".into()], None).is_err());
        let bad = MirrorCursor {
            url_mbid: "invalid".into(),
            ..cursor
        };
        assert!(checked_selection(&[MBID.into()], Some(&bad)).is_err());
        assert!(checked_selection(&vec![MBID.into(); MAX_RECORDINGS_PER_HARVEST], None).is_ok());
        assert!(
            checked_selection(&vec![MBID.into(); MAX_RECORDINGS_PER_HARVEST + 1], None).is_err()
        );
    }

    #[test]
    fn empty_candidate_page_advances_on_raw_edges_without_consuming_lookahead() {
        let mut edges: Vec<_> = (0..MAX_MIRROR_ROWS_PER_PAGE)
            .map(|i| {
                edge(
                    i,
                    Some("https://other.example.test/watch?v=v0000000000".into()),
                )
            })
            .collect();
        edges.push(edge(200, Some("https://youtu.be/v0000000000".into())));
        let page = page_from_edges(edges);
        assert!(page.candidates.is_empty());
        assert_eq!(page.scanned_rows, MAX_MIRROR_ROWS_PER_PAGE);
        assert_eq!(
            page.next_cursor.unwrap().url_mbid,
            edge(199, None).key.url_mbid
        );
        let last = page_from_edges(vec![edge(200, Some("https://youtu.be/v0000000000".into()))]);
        assert_eq!(last.candidates.len(), 1);
        assert_eq!(last.next_cursor, None);
    }

    #[test]
    fn pages_dedupe_and_drop_malformed_missing_and_oversized_urls() {
        let page = page_from_edges(vec![
            edge(0, Some("https://youtu.be/v0000000000".into())),
            edge(
                1,
                Some("https://www.youtube.com/watch?v=v0000000000".into()),
            ),
            edge(2, Some("https://youtu.be/invalid".into())),
            edge(3, None),
            edge(
                4,
                Some(format!(
                    "https://youtu.be/v0000000001?{}",
                    "x".repeat(MAX_SOURCE_URL_BYTES)
                )),
            ),
        ]);
        assert_eq!(page.scanned_rows, 5);
        assert_eq!(page.candidates.len(), 1);
        assert_eq!(page.candidates[0].source, VideoSource::MbUrl);
        assert_eq!(page.next_cursor, None);
        let prefix = "https://youtu.be/v0000000002?";
        let boundary = format!(
            "{prefix}{}",
            "x".repeat(MAX_SOURCE_URL_BYTES - prefix.len())
        );
        assert_eq!(boundary.len(), MAX_SOURCE_URL_BYTES);
        assert_eq!(
            page_from_edges(vec![edge(0, Some(boundary))])
                .candidates
                .len(),
            1
        );
    }
}
