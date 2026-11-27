//! Bounded mirror projection for the unmounted staging construction path.

use super::subsonic_pg_page::{admit_page, fetch_page, Param};
use super::{with_deadline, StoreError};
use crate::catalogue::{self, ArtistDbRow, ReleaseDbRow, TrackDbRow};
use crate::postgres::{PgClient, PgConnect, PgQuery, PgTransaction};
use nntune_subsonic::{
    construction_budget::ConstructionBudget, staging_rejection::StagingRejection,
};
use std::future::Future;
use tokio_postgres::{
    types::{ToSql, Type},
    Row,
};

type CatalogueRows = (Vec<ArtistDbRow>, Vec<ReleaseDbRow>, Vec<TrackDbRow>);
const MIRROR_ROWS: i64 = 1_024;
const MIRROR_TEXT_BYTES: i32 = 256 * 1_024;

/// Borrow already charged, normalized membership from the authenticated app
/// read. Exact pairs must remain sorted for the occurrence fence.
pub(crate) struct MirrorSelection<'a> {
    pub releases: &'a [String],
    pub pairs: &'a [(String, String)],
    pub remaining_text_bytes: i32,
}

impl MirrorSelection<'_> {
    fn contains_release(&self, release: &str) -> bool {
        self.releases
            .binary_search_by(|id| id.as_str().cmp(release))
            .is_ok()
    }

    fn validate(&self) -> Result<(), StagingRejection> {
        let canonical = |id: &str| {
            catalogue::is_catalogue_mbid(id) && !id.bytes().any(|b| b.is_ascii_uppercase())
        };
        if !(0..=MIRROR_TEXT_BYTES).contains(&self.remaining_text_bytes)
            || self.releases.len() > MIRROR_ROWS as usize
            || self.pairs.len() > MIRROR_ROWS as usize
            || !self.releases.iter().all(|id| canonical(id))
            || !self.releases.windows(2).all(|ids| ids[0] < ids[1])
            || !self.pairs.windows(2).all(|pairs| pairs[0] < pairs[1])
            || !self.pairs.iter().all(|(release, recording)| {
                canonical(recording) && self.contains_release(release)
            })
        {
            return Err(StagingRejection::BusyOrCapacity);
        }
        Ok(())
    }
}

fn join_text(budget: &mut ConstructionBudget, parts: &[&str]) -> Result<String, StoreError> {
    let len = parts
        .iter()
        .try_fold(0usize, |len, part| len.checked_add(part.len()))
        .ok_or(StoreError)?;
    let mut bytes = budget.allocate_exact(len).map_err(|_| StoreError)?;
    for part in parts {
        bytes.extend_from_slice(part.as_bytes());
    }
    String::from_utf8(bytes).map_err(|_| StoreError)
}

// Reuse the accepted page statements, including their ordering and sentinel
// LIMIT. Only the fixed-width row and byte counts leave before admission.
macro_rules! probe_sql {
    ($budget:expr, $statement:expr, $limit:literal, $remaining:literal, $($field:literal),+) => {
        join_text($budget, &[
            "SELECT row_count, used::int FROM (SELECT count(*) AS row_count, COALESCE(bool_and(true",
            $(concat!(" AND COALESCE(octet_length(", $field, "), 0) <= 1024"),)+
            "), true) AS fields_ok, COALESCE(sum(0::bigint",
            $(concat!(" + COALESCE(octet_length(", $field, "), 0)::bigint"),)+
            "), 0) AS used FROM (", $statement, ") AS page) AS admission",
            concat!(" WHERE row_count < ", $limit, " AND fields_ok AND used <= ", $remaining),
        ])
    };
}

async fn preflight(
    tx: &PgTransaction<'_>,
    probe: &str,
    params: &[Param<'_>],
    limit: i64,
    remaining: &mut i32,
) -> Result<usize, StoreError> {
    // Append the remaining text allowance without allocating a bind list.
    let available = *remaining;
    let allowance: Param<'_> = (&available, Type::INT4);
    let mut binds: [_; 4] = std::array::from_fn(|_| allowance.clone());
    if params.len() >= binds.len() {
        return Err(StoreError);
    }
    binds[..params.len()].clone_from_slice(params);
    admit_page(tx, probe, &binds[..=params.len()], limit, remaining).await
}

fn text(budget: &mut ConstructionBudget, value: &str) -> Result<String, StoreError> {
    join_text(budget, &[value])
}

fn optional_text(
    budget: &mut ConstructionBudget,
    row: &Row,
    column: &str,
) -> Result<Option<String>, StoreError> {
    row.try_get::<_, Option<&str>>(column)
        .map_err(|_| StoreError)?
        .map(|value| text(budget, value))
        .transpose()
}

async fn read_mirror(
    client: &mut impl PgConnect,
    budget: &mut ConstructionBudget,
    member: MirrorSelection<'_>,
    cap: i64,
) -> Result<CatalogueRows, StoreError> {
    let limit = cap
        .checked_add(1)
        .filter(|_| (0..=MIRROR_ROWS).contains(&cap))
        .ok_or(StoreError)?;
    let mut remaining = member.remaining_text_bytes;
    let tx = client.read_only_snapshot().await.map_err(|_| StoreError)?;
    tx.set_local_subsonic_timeouts()
        .await
        .map_err(|_| StoreError)?;
    // octet_length measures server bytes, while binary text uses the client encoding.
    let utf8 = "SELECT getdatabaseencoding() = 'UTF8' AND pg_client_encoding() = 'UTF8'";
    let encoding = tx
        .query_typed_one(utf8, &[])
        .await
        .map_err(|_| StoreError)?;
    if !encoding.try_get::<_, bool>(0).map_err(|_| StoreError)? {
        return Err(StoreError);
    }
    let params = [
        (&member.releases as &(dyn ToSql + Sync), Type::TEXT_ARRAY),
        (&limit, Type::INT8),
    ];
    let query = catalogue::artists_list_statement();
    let probe = probe_sql!(
        budget,
        query,
        "$2",
        "$3",
        "mbid",
        "name",
        "sort_name",
        "type",
        "country"
    )?;
    let count = preflight(&tx, &probe, &params, limit, &mut remaining).await?;
    drop(probe);
    let artists = fetch_page(&tx, budget, query, &params, count, |budget, row| {
        Ok(Some(ArtistDbRow {
            mbid: text(budget, row.try_get("mbid").map_err(|_| StoreError)?)?,
            name: text(budget, row.try_get("name").map_err(|_| StoreError)?)?,
            sort_name: text(budget, row.try_get("sort_name").map_err(|_| StoreError)?)?,
            artist_type: optional_text(budget, row, "type")?,
            country: optional_text(budget, row, "country")?,
        }))
    })
    .await?;
    let query = catalogue::releases_list_statement();
    let probe = probe_sql!(
        budget,
        query,
        "$2",
        "$3",
        "mbid",
        "title",
        "release_type",
        "artist_mbid",
        "artist_name"
    )?;
    let count = preflight(&tx, &probe, &params, limit, &mut remaining).await?;
    drop(probe);
    let releases = fetch_page(&tx, budget, query, &params, count, |budget, row| {
        Ok(Some(ReleaseDbRow {
            mbid: text(budget, row.try_get("mbid").map_err(|_| StoreError)?)?,
            title: text(budget, row.try_get("title").map_err(|_| StoreError)?)?,
            release_type: optional_text(budget, row, "release_type")?,
            year: row.try_get("year").map_err(|_| StoreError)?,
            artist_mbid: optional_text(budget, row, "artist_mbid")?,
            artist_name: optional_text(budget, row, "artist_name")?,
        }))
    })
    .await?;

    // Borrow both aligned columns from the charged pair strings, keeping the
    // normalized pairs for the independent exact-occurrence fence below.
    let pair_count = member.pairs.len();
    let mut pair_releases = budget.allocate_exact(pair_count).map_err(|_| StoreError)?;
    let mut pair_recordings = budget.allocate_exact(pair_count).map_err(|_| StoreError)?;
    for (release, recording) in member.pairs {
        pair_releases.push(release.as_str());
        pair_recordings.push(recording.as_str());
    }
    let query = catalogue::tracks_list_statement();
    let probe = probe_sql!(
        budget,
        query,
        "$3",
        "$4",
        "release_mbid",
        "recording_mbid",
        "title",
        "release_title",
        "artist_mbid",
        "artist_name"
    )?;
    let params: [Param<'_>; 3] = [
        (&pair_releases, Type::TEXT_ARRAY),
        (&pair_recordings, Type::TEXT_ARRAY),
        (&limit, Type::INT8),
    ];
    let count = preflight(&tx, &probe, &params, limit, &mut remaining).await?;
    drop(probe);
    let tracks = fetch_page(&tx, budget, query, &params, count, |budget, row| {
        let release: &str = row.try_get("release_mbid").map_err(|_| StoreError)?;
        let recording: &str = row.try_get("recording_mbid").map_err(|_| StoreError)?;
        if member
            .pairs
            .binary_search_by(|(r, t)| (r.as_str(), t.as_str()).cmp(&(release, recording)))
            .is_err()
        {
            return Err(StoreError);
        }
        Ok(Some(TrackDbRow {
            release_mbid: text(budget, release)?,
            recording_mbid: text(budget, recording)?,
            title: text(budget, row.try_get("title").map_err(|_| StoreError)?)?,
            duration_ms: row.try_get("duration_ms").map_err(|_| StoreError)?,
            medium_no: row.try_get("medium_no").map_err(|_| StoreError)?,
            track_no: row.try_get("track_no").map_err(|_| StoreError)?,
            release_title: text(
                budget,
                row.try_get("release_title").map_err(|_| StoreError)?,
            )?,
            artist_mbid: optional_text(budget, row, "artist_mbid")?,
            artist_name: optional_text(budget, row, "artist_name")?,
        }))
    })
    .await?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok((artists, releases, tracks))
}

pub(crate) async fn load_staging_mirror<F>(
    budget: &mut ConstructionBudget,
    member: MirrorSelection<'_>,
    connect_isolated: impl FnOnce() -> F,
) -> Result<CatalogueRows, StagingRejection>
where
    F: Future<Output = Result<PgClient, StagingRejection>>,
{
    member.validate()?;
    if member.releases.is_empty() && member.pairs.is_empty() {
        return Ok(CatalogueRows::default());
    }
    let mut client = with_deadline(
        async { connect_isolated().await },
        2_000,
        StagingRejection::BusyOrCapacity,
    )
    .await?;
    with_deadline(
        read_mirror(&mut client, budget, member, MIRROR_ROWS),
        1_000,
        StoreError,
    )
    .await
    .map_err(|_| StagingRejection::BusyOrCapacity)
}

#[cfg(test)]
#[tokio::test]
async fn malformed_membership_never_calls_the_isolated_mirror_connector() {
    let a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned();
    let b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned();
    let pair = (a.clone(), b.clone());
    let releases = [a.clone()];
    let pairs = [pair.clone()];
    let valid = MirrorSelection {
        releases: &releases,
        pairs: &pairs,
        remaining_text_bytes: MIRROR_TEXT_BYTES,
    };
    assert!(valid.validate().is_ok());
    let oversized = vec![a.clone(); MIRROR_ROWS as usize + 1];
    for member in [
        MirrorSelection {
            releases: &[],
            ..valid
        },
        MirrorSelection {
            releases: &[b, a.clone()],
            ..valid
        },
        MirrorSelection {
            releases: &[a.to_ascii_uppercase()],
            ..valid
        },
        MirrorSelection {
            releases: &oversized,
            ..valid
        },
        MirrorSelection {
            pairs: &[(a, "invalid".into())],
            ..valid
        },
        MirrorSelection {
            pairs: &[pair.clone(), pair],
            ..valid
        },
        MirrorSelection {
            remaining_text_bytes: -1,
            ..valid
        },
        MirrorSelection {
            remaining_text_bytes: MIRROR_TEXT_BYTES + 1,
            ..valid
        },
    ] {
        let mut budget = ConstructionBudget::new(0);
        let result = load_staging_mirror::<std::future::Ready<_>>(&mut budget, member, || {
            panic!("invalid membership must fail before mirror connection")
        })
        .await;
        assert!(matches!(result, Err(StagingRejection::BusyOrCapacity)));
        assert_eq!(budget.charged(), 0);
    }
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "subsonic_mirror_pg_tests.rs"]
mod postgres_tests;

#[cfg(test)]
#[tokio::test]
async fn empty_membership_never_calls_the_isolated_mirror_connector() {
    let mut budget = ConstructionBudget::new(0);
    let member = MirrorSelection {
        releases: &[],
        pairs: &[],
        remaining_text_bytes: 0,
    };
    let rows = load_staging_mirror::<std::future::Ready<_>>(&mut budget, member, || {
        panic!("empty membership must not open a mirror connection")
    })
    .await
    .unwrap();
    assert!(rows.0.is_empty() && rows.1.is_empty() && rows.2.is_empty());
    assert_eq!(budget.charged(), 0);
}
