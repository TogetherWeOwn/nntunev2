//! Project admitted staging pages without retaining a caller-owned row list.

use super::StoreError;
use futures_util::{Stream, StreamExt, TryStreamExt};
use nntune_subsonic::construction_budget::ConstructionBudget;
use std::{future::Future, pin::pin};
use tokio_postgres::{
    types::{ToSql, Type},
    Row, Transaction,
};

pub(super) type Param<'a> = (&'a (dyn ToSql + Sync), Type);

pub(super) async fn admit_page(
    tx: &Transaction<'_>,
    query: &str,
    params: &[Param<'_>],
    limit: i64,
    remaining: &mut i32,
) -> Result<usize, StoreError> {
    let row = tx
        .query_typed_one(query, params)
        .await
        .map_err(|_| StoreError)?;
    let count: i64 = row.try_get(0).map_err(|_| StoreError)?;
    let used: i32 = row.try_get(1).map_err(|_| StoreError)?;
    if !(0..limit).contains(&count) {
        return Err(StoreError);
    }
    *remaining = remaining
        .checked_sub(used)
        .filter(|n| *n >= 0 && used >= 0)
        .ok_or(StoreError)?;
    usize::try_from(count).map_err(|_| StoreError)
}

// The count must come from a probe over the same rows in the same snapshot.
pub(super) async fn fetch_page<T>(
    tx: &Transaction<'_>,
    budget: &mut ConstructionBudget,
    query: &str,
    params: &[Param<'_>],
    admitted: usize,
    decode: impl FnMut(&mut ConstructionBudget, &Row) -> Result<Option<T>, StoreError>,
) -> Result<Vec<T>, StoreError> {
    project_rows(
        budget,
        admitted,
        async {
            tx.query_typed_raw(query, params.iter().cloned())
                .await
                .map(|rows| rows.map_err(|_| StoreError))
                .map_err(|_| StoreError)
        },
        decode,
    )
    .await
}

pub(super) async fn project_rows<R, T, S>(
    budget: &mut ConstructionBudget,
    admitted: usize,
    start: impl Future<Output = Result<S, StoreError>>,
    mut decode: impl FnMut(&mut ConstructionBudget, &R) -> Result<Option<T>, StoreError>,
) -> Result<Vec<T>, StoreError>
where
    S: Stream<Item = Result<R, StoreError>>,
{
    let mut output = budget.allocate_exact(admitted).map_err(|_| StoreError)?;
    let mut rows = pin!(start.await?);
    let mut remaining = admitted;
    while let Some(row) = rows.next().await {
        // Count even filtered rows. Reject an extra row before decoding it or
        // growing the admitted backing allocation.
        remaining = remaining.checked_sub(1).ok_or(StoreError)?;
        if let Some(value) = decode(budget, &row?)? {
            output.push(value);
        }
    }
    if remaining != 0 {
        return Err(StoreError);
    }
    Ok(output)
}

#[cfg(any(test, all(target_arch = "wasm32", feature = "ledger-wasm-test")))]
#[cfg_attr(test, test)]
pub(crate) fn streamed_projection_enforces_admission() {
    use futures_util::{future, stream, FutureExt};
    use std::mem::{align_of, size_of};

    let mut exact = ConstructionBudget::new(usize::MAX);
    exact
        .charge(3, size_of::<u32>(), align_of::<u32>())
        .unwrap();
    let charge = exact.charged();
    for (admitted, limit) in [(3, charge), (3, charge - 1), (usize::MAX, usize::MAX)] {
        let mut budget = ConstructionBudget::new(limit);
        let mut started = false;
        let result = project_rows(
            &mut budget,
            admitted,
            async {
                started = true;
                Ok(stream::iter([Ok(2_u32), Ok(0), Ok(1)]))
            },
            |_, value| Ok((*value != 0).then_some(*value)),
        )
        .now_or_never()
        .unwrap();
        if limit == charge {
            let page = result.unwrap();
            assert_eq!(page, [2, 1]);
            assert_eq!(page.capacity(), 3);
            assert_eq!(budget.charged(), charge);
            assert!(started);
        } else {
            assert!(result.is_err());
            assert_eq!(budget.charged(), 0);
            assert!(!started);
        }
    }

    // Reject early EOF, an extra row, and a stream error within the admitted
    // count after retaining a row. An extra row must not reach the decoder.
    for (values, count) in [(&[0][..], 2), (&[0, 1], 1), (&[1, -1], 2)] {
        let mut budget = ConstructionBudget::new(charge);
        let mut decoded = 0;
        let result = project_rows(
            &mut budget,
            count,
            future::ready(Ok(stream::iter(
                values
                    .iter()
                    .map(|&value| u32::try_from(value).map_err(|_| StoreError)),
            ))),
            |_, value| {
                decoded += 1;
                Ok((*value != 0).then_some(*value))
            },
        );
        assert!(result.now_or_never().unwrap().is_err());
        assert_eq!(decoded, 1);
    }
}
