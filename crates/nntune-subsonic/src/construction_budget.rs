//! Checked accounting primitive for a caller-supplied construction budget.
//!
//! Callers must charge every covered allocation before allocating. This ledger
//! bounds the charged construction graph, not total runtime memory.

#[derive(Debug, PartialEq, Eq)]
pub struct BudgetExceeded;

/// A conservative, cumulative ledger: charges are not returned on drop.
pub struct ConstructionBudget {
    limit: usize,
    charged: usize,
}

impl ConstructionBudget {
    pub fn new(limit: usize) -> Self {
        Self { limit, charged: 0 }
    }

    pub fn charged(&self) -> usize {
        self.charged
    }

    /// Charge one allocation before allocating; failure leaves the ledger unchanged.
    ///
    /// Pinned Rust's rust-src `vendor/dlmalloc-0.2.13/src/dlmalloc.rs` defines
    /// 8-byte alignment, 4-byte chunk overhead, and a 16-byte minimum chunk on
    /// wasm32. Reject larger or invalid alignments rather than undercharge them.
    pub fn charge(
        &mut self,
        capacity: usize,
        element_size: usize,
        alignment: usize,
    ) -> Result<(), BudgetExceeded> {
        if !alignment.is_power_of_two() || alignment > 8 {
            return Err(BudgetExceeded);
        }
        let bytes = capacity
            .checked_mul(element_size)
            .and_then(|bytes| bytes.checked_add(4))
            .and_then(|bytes| bytes.checked_add(7))
            .map(|bytes| (bytes & !7).max(16))
            .and_then(|bytes| self.charged.checked_add(bytes))
            .filter(|total| *total <= self.limit)
            .ok_or(BudgetExceeded)?;
        self.charged = bytes;
        Ok(())
    }
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
pub mod tests {
    use super::*;

    #[cfg_attr(test, test)]
    pub fn rounds_each_allocation_and_fails_closed_at_limit() {
        let mut budget = ConstructionBudget::new(56);
        assert_eq!(budget.charge(1, 1, 1), Ok(()));
        assert_eq!(budget.charge(12, 1, 4), Ok(()));
        assert_eq!(budget.charge(13, 1, 8), Ok(()));
        assert_eq!(budget.charged(), 56);
        assert_eq!(budget.charge(1, 1, 1), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 56);
    }

    #[cfg_attr(test, test)]
    pub fn rejects_alignment_and_arithmetic_overflow_without_changing_charge() {
        let mut budget = ConstructionBudget::new(usize::MAX);
        assert_eq!(budget.charge(1, 1, 1), Ok(()));
        for (capacity, size, alignment) in [
            (1, 1, 0),
            (1, 1, 3),
            (1, 1, 16),
            (usize::MAX, 2, 1),
            (usize::MAX, 1, 1),
            (usize::MAX - 4, 1, 1),
            (usize::MAX - 15, 1, 1),
        ] {
            assert_eq!(
                budget.charge(capacity, size, alignment),
                Err(BudgetExceeded)
            );
        }
        assert_eq!(budget.charged(), 16);
    }
}
