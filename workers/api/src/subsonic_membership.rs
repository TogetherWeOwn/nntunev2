//! Unmounted membership read. The app and mirror have separate snapshots.

use super::{with_deadline, StoreError};
use crate::catalogue::{
    catalogue_member_releases_statement, catalogue_member_track_pairs_statement, is_catalogue_mbid,
};
use crate::subsonic_auth::Principal;
use futures_util::TryStreamExt;
use nntune_subsonic::{
    construction_budget::ConstructionBudget, staging_rejection::StagingRejection,
};
use tokio_postgres::{
    types::{ToSql, Type},
    Client, Row, Transaction,
};

const MEMBERSHIP_ROWS: i64 = 1_024;
const SOURCE_TEXT_BYTES: i32 = 256 * 1_024;

pub(crate) struct StagingMembership {
    pub(crate) releases: Vec<String>,
    pub(crate) pairs: Vec<(String, String)>,
    pub(crate) remaining_text_bytes: i32,
}

fn sql(budget: &mut ConstructionBudget, parts: &[&str]) -> Result<String, StoreError> {
    let len = parts.iter().try_fold(0usize, |sum, part| {
        sum.checked_add(part.len()).ok_or(StoreError)
    })?;
    let mut bytes = budget.allocate_exact(len).map_err(|_| StoreError)?;
    for part in parts {
        bytes.extend_from_slice(part.as_bytes());
    }
    String::from_utf8(bytes).map_err(|_| StoreError)
}

// Keep the old ID transfer guard (36 bytes) and reject a stored field over
// 1 KiB before fetching. Count the bytes actually projected, not sentinels.
const RELEASE_BYTES: &str =
    "CASE WHEN octet_length(release_mbid) <= 36 THEN octet_length(release_mbid) ELSE 0 END";
const RECORDING_BYTES: &str =
    " + CASE WHEN octet_length(recording_mbid) <= 36 THEN octet_length(recording_mbid) ELSE 0 END";
const PROJECT_RELEASE: &str =
    "CASE WHEN octet_length(release_mbid) <= 36 THEN release_mbid ELSE '' END";
const PROJECT_RECORDING: &str =
    ", CASE WHEN octet_length(recording_mbid) <= 36 THEN recording_mbid ELSE '' END";

async fn preflight(
    tx: &Transaction<'_>,
    budget: &mut ConstructionBudget,
    statement: &str,
    user_id: &str,
    limit: i64,
    pairs: bool,
    remaining: &mut i32,
) -> Result<usize, StoreError> {
    let query = sql(
        budget,
        &[
            "SELECT CASE WHEN row_count < $2 AND fields_ok AND used <= $3 THEN row_count END, used::int \
             FROM (SELECT count(*) AS row_count, COALESCE(bool_and(octet_length(release_mbid) <= 1024",
            if pairs { " AND COALESCE(octet_length(recording_mbid), 0) <= 1024" } else { "" },
            "), true) AS fields_ok, COALESCE(sum(",
            RELEASE_BYTES,
            if pairs { RECORDING_BYTES } else { "" },
            "), 0) AS used FROM (",
            statement,
            " LIMIT $2) AS membership) AS admission",
        ],
    )?;
    let row = tx
        .query_typed_one(
            &query,
            &[
                (&user_id, Type::TEXT),
                (&limit, Type::INT8),
                (&*remaining, Type::INT4),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let count: i64 = row.try_get(0).map_err(|_| StoreError)?;
    let used: i32 = row.try_get(1).map_err(|_| StoreError)?;
    *remaining = remaining
        .checked_sub(used)
        .filter(|n| *n >= 0 && used >= 0)
        .ok_or(StoreError)?;
    usize::try_from(count).map_err(|_| StoreError)
}

async fn fetch<T: Ord>(
    tx: &Transaction<'_>,
    budget: &mut ConstructionBudget,
    statement: &str,
    user_id: &str,
    (count, pairs): (usize, bool),
    #[cfg(test)] fetches: &mut usize,
    mut decode: impl FnMut(&mut ConstructionBudget, &Row) -> Result<Option<T>, StoreError>,
) -> Result<Vec<T>, StoreError> {
    let query = sql(
        budget,
        &[
            "SELECT ",
            PROJECT_RELEASE,
            if pairs { PROJECT_RECORDING } else { "" },
            " FROM (",
            statement,
            " LIMIT $2) AS membership",
        ],
    )?;
    // Charge the exact output slots before starting the ID query. The driver
    // owns its buffers; the projection holds only one transient row at a time.
    let mut projected = budget.allocate_exact(count).map_err(|_| StoreError)?;
    let limit = count as i64 + 1; // preflight bounded count by MEMBERSHIP_ROWS
    #[cfg(test)]
    {
        *fetches += 1;
    }
    let params: [(&(dyn ToSql + Sync), Type); 2] = [(&user_id, Type::TEXT), (&limit, Type::INT8)];
    let rows = tx
        .query_typed_raw(&query, params)
        .await
        .map_err(|_| StoreError)?;
    let mut rows = std::pin::pin!(rows);
    let mut seen = 0;
    while let Some(row) = rows.try_next().await.map_err(|_| StoreError)? {
        if seen == count {
            return Err(StoreError);
        }
        seen += 1;
        if let Some(value) = decode(budget, &row)? {
            projected.push(value);
        }
    }
    if seen != count {
        return Err(StoreError);
    }
    normalize_order(&mut projected);
    Ok(projected)
}

fn copy_id(budget: &mut ConstructionBudget, value: &str) -> Result<String, StoreError> {
    let mut bytes = budget.allocate_exact(value.len()).map_err(|_| StoreError)?;
    bytes.extend_from_slice(value.as_bytes());
    bytes.make_ascii_lowercase();
    String::from_utf8(bytes).map_err(|_| StoreError)
}

fn normalize_order<T: Ord>(ids: &mut Vec<T>) {
    // Unstable sort is in-place. Deduplicate after ASCII normalization.
    ids.sort_unstable();
    ids.dedup();
}

pub(super) async fn read_membership(
    client: &mut Client,
    budget: &mut ConstructionBudget,
    user_id: &str,
    cap: i64,
    mut remaining: i32,
    #[cfg(test)] fetches: &mut usize,
) -> Result<StagingMembership, StoreError> {
    let limit = cap
        .checked_add(1)
        .filter(|_| (0..=MEMBERSHIP_ROWS).contains(&cap))
        .ok_or(StoreError)?;
    let tx = client
        .build_transaction()
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await
        .map_err(|_| StoreError)?;
    tx.batch_execute("SET LOCAL statement_timeout = '1s'; SET LOCAL lock_timeout = '1s'")
        .await
        .map_err(|_| StoreError)?;
    let releases = catalogue_member_releases_statement();
    let pairs = catalogue_member_track_pairs_statement();
    let release_count =
        preflight(&tx, budget, releases, user_id, limit, false, &mut remaining).await?;
    let pair_count = preflight(&tx, budget, pairs, user_id, limit, true, &mut remaining).await?;
    let releases = fetch(
        &tx,
        budget,
        releases,
        user_id,
        (release_count, false),
        #[cfg(test)]
        fetches,
        |budget, row| {
            let release: &str = row.try_get(0).map_err(|_| StoreError)?;
            is_catalogue_mbid(release)
                .then(|| copy_id(budget, release))
                .transpose()
        },
    )
    .await?;
    let pairs = fetch(
        &tx,
        budget,
        pairs,
        user_id,
        (pair_count, true),
        #[cfg(test)]
        fetches,
        |budget, row| {
            let release: &str = row.try_get(0).map_err(|_| StoreError)?;
            let recording: &str = row.try_get(1).map_err(|_| StoreError)?;
            if !is_catalogue_mbid(release) || !is_catalogue_mbid(recording) {
                return Ok(None);
            }
            Ok(Some((
                copy_id(budget, release)?,
                copy_id(budget, recording)?,
            )))
        },
    )
    .await?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(StagingMembership {
        releases,
        pairs,
        remaining_text_bytes: remaining,
    })
}

pub(crate) async fn load_staging_membership(
    budget: &mut ConstructionBudget,
    client: &mut Client,
    principal: &Principal,
) -> Result<StagingMembership, StagingRejection> {
    // The existing authenticated app client is reused. The deadline does not
    // prove cancellation or driver teardown.
    with_deadline(
        read_membership(
            client,
            budget,
            principal.user_id(),
            MEMBERSHIP_ROWS,
            SOURCE_TEXT_BYTES,
            #[cfg(test)]
            &mut 0,
        ),
        1_000,
        StoreError,
    )
    .await
    .map_err(|_| StagingRejection::BusyOrCapacity)
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "ledger-wasm-test")))]
#[cfg_attr(test, test)]
pub(crate) fn membership_projection_charges_capacity_before_copying() {
    const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const UPPER: &str = "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA";
    let rows = [UPPER, ID, "invalid"];
    let mut expected = ConstructionBudget::new(usize::MAX);
    expected.charge(5, 1, 1).unwrap(); // prior input phase
    expected
        .charge(
            rows.len(),
            std::mem::size_of::<String>(),
            std::mem::align_of::<String>(),
        )
        .unwrap();
    for _ in 0..2 {
        expected.charge(ID.len(), 1, 1).unwrap();
    }
    let charge = expected.charged();
    for limit in [charge - 1, charge] {
        let mut budget = ConstructionBudget::new(limit);
        budget.charge(5, 1, 1).unwrap();
        let result = (|| -> Result<_, StoreError> {
            let mut ids = budget.allocate_exact(rows.len()).map_err(|_| StoreError)?;
            for value in rows.into_iter().filter(|id| is_catalogue_mbid(id)) {
                ids.push(copy_id(&mut budget, value)?);
            }
            normalize_order(&mut ids);
            Ok(ids)
        })();
        if limit < charge {
            assert!(result.is_err());
        } else {
            let result = result.unwrap();
            assert_eq!(result.as_slice(), [ID]);
            assert_eq!(result.capacity(), rows.len());
            assert_eq!(result[0].capacity(), ID.len());
            assert_eq!(budget.charged(), charge);
        }
    }
}
