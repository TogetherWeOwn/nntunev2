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
//! session, same play, same track as the stored lease row — skips the
//! subscription and cap gates: the track that crosses the cap (or whose
//! subscription lapses mid-play) finishes, and seeks/refreshes within
//! the current play stay allowed. The next new start is refused.
//! Abuse of a continuation is bounded by the meter: one play id bills
//! at most its track's duration + 2 s, however often it is refreshed.
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
//! - Expiry only frees the lease for OTHER sessions: the holding
//!   session's row still matches, so its continuation/heartbeat after
//!   a stall re-extends the same epoch instead of minting a new play.

use crate::billing::{EffectivePlan, SubscriptionView};

/// Lease lifetime without a heartbeat, in seconds.
pub const LEASE_TTL_SECS: u64 = 45;
/// Client heartbeat cadence the issuance response advertises, in
/// seconds: a superseded player learns within one interval.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 15;

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
}

/// Continuation deadline for a play acquired at `now`: a known positive
/// duration rounds up to whole seconds plus [`LEASE_TTL_SECS`] of slack;
/// an unknown or zero duration falls back to the grant TTL cap.
/// Saturating, so it never wraps.
pub fn continuation_until(now: u64, duration_ms: Option<u64>) -> u64 {
    let window = match duration_ms {
        Some(ms) if ms > 0 => ms.div_ceil(1000).saturating_add(LEASE_TTL_SECS),
        _ => crate::play_grant::MAX_GRANT_TTL,
    };
    now.saturating_add(window)
}

impl LeaseRow {
    /// True while the lease has not expired at `now` (strict: at the
    /// expiry instant it is already free for other sessions, matching
    /// the acquire statement's `expires_at <= now`).
    pub fn is_live(&self, now: u64) -> bool {
        self.expires_at > now
    }

    /// True when this row is the same client session continuing the
    /// same play of the same track — the continuation case that skips
    /// the subscription/cap gates. Expiry is irrelevant here: it frees
    /// the lease for others, it does not revoke the holder's own play
    /// (a stall then resumes the same epoch, metered under the same
    /// play id).
    pub fn is_continuation(&self, session_id: &str, play_id: &str, track_id: &str) -> bool {
        self.session_id == session_id && self.play_id == play_id && self.track_id == track_id
    }

    /// Seconds until another session may acquire without a takeover
    /// (`0` once expired): the `429`'s `Retry-After`.
    pub fn retry_after_secs(&self, now: u64) -> u64 {
        self.expires_at.saturating_sub(now)
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
    /// No lease row exists for the user (never acquired, or the account
    /// row is gone): nothing to extend.
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

    /// Classify a heartbeat against the stored row. `Active` requires
    /// the exact (session, play, epoch) triple — an epoch mismatch is
    /// a takeover even from the same session (its newer play replaced
    /// this one). The handler still performs the conditional touch;
    /// this is the read-side classification it reports.
    pub fn classify(row: Option<&LeaseRow>, session_id: &str, play_id: &str, epoch: u64) -> Self {
        match row {
            None => LeaseHeartbeat::Expired,
            Some(row)
                if row.session_id == session_id && row.play_id == play_id && row.epoch == epoch =>
            {
                LeaseHeartbeat::Active
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
    "SELECT session_id, track_id, play_id, epoch, expires_at FROM playback_leases WHERE user_id = $1"
}

/// Parameters: user_id, session_id, track_id, play_id, expires_at, now,
/// takeover. Atomic acquire: inserts at epoch 1, or replaces the row at
/// epoch+1 when the stored lease has expired (`expires_at <= now`), the
/// same session re-acquires, or `takeover` is set. Zero rows back means
/// another session holds a live lease — the caller reads the row for
/// the `Retry-After` — or the deletion fence refused the write (the
/// handler's `account_deleting` pre-check normally answers first; the
/// fence closes the race). The epoch increments in SQL from the stored
/// row, never from a client-supplied value, so concurrent acquires
/// cannot mint colliding epochs.
pub fn playback_lease_acquire_statement() -> &'static str {
    "INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at) SELECT $1, $2, $3, $4, 1, $5, $6 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at WHERE playback_leases.expires_at <= $6 OR playback_leases.session_id = EXCLUDED.session_id OR $7 RETURNING epoch"
}

/// Parameters: user_id, session_id, play_id, epoch, expires_at, now.
/// Heartbeat/continuation extension: extends only the exact holder's
/// row; zero rows back means the caller no longer holds the lease
/// (superseded between its read and this touch) or the deletion fence
/// refused. The write is monotonic (`GREATEST`): overlapping heartbeats
/// from the same holder can arrive out of order, and an older request
/// must never shorten the deadline a newer one already stored.
pub fn playback_lease_touch_statement() -> &'static str {
    "UPDATE playback_leases SET expires_at = GREATEST(playback_leases.expires_at, $5), updated_at = GREATEST(playback_leases.updated_at, $6) FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND session_id = $2 AND play_id = $3 AND epoch = $4 RETURNING expires_at"
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
        }
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
    fn continuation_deadline_computation() {
        assert_eq!(continuation_until(NOW, Some(180_000)), NOW + 180 + LEASE_TTL_SECS);
        assert_eq!(continuation_until(NOW, Some(180_001)), NOW + 181 + LEASE_TTL_SECS);
        assert_eq!(continuation_until(NOW, Some(1)), NOW + 1 + LEASE_TTL_SECS);
        assert_eq!(continuation_until(NOW, None), NOW + 900);
        assert_eq!(continuation_until(NOW, Some(0)), NOW + 900);
        assert_eq!(continuation_until(u64::MAX - 1, Some(180_000)), u64::MAX);
        assert_eq!(continuation_until(u64::MAX, None), u64::MAX);
        assert_eq!(continuation_until(NOW, Some(u64::MAX)), NOW + u64::MAX.div_ceil(1000) + LEASE_TTL_SECS);
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
    fn continuation_needs_the_exact_session_play_track() {
        let row = lease();
        assert!(row.is_continuation("sess-1", "play-7", "track-9"));
        assert!(!row.is_continuation("sess-2", "play-7", "track-9"));
        assert!(!row.is_continuation("sess-1", "play-8", "track-9"));
        assert!(!row.is_continuation("sess-1", "play-7", "track-8"));
        // Expiry does not revoke the holder's own continuation.
        let mut stale = lease();
        stale.expires_at = NOW - 3600;
        assert!(stale.is_continuation("sess-1", "play-7", "track-9"));
        assert!(!stale.is_live(NOW));
    }

    #[test]
    fn heartbeat_classification() {
        let row = lease();
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-7", 3),
            LeaseHeartbeat::Active
        );
        // Epoch bump (takeover, or a newer play from the same session).
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-7", 2),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-2", "play-7", 3),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(Some(&row), "sess-1", "play-9", 3),
            LeaseHeartbeat::Superseded
        );
        assert_eq!(
            LeaseHeartbeat::classify(None, "sess-1", "play-7", 3),
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
            "SELECT session_id, track_id, play_id, epoch, expires_at FROM playback_leases WHERE user_id = $1"
        );
        assert_eq!(
            playback_lease_acquire_statement(),
            "INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, updated_at) SELECT $1, $2, $3, $4, 1, $5, $6 FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at WHERE playback_leases.expires_at <= $6 OR playback_leases.session_id = EXCLUDED.session_id OR $7 RETURNING epoch"
        );
        assert_eq!(
            playback_lease_touch_statement(),
            "UPDATE playback_leases SET expires_at = GREATEST(playback_leases.expires_at, $5), updated_at = GREATEST(playback_leases.updated_at, $6) FROM (SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE) AS writer_fence WHERE user_id = $1 AND session_id = $2 AND play_id = $3 AND epoch = $4 RETURNING expires_at"
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
