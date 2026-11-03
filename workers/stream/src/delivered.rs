//! Delivered-byte intervals: the hour meter's raw input (replan spec
//! §3.1).
//!
//! The public play route ([`crate::play_route`]) counts bytes **actually
//! written to the client** and, per completed range response (including a
//! mid-range abort, with the span truncated to what was written), emits
//! one [`DeliveredInterval`] `(play_id, [start, end))`. Prefetch and the
//! `finalize` cache-fill drain deliver nothing to the client and emit
//! nothing: intervals come only from the response-writer side of a play.
//!
//! Topology: the route reports each interval to the per-user `NntpPool`
//! Durable Object (`POST /delivered`, [`crate::routes::DELIVERED_PATH`]),
//! which is the per-user accounting owner. This module defines the
//! handoff shape only — interval-union merging per play, persistence, and
//! the flush to the api Worker's ledger endpoint land with the
//! enforcement and meter card behind this same envelope, so the
//! Durable Object route here validates and acknowledges (`202`) without
//! storing. Duplicate delivery is safe by design: the meter merges unions
//! per `play_id`, so re-reporting an overlapping span adds zero.

use serde::{Deserialize, Serialize};

use crate::meter::is_valid_key_field;
use crate::probe::{ProbeError, ProbeReply, MAX_USER_ID_LEN};

/// Largest accepted delivered-report envelope. The report is three ids
/// (≤128 + 512 + 512 bytes, JSON-escaping at worst doubles them) and two
/// numbers, so 4 KiB is generous headroom, not a tight fit.
pub const MAX_DELIVERED_REQUEST_BYTES: usize = 4 * 1024;

/// One completed range response's client-written bytes, as the half-open
/// span `[start, end)` of the track file. Emitted exactly once per range
/// response by the play route; `start == end` (nothing written) is never
/// emitted and is refused at the Durable Object boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredInterval {
    pub user_id: String,
    /// From the verified `v2` grant; passes the ledger field rule.
    pub play_id: String,
    pub track_id: String,
    pub start: u64,
    /// Exclusive end: `start + bytes_written`.
    pub end: u64,
}

impl DeliveredInterval {
    /// Bytes in the span (`end - start`).
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Where the play route hands its per-response intervals. Implementations:
/// the Worker runtime forwards to the per-user Durable Object; tests
/// collect into [`VecDeliveredSink`].
pub trait DeliveredSink {
    fn delivered(&mut self, interval: DeliveredInterval);
}

/// In-memory sink for tests and native harnesses.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VecDeliveredSink(pub Vec<DeliveredInterval>);

impl DeliveredSink for VecDeliveredSink {
    fn delivered(&mut self, interval: DeliveredInterval) {
        self.0.push(interval);
    }
}

/// HTTP answer for one delivered report (rendered by the runtime adapter
/// like a probe reply: JSON body, `Retry-After` when carried).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredReply {
    pub status: u16,
    pub body: String,
    pub retry_after_secs: Option<u64>,
}

impl From<Result<(), ProbeError>> for DeliveredReply {
    fn from(result: Result<(), ProbeError>) -> Self {
        match result {
            Ok(()) => DeliveredReply {
                status: 202,
                body: r#"{"accepted":true}"#.to_string(),
                retry_after_secs: None,
            },
            Err(error) => DeliveredReply {
                status: error.status(),
                body: serde_json::json!({ "error": error.code() }).to_string(),
                retry_after_secs: error.retry_after_secs(),
            },
        }
    }
}

/// Parse and validate one delivered-report body for the pool owned by
/// `pool_user` (same owner discipline as the probe/body envelopes: the
/// Durable Object id is the user id, and a report naming anyone else is
/// refused before anything is accounted).
pub fn parse_delivered_report(
    body: &[u8],
    pool_user: &str,
) -> Result<DeliveredInterval, ProbeError> {
    if body.len() > MAX_DELIVERED_REQUEST_BYTES {
        return Err(ProbeError::TooLarge);
    }
    let report: DeliveredInterval = serde_json::from_slice(body)
        .map_err(|_| ProbeError::BadRequest("body must be a delivered report"))?;
    if report.user_id.is_empty() || report.user_id.len() > MAX_USER_ID_LEN {
        return Err(ProbeError::BadRequest("invalid user_id"));
    }
    if report.user_id != pool_user {
        return Err(ProbeError::BadRequest("report is for another user's pool"));
    }
    // The ids must be carryable by the ledger idempotency key the meter
    // sink will derive from them (`play:{user}:{track}:{play}`).
    if !is_valid_key_field(&report.play_id, 512) || !is_valid_key_field(&report.track_id, 512) {
        return Err(ProbeError::BadRequest("invalid play_id or track_id"));
    }
    if report.is_empty() {
        return Err(ProbeError::BadRequest("empty delivered interval"));
    }
    Ok(report)
}

/// Serve one delivered report end to end: validate `body` for
/// `pool_user`'s pool and acknowledge it. Accounting (union per play,
/// persistence, ledger flush) belongs to that card, behind this envelope.
pub fn serve_delivered(pool_user: &str, body: &[u8]) -> DeliveredReply {
    DeliveredReply::from(parse_delivered_report(body, pool_user).map(|_| ()))
}

impl From<DeliveredReply> for ProbeReply {
    /// The runtime renders every Durable Object JSON reply through one
    /// shape; the delivered reply is field-identical by construction.
    fn from(reply: DeliveredReply) -> Self {
        ProbeReply {
            status: reply.status,
            body: reply.body,
            retry_after_secs: reply.retry_after_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_body(user: &str, play: &str, track: &str, start: u64, end: u64) -> Vec<u8> {
        serde_json::json!({
            "user_id": user,
            "play_id": play,
            "track_id": track,
            "start": start,
            "end": end,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn valid_report_for_the_owner_parses() {
        let parsed = parse_delivered_report(
            &report_body("user-1", "play-7", "track-9", 10, 25),
            "user-1",
        )
        .expect("valid report");
        assert_eq!(parsed.len(), 15);
        assert!(!parsed.is_empty());
        assert_eq!(parsed.play_id, "play-7");
    }

    #[test]
    fn report_for_another_users_pool_is_refused() {
        let err =
            parse_delivered_report(&report_body("user-2", "play-7", "track-9", 0, 1), "user-1")
                .expect_err("foreign report must refuse");
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn empty_or_inverted_intervals_are_refused() {
        for (start, end) in [(5, 5), (9, 5), (0, 0)] {
            assert!(
                parse_delivered_report(
                    &report_body("user-1", "play-7", "track-9", start, end),
                    "user-1"
                )
                .is_err(),
                "[{start}, {end})"
            );
        }
    }

    #[test]
    fn unmeterable_ids_are_refused() {
        for (play, track) in [
            ("play:7", "track-9"),
            ("play-7", "track 9"),
            ("", "track-9"),
        ] {
            assert!(
                parse_delivered_report(&report_body("user-1", play, track, 0, 1), "user-1")
                    .is_err(),
                "play={play:?} track={track:?}"
            );
        }
    }

    #[test]
    fn oversized_and_garbage_bodies_are_refused() {
        assert_eq!(
            parse_delivered_report(&vec![b'x'; MAX_DELIVERED_REQUEST_BYTES + 1], "user-1")
                .expect_err("too large")
                .status(),
            413
        );
        assert!(parse_delivered_report(b"not json", "user-1").is_err());
        assert!(parse_delivered_report(b"{}", "user-1").is_err());
    }

    #[test]
    fn serve_delivered_acknowledges_with_202() {
        let reply = serve_delivered("user-1", &report_body("user-1", "play-7", "track-9", 0, 3));
        assert_eq!(reply.status, 202);
        assert_eq!(reply.body, r#"{"accepted":true}"#);
        let refused = serve_delivered("user-1", &report_body("user-2", "play-7", "track-9", 0, 3));
        assert_eq!(refused.status, 400);
        assert_eq!(refused.body, r#"{"error":"bad_request"}"#);
    }
}
