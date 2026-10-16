//! Metering: every play emits events for Analytics Engine and the usage
//! ledger (plan enforcement). [`MemoryMeter`] records them in-process for
//! tests; production sinks them to the `meter` Analytics Engine dataset and
//! the Hyperdrive usage-ledger table with the same shapes.
//!
//! Two signals leave this module per served byte range:
//!
//! - [`AnalyticsPoint`]: the fast per-second/per-byte signal written to the
//!   Analytics Engine dataset [`METER_DATASET`] through the [`METER_DATASET_BINDING`]
//!   binding. Blobs carry identity, doubles carry magnitude.
//! - [`MeterEvent`] with [`MeterEvent::seconds`]: stream seconds attributed to
//!   the play at the nominal rate [`STREAM_BYTES_PER_SEC`]. The authoritative
//!   ledger row (PlanetScale via Hyperdrive, owned by the api worker) derives
//!   its seconds from the folded byte total ([`served_bytes`]) with the same
//!   rate. Per-event seconds each round up and are not additive: reconcile
//!   from bytes, never by summing event seconds.

/// Analytics Engine binding name: must match `[analytics_engine_datasets]`
/// in `workers/stream/wrangler.toml`.
pub const METER_DATASET_BINDING: &str = "METER";
/// Analytics Engine dataset receiving stream usage events.
pub const METER_DATASET: &str = "nntune_meter";

/// Nominal bytes per stream second: served bytes map to stream seconds at
/// this rate (roughly 384 kbit/s lossless-ish audio). This is a metering
/// primitive, not a bitrate claim about any track: it lets byte counts and
/// second counts stay mutually derivable without a per-track bitrate lookup.
/// The ledger side derives seconds from bytes with this same rate.
pub const STREAM_BYTES_PER_SEC: u64 = 48_000;

/// Stream seconds attributed to `bytes` served, rounding any partial second
/// up: a started second counts. Zero bytes attribute zero seconds.
pub fn stream_seconds_for_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(STREAM_BYTES_PER_SEC)
}

/// What happened, in billing-relevant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterKind {
    PlayStart,
    CacheHit,
    CacheMiss,
    BytesServed,
    PrefetchEnqueued,
    ProviderError,
}

/// Stable Analytics Engine blob for a [`MeterKind`]. Distinct kinds never
/// share a slug.
pub fn kind_slug(kind: MeterKind) -> &'static str {
    match kind {
        MeterKind::PlayStart => "play_start",
        MeterKind::CacheHit => "cache_hit",
        MeterKind::CacheMiss => "cache_miss",
        MeterKind::BytesServed => "bytes_served",
        MeterKind::PrefetchEnqueued => "prefetch_enqueued",
        MeterKind::ProviderError => "provider_error",
    }
}

/// One billable-or-debuggable fact about a play.
#[derive(Clone, Debug)]
pub struct MeterEvent {
    pub kind: MeterKind,
    pub user_id: String,
    pub track_id: String,
    /// Bytes served to the client (0 for non-serving events).
    pub bytes: u64,
    /// Stream seconds attributed ([`stream_seconds_for_bytes`]); 0 for
    /// non-serving events.
    pub seconds: u64,
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

/// One Analytics Engine point: blobs carry identity, doubles carry
/// magnitude, the index is the billed user. `doubles` holds
/// `[bytes, seconds]`; both stay exactly representable for every realistic
/// play (magnitudes far below 2^53).
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyticsPoint {
    pub dataset: &'static str,
    pub index: String,
    pub blobs: [String; 3],
    pub doubles: [f64; 2],
}

/// Build the [`AnalyticsPoint`] for one event. Blobs are
/// `[kind, user_id, track_id]`; the index is the user.
pub fn analytics_point(event: &MeterEvent) -> AnalyticsPoint {
    AnalyticsPoint {
        dataset: METER_DATASET,
        index: event.user_id.clone(),
        blobs: [
            kind_slug(event.kind).to_string(),
            event.user_id.clone(),
            event.track_id.clone(),
        ],
        doubles: [event.bytes as f64, event.seconds as f64],
    }
}

/// Total bytes served to (`user_id`, `track_id`) across `events`: the sum of
/// every `BytesServed` payload. The ledger row for a play derives from this
/// total, so splitting service across chunks, aborts or orphans never changes
/// the billed bytes.
pub fn served_bytes(events: &[MeterEvent], user_id: &str, track_id: &str) -> u64 {
    events
        .iter()
        .filter(|e| {
            e.kind == MeterKind::BytesServed && e.user_id == user_id && e.track_id == track_id
        })
        .map(|e| e.bytes)
        .fold(0u64, u64::saturating_add)
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
                seconds: stream_seconds_for_bytes(7),
            });
        }
        assert_eq!(meter.events.len(), 2);
        assert!(meter.has_kind(MeterKind::PlayStart));
        assert!(!meter.has_kind(MeterKind::CacheHit));
    }

    #[test]
    fn dataset_names_match_wrangler() {
        assert_eq!(METER_DATASET_BINDING, "METER");
        assert_eq!(METER_DATASET, "nntune_meter");
    }

    #[test]
    fn nominal_rate_is_pinned() {
        // Literal pin (not in terms of the constant): changing the rate
        // re-bills every historical byte count, so it must be deliberate.
        assert_eq!(STREAM_BYTES_PER_SEC, 48_000);
    }

    #[test]
    fn seconds_vectors() {
        assert_eq!(stream_seconds_for_bytes(0), 0);
        assert_eq!(stream_seconds_for_bytes(1), 1);
        assert_eq!(stream_seconds_for_bytes(47_999), 1);
        assert_eq!(stream_seconds_for_bytes(48_000), 1);
        assert_eq!(stream_seconds_for_bytes(48_001), 2);
        assert_eq!(stream_seconds_for_bytes(96_000), 2);
        assert_eq!(stream_seconds_for_bytes(96_001), 3);
        // No overflow at the top: div_ceil never adds before dividing.
        // u64::MAX = 18_446_744_073_709_551_615 = 48_000 * 384_307_168_202_282
        // + 41_615, so the ceiling is one past the quotient.
        assert_eq!(stream_seconds_for_bytes(u64::MAX), 384_307_168_202_283);
    }

    #[test]
    fn kind_slugs_are_distinct_snake_case() {
        let slugs = [
            kind_slug(MeterKind::PlayStart),
            kind_slug(MeterKind::CacheHit),
            kind_slug(MeterKind::CacheMiss),
            kind_slug(MeterKind::BytesServed),
            kind_slug(MeterKind::PrefetchEnqueued),
            kind_slug(MeterKind::ProviderError),
        ];
        assert_eq!(
            slugs,
            [
                "play_start",
                "cache_hit",
                "cache_miss",
                "bytes_served",
                "prefetch_enqueued",
                "provider_error"
            ]
        );
        let mut distinct = slugs.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), slugs.len());
    }

    #[test]
    fn analytics_point_carries_identity_in_blobs_and_magnitude_in_doubles() {
        let event = MeterEvent {
            kind: MeterKind::BytesServed,
            user_id: "user-1".to_string(),
            track_id: "track-9".to_string(),
            bytes: 100_000,
            seconds: stream_seconds_for_bytes(100_000),
        };
        let point = analytics_point(&event);
        assert_eq!(point.dataset, "nntune_meter");
        assert_eq!(point.index, "user-1");
        assert_eq!(
            point.blobs,
            [
                "bytes_served".to_string(),
                "user-1".to_string(),
                "track-9".to_string()
            ]
        );
        assert_eq!(point.doubles, [100_000.0, 3.0]);
    }

    #[test]
    fn served_bytes_sums_only_matching_bytes_served_events() {
        let events = vec![
            MeterEvent {
                kind: MeterKind::PlayStart,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes: 0,
                seconds: 0,
            },
            MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes: 16_384,
                seconds: 1,
            },
            MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes: 100_000,
                seconds: 3,
            },
            MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "u".to_string(),
                track_id: "other".to_string(),
                bytes: 50_000,
                seconds: 2,
            },
            MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "v".to_string(),
                track_id: "t".to_string(),
                bytes: 60_000,
                seconds: 2,
            },
        ];
        assert_eq!(served_bytes(&events, "u", "t"), 116_384);
        assert_eq!(served_bytes(&events, "u", "other"), 50_000);
        assert_eq!(served_bytes(&events, "v", "t"), 60_000);
        assert_eq!(served_bytes(&events, "nobody", "t"), 0);
    }

    // Deterministic stand-in for a property-test RNG (this crate stays
    // dependency-free): xorshift64* over a fixed seed.
    fn rng_next(state: &mut u64) -> u64 {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[test]
    fn seconds_are_the_ceiling_monotone_and_split_bounded() {
        let mut state = 0x1234_5678_9ABC_DEF1u64;
        for i in 0usize..5_000 {
            let bytes = if i.is_multiple_of(7) {
                // Edge strata near multiples of the rate.
                48_000 * (rng_next(&mut state) % 5) + rng_next(&mut state) % 3
            } else {
                rng_next(&mut state) % 20_000_000
            };
            let seconds = stream_seconds_for_bytes(bytes);
            // Ceiling: the attributed seconds cover the bytes, and one fewer
            // second would not (for any positive byte count).
            assert!(seconds * STREAM_BYTES_PER_SEC >= bytes, "bytes {bytes}");
            if bytes > 0 {
                assert!(
                    seconds
                        .saturating_sub(1)
                        .saturating_mul(STREAM_BYTES_PER_SEC)
                        < bytes,
                    "bytes {bytes}"
                );
            } else {
                assert_eq!(seconds, 0);
            }
        }
        let mut sorted: Vec<u64> = {
            let mut s = 0xABCDu64;
            (0..2_000).map(|_| rng_next(&mut s) % 50_000_000).collect()
        };
        sorted.sort_unstable();
        let mut last = 0u64;
        for bytes in sorted {
            let seconds = stream_seconds_for_bytes(bytes);
            assert!(seconds >= last, "monotone at {bytes}");
            last = seconds;
        }
        // Split bound: serving a..b in two events attributes at most one
        // extra second versus serving a+b at once, never fewer.
        let mut s2 = 0x99u64;
        for _ in 0..2_000 {
            let a = rng_next(&mut s2) % 5_000_000;
            let b = rng_next(&mut s2) % 5_000_000;
            let whole = stream_seconds_for_bytes(a.saturating_add(b));
            let split = stream_seconds_for_bytes(a).saturating_add(stream_seconds_for_bytes(b));
            assert!(split >= whole, "split {a}+{b}");
            assert!(split <= whole.saturating_add(1), "split {a}+{b}");
        }
    }

    #[test]
    fn doubles_stay_exact_for_realistic_plays() {
        // Every byte count a real play can serve is far below 2^53, so the
        // Analytics Engine doubles round-trip exactly.
        let mut state = 42u64;
        for _ in 0..1_000 {
            let bytes = rng_next(&mut state) % 10_737_418_240;
            let event = MeterEvent {
                kind: MeterKind::BytesServed,
                user_id: "u".to_string(),
                track_id: "t".to_string(),
                bytes,
                seconds: stream_seconds_for_bytes(bytes),
            };
            let point = analytics_point(&event);
            assert_eq!(point.doubles[0] as u64, bytes);
            assert_eq!(point.doubles[1] as u64, event.seconds);
        }
    }
}
