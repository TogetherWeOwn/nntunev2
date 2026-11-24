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

    /// Narrow the remaining allowance without refunding any prior charge or
    /// raising the original ceiling.
    pub fn restrict_remaining(&mut self, bytes: usize) {
        self.limit = self.charged + bytes.min(self.limit - self.charged);
    }

    /// Charge and reserve a fresh vector. Callers may fill it up to the
    /// requested capacity; any later growth needs its own pre-allocation charge.
    pub fn allocate_exact<T>(&mut self, capacity: usize) -> Result<Vec<T>, BudgetExceeded> {
        let mut out = Vec::new();
        if capacity != 0 && std::mem::size_of::<T>() != 0 {
            self.charge(
                capacity,
                std::mem::size_of::<T>(),
                std::mem::align_of::<T>(),
            )?;
            out.try_reserve_exact(capacity)
                .map_err(|_| BudgetExceeded)?;
            if out.capacity() != capacity {
                return Err(BudgetExceeded);
            }
        }
        Ok(out)
    }

    /// Charge one allocation before allocating; failure leaves the ledger unchanged.
    ///
    /// Pinned Rust's rust-src `vendor/dlmalloc-0.2.13/src/dlmalloc.rs` defines
    /// 8-byte alignment, 4-byte chunk overhead, and a 16-byte minimum chunk on
    /// wasm32. Add 8 bytes for an unsplittable remainder or the next small bin.
    /// Reject larger or invalid alignments rather than undercharge them.
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
            .and_then(|bytes| bytes.checked_add(8))
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
    pub fn exact_vector_reservation_checks_limit_and_overflow() {
        let mut short = ConstructionBudget::new(31);
        assert_eq!(short.allocate_exact::<u32>(4), Err(BudgetExceeded));
        assert_eq!(short.charged(), 0);
        let mut budget = ConstructionBudget::new(32);
        assert_eq!(
            budget.allocate_exact::<u32>(usize::MAX),
            Err(BudgetExceeded)
        );
        assert_eq!(budget.charged(), 0);
        assert_eq!(budget.allocate_exact::<u32>(0).unwrap().capacity(), 0);
        assert_eq!(budget.allocate_exact::<u32>(4).unwrap().capacity(), 4);
        assert_eq!(budget.charged(), 32);
    }

    #[cfg_attr(test, test)]
    pub fn rounds_each_allocation_and_fails_closed_at_limit() {
        let mut budget = ConstructionBudget::new(80);
        assert_eq!(budget.charge(1, 1, 1), Ok(()));
        assert_eq!(budget.charge(12, 1, 4), Ok(()));
        assert_eq!(budget.charge(13, 1, 8), Ok(()));
        assert_eq!(budget.charged(), 80);
        budget.restrict_remaining(usize::MAX);
        assert_eq!(budget.charge(1, 1, 1), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 80);

        let mut restricted = ConstructionBudget::new(8 * 1024 * 1024);
        restricted.charge(1, 1, 1).unwrap();
        restricted.restrict_remaining(23);
        restricted.restrict_remaining(24);
        assert_eq!(restricted.charge(1, 1, 1), Err(BudgetExceeded));
        assert_eq!(restricted.charged(), 24);
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
        assert_eq!(budget.charged(), 24);
    }
}
