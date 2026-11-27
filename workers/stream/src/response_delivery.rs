//! Ordered plaintext accounting shared by the playback response adapters.

use nntune_core::original_source::SourceError;

pub(crate) struct OrderedDelivery {
    expected: u64,
    accepted: u64,
    offered: usize,
}

impl OrderedDelivery {
    pub(crate) fn new(expected: u64) -> Self {
        Self {
            expected,
            accepted: 0,
            offered: 0,
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.offered != 0
    }

    pub(crate) fn offer(&mut self, bytes: usize) -> Result<(), SourceError> {
        if self.pending() || bytes as u64 > self.expected - self.accepted {
            return Err(SourceError::Bounds);
        }
        self.offered = bytes;
        Ok(())
    }

    pub(crate) fn acknowledge(&mut self, bytes: usize) -> Result<(), SourceError> {
        if bytes == 0 || bytes > self.offered {
            return Err(SourceError::Bounds);
        }
        self.accepted += bytes as u64;
        self.offered -= bytes;
        Ok(())
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.accepted
    }

    pub(crate) fn complete(&self) -> bool {
        !self.pending() && self.accepted == self.expected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_acknowledged_prefixes_count() {
        let mut delivery = OrderedDelivery::new(5);
        assert!(delivery.acknowledge(0).is_err());
        assert!(delivery.acknowledge(1).is_err());
        assert!(delivery.offer(6).is_err());
        delivery.offer(3).unwrap();
        assert!(delivery.acknowledge(0).is_err());
        assert_eq!(delivery.accepted(), 0);
        delivery.acknowledge(1).unwrap();
        assert!(delivery.offer(2).is_err());
        assert!(delivery.acknowledge(3).is_err());
        assert_eq!(delivery.accepted(), 1);
        delivery.acknowledge(2).unwrap();
        assert!(!delivery.complete());
        assert!(delivery.offer(3).is_err());
        delivery.offer(2).unwrap();
        delivery.acknowledge(2).unwrap();
        assert!(delivery.complete());
        assert!(delivery.acknowledge(1).is_err());
        assert_eq!(delivery.accepted(), 5);
    }
}
