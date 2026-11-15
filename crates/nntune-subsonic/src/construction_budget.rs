//! Checked accounting primitive for a caller-supplied construction budget.
//!
//! This ledger does not discover allocations or allocator overhead. Callers must
//! charge every allocation, including any substantiated rounding and metadata,
//! before allocating, and must not treat this as a bound on runtime memory.

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

    /// Charge the requested element capacity and separately established overhead.
    /// A failed charge leaves the ledger unchanged.
    pub fn charge(
        &mut self,
        capacity: usize,
        element_size: usize,
        overhead: usize,
    ) -> Result<(), BudgetExceeded> {
        let bytes = capacity
            .checked_mul(element_size)
            .and_then(|bytes| bytes.checked_add(overhead))
            .and_then(|bytes| self.charged.checked_add(bytes))
            .filter(|total| *total <= self.limit)
            .ok_or(BudgetExceeded)?;
        self.charged = bytes;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_exact_limit_and_rejects_one_over_without_changing_charge() {
        let mut budget = ConstructionBudget::new(12);
        assert_eq!(budget.charge(2, 4, 1), Ok(()));
        assert_eq!(budget.charge(3, 1, 0), Ok(()));
        assert_eq!(budget.charged(), 12);
        assert_eq!(budget.charge(1, 1, 0), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 12);
    }

    #[test]
    fn rejects_every_arithmetic_overflow_without_changing_charge() {
        let mut budget = ConstructionBudget::new(usize::MAX);
        assert_eq!(budget.charge(1, 1, 0), Ok(()));
        assert_eq!(budget.charge(usize::MAX, 2, 0), Err(BudgetExceeded));
        assert_eq!(budget.charge(usize::MAX, 1, 1), Err(BudgetExceeded));
        assert_eq!(budget.charge(usize::MAX, 1, 0), Err(BudgetExceeded));
        assert_eq!(budget.charged(), 1);
    }
}
