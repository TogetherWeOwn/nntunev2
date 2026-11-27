use nntune_subsonic::construction_budget::BudgetExceeded;

use crate::subsonic_dispatch::snapshot_order::{order_work, OrderWork};

#[allow(dead_code)]
pub(super) fn check_order_work(
    artists: usize,
    releases: usize,
    tracks: usize,
    work: OrderWork,
) -> Result<OrderWork, BudgetExceeded> {
    let needed = order_work(artists, releases, tracks, artists).ok_or(BudgetExceeded)?;
    // Byte preflight, output planning and mapping each scan artists for every
    // release. Mapping also scans tracks per release and releases per track.
    let lookups = releases
        .checked_mul(artists)
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| releases.checked_mul(tracks)?.checked_mul(2)?.checked_add(n))
        .ok_or(BudgetExceeded)?;
    let comparisons = work
        .comparisons
        .checked_sub(lookups)
        .ok_or(BudgetExceeded)?;
    if needed.comparisons > comparisons || needed.artist_row_probes > work.artist_row_probes {
        return Err(BudgetExceeded);
    }
    Ok(OrderWork {
        comparisons,
        ..work
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_needs_no_ordering_work() {
        let zero = OrderWork {
            comparisons: 0,
            artist_row_probes: 0,
        };
        assert!(check_order_work(0, 0, 0, zero).is_ok());
        assert!(check_order_work(1, 1, 0, zero).is_err());
        assert!(check_order_work(0, 1, 1, zero).is_err());
    }

    #[test]
    fn work_limit_includes_all_preflight_and_mapping_scans() {
        let ordering = order_work(2, 1, 1, 2).unwrap();
        let exact = OrderWork {
            comparisons: ordering.comparisons + 3 * 2 + 2,
            artist_row_probes: ordering.artist_row_probes,
        };
        assert_eq!(check_order_work(2, 1, 1, exact), Ok(ordering));
        assert!(check_order_work(
            2,
            1,
            1,
            OrderWork {
                comparisons: exact.comparisons - 1,
                ..exact
            }
        )
        .is_err());
        assert!(exact.artist_row_probes > 0);
        assert!(check_order_work(
            2,
            1,
            1,
            OrderWork {
                artist_row_probes: exact.artist_row_probes - 1,
                ..exact
            }
        )
        .is_err());
        assert!(check_order_work(
            usize::MAX,
            2,
            0,
            OrderWork {
                comparisons: usize::MAX,
                artist_row_probes: usize::MAX,
            }
        )
        .is_err());
    }
}
