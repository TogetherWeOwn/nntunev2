//! Non-waiting admission for construction within one isolate.
//!
//! This is inert until a caller acquires the shared slot. It does not bound
//! other isolates, other routes, or buffers retained after the permit is dropped.

use std::sync::atomic::{AtomicBool, Ordering};

/// The single construction slot in this instance of the module.
pub static CONSTRUCTION_SLOT: ConstructionSlot = ConstructionSlot::new();

pub struct ConstructionSlot {
    occupied: AtomicBool,
}

impl ConstructionSlot {
    const fn new() -> Self {
        Self {
            occupied: AtomicBool::new(false),
        }
    }

    /// Acquire immediately, or return `None` without waiting or queuing.
    #[must_use]
    pub fn try_acquire(&self) -> Option<ConstructionPermit<'_>> {
        self.occupied
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| ConstructionPermit { slot: self })
    }
}

/// Releases the construction slot on drop, including error returns and unwinds.
#[must_use]
pub struct ConstructionPermit<'a> {
    slot: &'a ConstructionSlot,
}

impl Drop for ConstructionPermit<'_> {
    fn drop(&mut self) {
        self.slot.occupied.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn contention_rejects_immediately_and_drop_allows_reacquisition() {
        let slot = Arc::new(ConstructionSlot::new());
        let permit = slot.try_acquire().expect("first acquisition");
        let barrier = Arc::new(Barrier::new(2));
        let contender_slot = Arc::clone(&slot);
        let contender_barrier = Arc::clone(&barrier);
        let contender = std::thread::spawn(move || {
            contender_barrier.wait();
            assert!(contender_slot.try_acquire().is_none());
        });
        barrier.wait();
        contender.join().unwrap();
        drop(permit);
        assert!(slot.try_acquire().is_some());
    }

    #[test]
    fn error_return_releases_slot() {
        fn fail(slot: &ConstructionSlot) -> Result<(), ()> {
            let _permit = slot.try_acquire().ok_or(())?;
            Err(())
        }
        let slot = ConstructionSlot::new();
        assert_eq!(fail(&slot), Err(()));
        assert!(slot.try_acquire().is_some());
    }

    #[test]
    fn unwind_releases_slot() {
        let slot = ConstructionSlot::new();
        let result = std::panic::catch_unwind(|| {
            let _permit = slot.try_acquire().unwrap();
            panic!("test unwind");
        });
        assert!(result.is_err());
        assert!(slot.try_acquire().is_some());
    }
}
