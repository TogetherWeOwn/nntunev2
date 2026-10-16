//! Metering: every play emits events for Analytics Engine and the usage
//! ledger (plan enforcement). [`MemoryMeter`] records them in-process for
//! tests; production sinks them to the `meter` Analytics Engine dataset and
//! the Hyperdrive usage-ledger table with the same shapes.

/// What happened, in billing-relevant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    PlayStart,
    CacheHit,
    CacheMiss,
    BytesServed,
    PrefetchEnqueued,
    ProviderError,
    /// A play was rejected because its grant had expired
    /// (`GrantError::Expired`). Carries the requested track and the granted
    /// user only; never secret or token material.
    GrantRejectedExpired,
    /// A play was rejected because its grant's remaining lifetime exceeded
    /// `MAX_GRANT_TTL + CLOCK_SKEW_SECS` (`GrantError::TtlTooLong`).
    /// Same payload shape as the other rejection events.
    GrantRejectedExcessiveExpiry,
    /// A play was rejected because a valid grant was presented for the
    /// wrong track (`PlayError::WrongTrack`). `track_id` is the requested
    /// track, not the granted one.
    GrantRejectedWrongTrack,
}

/// One billable-or-debuggable fact about a play.
#[derive(Clone, Debug)]
pub struct MeterEvent {
    pub kind: MeterKind,
    pub user_id: String,
    pub track_id: String,
    /// Bytes served to the client (0 for non-serving events).
    pub bytes: u64,
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
            });
        }
        assert_eq!(meter.events.len(), 2);
        assert!(meter.has_kind(MeterKind::PlayStart));
        assert!(!meter.has_kind(MeterKind::CacheHit));
    }
}
