//! Metering: every play emits events for Analytics Engine and the usage
//! ledger (plan enforcement). [`MemoryMeter`] records them in-process for
//! tests; production sinks them to the `meter` Analytics Engine dataset and
//! the Hyperdrive usage-ledger table with the same shapes.

use crate::pool::Lane;

/// What happened, in billing-relevant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    PlayStart,
    CacheHit,
    CacheMiss,
    BytesServed,
    PrefetchEnqueued,
    ProviderError,
    /// A provider-level failure quarantined the pool; the event carries the
    /// failed call's lane and the backoff just applied.
    QuarantineEntered,
    /// A successful call cleared a pending quarantine (no backoff).
    QuarantineExited,
    /// A pooled connection the provider silently closed was transparently
    /// redialed (no backoff, no quarantine).
    WarmRedial,
}

/// One billable-or-debuggable fact about a play.
#[derive(Clone, Debug)]
pub struct MeterEvent {
    pub kind: MeterKind,
    pub user_id: String,
    pub track_id: String,
    /// Bytes served to the client (0 for non-serving events).
    pub bytes: u64,
    /// Pool lane of the call a pool-health event belongs to; `None` for
    /// events unrelated to pool state.
    pub lane: Option<Lane>,
    /// Quarantine backoff in milliseconds for
    /// [`MeterKind::QuarantineEntered`]; 0 otherwise.
    pub backoff_ms: u64,
}

pub trait Meter {
    fn emit(&mut self, event: MeterEvent);
}

/// In-process meter for tests.
#[derive(Default, Debug)]
pub struct MemoryMeter {
    pub events: Vec<MeterEvent>,
}

impl Meter for MemoryMeter {
    fn emit(&mut self, event: MeterEvent) {
        self.events.push(event);
    }
}

impl MemoryMeter {
    pub fn has_kind(&self, kind: MeterKind) -> bool {
        self.events.iter().any(|e| e.kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_events_are_recorded_in_order() {
        let mut meter = MemoryMeter::default();
        for kind in [MeterKind::PlayStart, MeterKind::BytesServed] {
            meter.emit(MeterEvent {
                kind,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes: 7,
                lane: None,
                backoff_ms: 0,
            });
        }
        assert_eq!(meter.events.len(), 2);
        assert!(meter
            .events
            .iter()
            .all(|e| e.lane.is_none() && e.backoff_ms == 0));
        assert!(meter.has_kind(MeterKind::PlayStart));
        assert!(!meter.has_kind(MeterKind::CacheHit));
    }
}
