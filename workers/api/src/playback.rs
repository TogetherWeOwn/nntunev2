//! Playback enforcement core: the pure decisions behind grant issuance
//! (subscription gate, hour cap, one-concurrent-stream lease) plus the
//! `playback_leases` statements (migration 0031).
//!
//! Pure Rust, native + wasm32, NO Worker runtime imports — same
//! constraint as `billing` / `plan_limits` / `ledger`. Time arrives only
//! as `now` Unix seconds; the handler composes these decisions with
//! `billing::resolve_effective_plan` and the `plans`-row caps, and the
//! Worker binds the statements below. The stream Worker has no DB
//! binding, so this one handler is the single enforcement point: it
//! mints the short-lived grant ([`crate::play_grant`]) only after every
//! gate below allows.
//!
//! Gate order at a NEW play start (queue advance, repeat-one, fresh
//! press): subscription first (`402 subscription_required`), then the
//! monthly hour cap (`402 plan_limit_exceeded`), then the lease (`429`
//! while another client session holds it). A CONTINUATION — same client
//! session, same play, same track as the live lease row — skips the
//! subscription and cap gates so the current track can finish. The
//! per-play deadline is fixed at acquisition and caps both lease and
//! grant expiry. After expiry, issuance goes through the new-play gates.
//!
//! Lease rules (one row per user, holder = client session id):
//! - TTL [`LEASE_TTL_SECS`], client heartbeat every
//!   [`HEARTBEAT_INTERVAL_SECS`]; a closed tab frees the lease within
//!   one TTL for other sessions.
//! - Same-session re-acquire replaces the track (never self-refused).
//! - Another session acquires only after expiry, or immediately with
//!   an explicit takeover; either way `epoch` increments and rides in
//!   the grant, so the displaced player's next heartbeat reports
//!   `superseded` and it pauses itself. The hard guarantee is its next
//!   grant request, which the lease gate refuses.
//! - Idle expiry and the per-play deadline are strict: at either
//!   boundary the lease is no longer eligible for renewal.

use crate::billing::{EffectivePlan, SubscriptionView};

/// Lease lifetime without a heartbeat, in seconds.
pub const LEASE_TTL_SECS: u64 = 45;
/// Client heartbeat cadence the issuance response advertises, in
/// seconds: a superseded player learns within one interval.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

const CONTINUATION_SLACK_SECS: u64 = 120;
const UNKNOWN_DURATION_WINDOW_SECS: u64 = 900;

/// Use the identity-safe app/mirror durations, never bitrate estimates.
/// Round up to a whole second and allow time for startup and rebuffering.
pub(crate) fn continuation_deadline(now: u64, meta: crate::meter_sink::TrackMeterMeta) -> u64 {
    let window = meta
        .track_duration_ms
        .filter(|&ms| ms > 0)
        .or_else(|| meta.mb_duration_ms.filter(|&ms| ms > 0))
        .map_or(UNKNOWN_DURATION_WINDOW_SECS, |ms| {
            ms.div_ceil(1000) + CONTINUATION_SLACK_SECS
        });
    now.saturating_add(window)
}

pub(crate) fn lease_expiry(now: u64, continuation_deadline: u64) -> u64 {
    now.saturating_add(LEASE_TTL_SECS)
        .min(continuation_deadline)
}

/// Why an unentitled account was refused, carried as the `reason` field
/// of the `402 subscription_required` body so the client can word the
/// paywall ("subscribe" vs "resubscribe") without another round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRefusal {
    /// No subscription ever reached paid coverage.
    NeverSubscribed,
    /// Paid coverage existed once but does not cover now.
    Lapsed,
}

impl SubscriptionRefusal {
    /// Stable wire value for the body's `reason` field.
    pub fn reason(self) -> &'static str {
        match self {
            SubscriptionRefusal::NeverSubscribed => "never_subscribed",
            SubscriptionRefusal::Lapsed => "lapsed",
        }
    }
}

/// The subscription gate: `None` when entitled, else the refusal reason.
/// `ever_paid` is the [`crate::billing::subscription_ever_paid_statement`]
/// signal (any row, live or terminal, with a non-NULL paid end): an
/// abandoned checkout (`incomplete`, never paid) reads
/// `never_subscribed`, a once-paying account reads `lapsed` whatever
/// its current status.
pub fn subscription_refusal(
    effective: &EffectivePlan,
    ever_paid: bool,
) -> Option<SubscriptionRefusal> {
    if effective.entitled {
        return None;
    }
    Some(if ever_paid {
        SubscriptionRefusal::Lapsed
    } else {
        SubscriptionRefusal::NeverSubscribed
    })
}

/// The live-slot subscription row as owned data (the `Store` returns
/// this; [`SubscriptionRow::as_view`] borrows it for
/// `billing::resolve_effective_plan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionRow {
    pub plan_id: String,
    pub status: String,
    pub cancel_at_period_end: bool,
    pub paid_through_at_unix: Option<i64>,
    pub grace_until_unix: Option<i64>,
    /// Carried for `SubscriptionView` parity (display-only there);
    /// never an entitlement input at the gate.
    pub current_period_end_unix: Option<i64>,
}

impl SubscriptionRow {
    pub fn as_view(&self) -> SubscriptionView<'_> {
        SubscriptionView {
            plan_id: &self.plan_id,
            status: &self.status,
            cancel_at_period_end: self.cancel_at_period_end,
            paid_through_at_unix: self.paid_through_at_unix,
            grace_until_unix: self.grace_until_unix,
            current_period_end_unix: self.current_period_end_unix,
        }
    }
}

/// One `playback_leases` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRow {
    pub session_id: String,
    pub track_id: String,
    pub play_id: String,
    pub epoch: u64,
    pub expires_at: u64,
    /// Fixed for the play; legacy rows use their existing idle expiry.
    pub continuation_deadline: u64,
}

impl LeaseRow {
    /// True while the lease has not expired at `now` (strict: at the
    /// expiry instant it is already free for other sessions, matching
    /// the acquire statement's `expires_at <= now`).
    pub fn is_live(&self, now: u64) -> bool {
        self.expires_at > now && self.continuation_deadline > now
    }

    /// True while the same client session holds a live lease for this
    /// play and track. Retain expired rows for the next acquire's epoch.
    pub fn is_continuation(
        &self,
        session_id: &str,
        play_id: &str,
        track_id: &str,
        now: u64,
    ) -> bool {
        self.is_live(now)
            && self.session_id == session_id
            && self.play_id == play_id
            && self.track_id == track_id
    }

    /// Seconds until another session may acquire without a takeover
    /// (`0` once expired): the `429`'s `Retry-After`.
    pub fn retry_after_secs(&self, now: u64) -> u64 {
        self.expires_at
            .min(self.continuation_deadline)
            .saturating_sub(now)
    }
}

/// Outcome of a lease acquire attempt (the conditional upsert).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAcquire {
    /// The caller holds the lease at this epoch (fresh row, same-session
    /// replace, post-expiry acquire, or takeover — every successful
    /// new-play acquire bumps the epoch).
    Acquired { epoch: u64 },
    /// Another session holds a live lease; retry after this many
    /// seconds (its remaining TTL) or take over explicitly.
    Held { retry_after_secs: u64 },
}

/// What a heartbeat reports for (`session_id`, `play_id`, `epoch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseHeartbeat {
    /// The caller still holds the lease; the touch extended it.
    Active,
    /// The row names another session/play/epoch: the caller was
    /// displaced (takeover or its own newer play elsewhere) and must
    /// pause.
    Superseded,
    /// No lease row exists for the user, or the matching lease expired.
    Expired,
}

impl LeaseHeartbeat {
    /// Stable wire value for the heartbeat body's `lease` field.
    pub fn as_str(self) -> &'static str {
        match self {
            LeaseHeartbeat::Active => "active",
            LeaseHeartbeat::Superseded => "superseded",
            LeaseHeartbeat::Expired => "expired",
        }
    }

    /// `Active` requires a live lease for the exact (session, play, epoch).
    /// An identity mismatch still reports displacement. The handler must
    /// also win the conditional touch before reporting `Active`.
    pub fn classify(
        row: Option<&LeaseRow>,
        session_id: &str,
        play_id: &str,
        epoch: u64,
        now: u64,
    ) -> Self {
        match row {
            None => LeaseHeartbeat::Expired,
            Some(row)
                if row.session_id == session_id && row.play_id == play_id && row.epoch == epoch =>
            {
                if row.is_live(now) {
                    LeaseHeartbeat::Active
                } else {
                    LeaseHeartbeat::Expired
                }
            }
            Some(_) => LeaseHeartbeat::Superseded,
        }
    }
}

// --- Statements: parameterized lease access ---------------------------------
//
// The Worker binds fields positionally and never interpolates. Epoch and
// timestamps are BIGINT Unix seconds (migration 0031 domain). Both
// writers carry the account-deletion fence in its locking form (ADR
// 0002 rule 3): the eligible `users` row is read `FOR SHARE` inside the
// write (`writer_fence`, the same shape the ledger uses), so the
// deletion scheduler's row lock cannot slip `deleting_at` past an
// in-flight lease write — a user with `deleting_at` set, or no user row
// at all, writes nothing, and a write that passes the fence commits
// before any later deletion marker can.

/// Parameters: user_id. The user's lease row, if any.
pub fn playback_lease_select_statement() -> &'static str {
    "SELECT session_id, track_id, play_id, epoch, expires_at, COALESCE(continuation_deadline, expires_at) AS continuation_deadline FROM playback_leases WHERE user_id = $1"
}

/// Parameters: user_id, session_id, track_id, play_id, expires_at, now,
/// takeover, continuation_deadline. Atomic acquire: inserts at epoch 1,
/// or replaces the row at epoch+1 when either expiry boundary is reached, the
/// same session re-acquires, or `takeover` is set. Zero rows back means
/// another session holds a live lease — the caller reads the row for
/// the `Retry-After` — or the deletion fence refused the write (the
/// handler's `account_deleting` pre-check normally answers first; the
/// fence closes the race). The epoch increments in SQL from the stored
/// row, never from a client-supplied value, so concurrent acquires
/// cannot mint colliding epochs.
pub fn playback_lease_acquire_statement() -> &'static str {
    "INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at, continuation_deadline) SELECT $1, $2, $3, $4, 1, LEAST($5, $8), $6, $8 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at, continuation_deadline = EXCLUDED.continuation_deadline WHERE playback_leases.expires_at <= $6 OR playback_leases.continuation_deadline <= $6 OR playback_leases.session_id = EXCLUDED.session_id OR $7 RETURNING epoch"
}

/// Parameters: user_id, session_id, play_id, epoch, expires_at, now.
/// Heartbeat/continuation extension: extends only the exact holder's
/// live row; zero rows means expiry, displacement, or a deletion fence
/// refusal. Overlapping touches cannot shorten the idle expiry or
/// move the per-play deadline. Legacy rows cannot extend their expiry.
pub fn playback_lease_touch_statement() -> &'static str {
    "UPDATE playback_leases SET expires_at = LEAST(GREATEST(playback_leases.expires_at, $5), COALESCE(playback_leases.continuation_deadline, playback_leases.expires_at)), updated_at = GREATEST(playback_leases.updated_at, $6) FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND session_id = $2 AND play_id = $3 AND epoch = $4 AND playback_leases.expires_at > $6 AND COALESCE(playback_leases.continuation_deadline, playback_leases.expires_at) > $6 RETURNING expires_at"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_limits::PlanId;

    const NOW: u64 = 1_800_000_000;

    fn lease() -> LeaseRow {
        LeaseRow {
            session_id: "sess-1".to_string(),
            track_id: "track-9".to_string(),
            play_id: "play-7".to_string(),
            epoch: 3,
            expires_at: NOW + 30,
            continuation_deadline: NOW + 900,
        }
    }

    #[test]
    fn deadline_prefers_positive_app_then_mirror_duration() {
        let mut meta = crate::meter_sink::TrackMeterMeta {
            track_duration_ms: Some(180_001),
            mb_duration_ms: Some(240_000),
            ..Default::default()
        };
        assert_eq!(continuation_deadline(NOW, meta), NOW + 181 + 120);
        meta.track_duration_ms = Some(0);
        assert_eq!(continuation_deadline(NOW, meta), NOW + 240 + 120);
    }

    #[test]
    fn deadline_uses_finite_fallback_without_authoritative_duration() {
        let meta = crate::meter_sink::TrackMeterMeta {
            mb_duration_ms: Some(0),
            bitrate_kbps: Some(1),
            file_bytes: Some(50_000_000),
            ..Default::default()
        };
        assert_eq!(continuation_deadline(NOW, meta), NOW + 900);
        assert_eq!(continuation_deadline(u64::MAX - 1, meta), u64::MAX);
    }

    #[test]
    fn lease_timing_constants_are_pinned() {
        // The 45/15 ratio is the point: at least two missed heartbeats fit
        // inside one TTL, so a single dropped beat never frees a live
        // player's lease. Pinning both values pins the ratio.
        assert_eq!(LEASE_TTL_SECS, 45);
        assert_eq!(HEARTBEAT_INTERVAL_SECS, 15);
    }

    #[test]
    fn subscription_gate_reasons() {
        let entitled = EffectivePlan {
            plan: PlanId::Plus,
            entitled: true,
        };
        assert_eq!(subscription_refusal(&entitled, false), None);
        assert_eq!(subscription_refusal(&entitled, true), None);
        let closed = EffectivePlan {
            plan: PlanId::Free,
            entitled: false,
        };
        assert_eq!(
            subscription_refusal(&closed, false),
            Some(SubscriptionRefusal::NeverSubscribed)
        );
        assert_eq!(
            subscription_refusal(&closed, true),
            Some(SubscriptionRefusal::Lapsed)
        );
        assert_eq!(
            SubscriptionRefusal::NeverSubscribed.reason(),
            "never_subscribed"
        );
        assert_eq!(SubscriptionRefusal::Lapsed.reason(), "lapsed");
    }

    #[test]
    fn lease_liveness_and_retry_after() {
        let row = lease();
        assert!(row.is_live(NOW));
        assert!(row.is_live(NOW + 29));
        assert!(!row.is_live(NOW + 30), "at expiry the lease is free");
        assert_eq!(row.retry_after_secs(NOW), 30);
        assert_eq!(row.retry_after_secs(NOW + 45), 0);
    }

    #[test]
    fn continuation_needs_the_exact_session_play_track_and_live_lease() {
        let row = lease();
        assert!(row.is_continuation("sess-1", "play-7", "track-9", row.expires_at - 1));
        assert!(!row.is_continuation("sess-2", "play-7", "track-9", NOW));
        assert!(!row.is_continuation("sess-1", "play-8", "track-9", NOW));
        assert!(!row.is_continuation("sess-1", "play-7", "track-8", NOW));
        assert!(!row.is_continuation("sess-1", "play-7", "track-9", row.expires_at));
        assert!(!row.is_continuation("sess-1", "play-7", "track-9", row.expires_at + 1));
    }

    #[test]
    fn heartbeat_classification() {
        let row = lease();
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-7", 3, row.expires_at - 1),
            LeaseHeartbeat::Active
        );
        for now in [row.expires_at, row.expires_at + 1] {
            assert_eq!(
                LeaseHeartbeat::classify(Some(&row), "sess-1", "play-7", 3, now),
                LeaseHeartbeat::Expired
            );
        }
        // A replaced epoch stays superseded even after the new row expires.
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-7", 2, row.expires_at),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-2", "play-7", 3, NOW),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-9", 3, NOW),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(None, "sess-1", "play-7", 3, NOW),
            LeaseHeartbeat::Expired
        );
        assert_eq!(LeaseHeartbeat::Active.as_str(), "active");
        assert_eq!(LeaseHeartbeat::Superseded.as_str(), "superseded");
        assert_eq!(LeaseHeartbeat::Expired.as_str(), "expired");
    }

    #[test]
    fn statements_match_exact_text() {
        assert_eq!(
            playback_lease_select_statement(),
            "SELECT session_id, track_id, play_id, epoch, expires_at, COALESCE(continuation_deadline, expires_at) AS continuation_deadline FROM playback_leases WHERE user_id = $1"
        );
        assert_eq!(
            playback_lease_acquire_statement(),
            "INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at, continuation_deadline) SELECT $1, $2, $3, $4, 1, LEAST($5, $8), $6, $8 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at, continuation_deadline = EXCLUDED.continuation_deadline WHERE playback_leases.expires_at <= $6 OR playback_leases.continuation_deadline <= $6 OR playback_leases.session_id = EXCLUDED.session_id OR $7 RETURNING epoch"
        );
    }

    #[test]
    fn touch_statement_requires_a_live_lease() {
        assert_eq!(
            playback_lease_touch_statement(),
            "UPDATE playback_leases SET expires_at = LEAST(GREATEST(playback_leases.expires_at, $5), COALESCE(playback_leases.continuation_deadline, playback_leases.expires_at)), updated_at = GREATEST(playback_leases.updated_at, $6) FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND session_id = $2 AND play_id = $3 AND epoch = $4 AND playback_leases.expires_at > $6 AND COALESCE(playback_leases.continuation_deadline, playback_leases.expires_at) > $6 RETURNING expires_at"
        );
    }

    #[test]
    fn subscription_row_borrows_as_view() {
        let row = SubscriptionRow {
            plan_id: "pro".to_string(),
            status: "active".to_string(),
            cancel_at_period_end: false,
            paid_through_at_unix: Some(42),
            grace_until_unix: None,
            current_period_end_unix: Some(84),
        };
        let view = row.as_view();
        assert_eq!(view.plan_id, "pro");
        assert_eq!(view.paid_through_at_unix, Some(42));
        assert_eq!(view.current_period_end_unix, Some(84));
    }
}
