//! Meter sink: per-play delivered-byte unions and the signed flush to
//! the api Worker's ledger endpoint (replan spec §3.1).
//!
//! The per-user `NntpPool` Durable Object receives one
//! [`DeliveredInterval`](crate::delivered::DeliveredInterval) per
//! completed range response and merges them into a union per play
//! ([`IntervalUnion`]): seeking back over audio already delivered in
//! the same play adds nothing, and re-reporting an overlapping span is
//! a no-op, so duplicate delivery meters zero extra seconds by
//! construction. A [`MeterBook`] holds every in-flight play for the
//! pool's user; the runtime persists it in DO storage and, on the DO
//! alarm, flushes the plays whose union grew as cumulative windows to
//! `POST /internal/v1/play-windows` on the api Worker over the `API`
//! service binding. The request is HMAC-signed (`METER_SINK_SECRET`,
//! raw string bytes like the play-grant secret) as
//! `Authorization: MeterSink <unix-secs>.<hex hmac-sha256("<secs>.<body>")>`;
//! the api side verifies the same scheme in `workers/api/src/meter_sink.rs`
//! and writes through its monotonic play-window upsert, so flushing the
//! same cumulative snapshot twice (lost ack, alarm replay) never
//! double-bills. Windows the api names as terminal (accepted, fenced
//! behind account deletion, or rejected) are marked flushed here and
//! never retried; anything else stays pending for the next alarm.
//!
//! Prefetch and the cache-fill drain never reach this module: intervals
//! exist only for bytes actually written to a client
//! ([`crate::delivered`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::delivered::DeliveredInterval;
use crate::grant::hmac_sha256;

/// Internal api endpoint the DO flushes cumulative windows to.
pub const SINK_PATH: &str = "/internal/v1/play-windows";
/// Authorization scheme carrying `<unix-secs>.<hex-signature>`.
pub const SINK_AUTH_SCHEME: &str = "MeterSink";
/// Secret binding/var name shared with the api Worker
/// (`wrangler secret put METER_SINK_SECRET` on both).
pub const METER_SINK_SECRET_VAR: &str = "METER_SINK_SECRET";
/// Minimum accepted secret length, mirroring the api side: a shorter
/// (or absent) secret never signs — windows stay pending instead of
/// flushing under a weak key.
pub const MIN_SECRET_BYTES: usize = 32;
/// Windows per flush request; the api body cap admits a full batch of
/// worst-case windows with headroom.
pub const MAX_BATCH_WINDOWS: usize = 64;
/// In-flight plays retained per user. One user holds one live lease,
/// so real concurrency is a handful of plays; the cap only bounds a
/// report flood. Beyond it, fully-flushed plays are evicted first,
/// then new plays go unrecorded (bounded under-metering, never an
/// unbounded DO value).
pub const MAX_TRACKED_PLAYS: usize = 128;
/// Disjoint spans kept per play union. Seeks merge on overlap and
/// adjacency, so realistic playheads stay far below this; beyond it,
/// further spans are dropped (bounded under-metering) rather than
/// growing storage per range response.
pub const MAX_UNION_SPANS: usize = 4096;
/// A fully-flushed play is forgotten this long after its last
/// delivered report. A late report recreates the entry with a smaller
/// cumulative union, which the api upsert treats as stale — safe by
/// monotonicity.
pub const FLUSHED_RETENTION_SECS: u64 = 3600;
/// DO storage key for the serialized [`MeterBook`].
pub const METER_BOOK_KEY: &str = "meter_book_v1";

/// A set of half-open byte spans kept sorted, disjoint and
/// non-adjacent; the per-play delivered union.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalUnion {
    spans: Vec<(u64, u64)>,
}

impl IntervalUnion {
    /// Merge `[start, end)` into the union. Empty or inverted spans
    /// are ignored (the DO boundary refuses them earlier; this is the
    /// second fence). Spans that would push the union past
    /// [`MAX_UNION_SPANS`] disjoint spans are dropped.
    pub fn add(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        // First span that ends at or after `start` (adjacency merges).
        let lo = self.spans.partition_point(|&(_, e)| e < start);
        // First span that starts after `end` (adjacency merges).
        let hi = self.spans.partition_point(|&(s, _)| s <= end);
        if lo == hi {
            if self.spans.len() >= MAX_UNION_SPANS {
                return;
            }
            self.spans.insert(lo, (start, end));
            return;
        }
        let merged = (start.min(self.spans[lo].0), end.max(self.spans[hi - 1].1));
        self.spans.splice(lo..hi, [merged]);
    }

    /// Total union bytes: the value the api derives seconds from.
    pub fn total_bytes(&self) -> u64 {
        self.spans.iter().map(|(s, e)| e - s).sum()
    }

    pub fn span_count(&self) -> usize {
        self.spans.len()
    }
}

/// One play's accounting: its delivered union so far and how much of
/// it the api has acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayMeter {
    pub track_id: String,
    pub union: IntervalUnion,
    /// Confirmed file size when the serving path knew it (R2 blob
    /// length); rides the flush as the derivation's snapshot input.
    pub file_bytes: Option<u64>,
    /// First delivered report's time: the play bills to this month.
    pub started_at: u64,
    pub updated_at: u64,
    /// Cumulative union bytes the api has terminally answered for.
    pub flushed_bytes: u64,
}

impl PlayMeter {
    fn pending(&self) -> bool {
        self.union.total_bytes() > self.flushed_bytes
    }
}

/// Every in-flight play for one user (one DO instance), keyed by play
/// id. Serialized whole into DO storage after each mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterBook {
    pub user_id: String,
    pub plays: BTreeMap<String, PlayMeter>,
}

impl MeterBook {
    pub fn new(user_id: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            plays: BTreeMap::new(),
        }
    }

    /// Merge one delivered interval. A report naming a different track
    /// for a known play id is refused (`false`): play ids are minted
    /// per (user, track) at grant issuance, so a mismatch is a bug or
    /// a forgery and must not smear bytes across tracks.
    pub fn record(&mut self, interval: &DeliveredInterval, now_unix: u64) -> bool {
        if interval.user_id != self.user_id {
            return false;
        }
        if let Some(play) = self.plays.get_mut(&interval.play_id) {
            if play.track_id != interval.track_id {
                return false;
            }
            play.union.add(interval.start, interval.end);
            if interval.file_bytes.is_some() {
                play.file_bytes = interval.file_bytes;
            }
            play.updated_at = now_unix;
            return true;
        }
        if self.plays.len() >= MAX_TRACKED_PLAYS {
            self.evict_flushed(now_unix);
            if self.plays.len() >= MAX_TRACKED_PLAYS {
                return false;
            }
        }
        let mut union = IntervalUnion::default();
        union.add(interval.start, interval.end);
        self.plays.insert(
            interval.play_id.clone(),
            PlayMeter {
                track_id: interval.track_id.clone(),
                union,
                file_bytes: interval.file_bytes,
                started_at: now_unix,
                updated_at: now_unix,
                flushed_bytes: 0,
            },
        );
        true
    }

    /// Cumulative windows not yet acknowledged, oldest first, capped
    /// at [`MAX_BATCH_WINDOWS`] for one request.
    pub fn pending_windows(&self) -> Vec<WindowReport> {
        let mut pending: Vec<(&String, &PlayMeter)> =
            self.plays.iter().filter(|(_, p)| p.pending()).collect();
        pending.sort_by_key(|(_, p)| p.started_at);
        pending
            .into_iter()
            .take(MAX_BATCH_WINDOWS)
            .map(|(play_id, play)| WindowReport {
                user_id: self.user_id.clone(),
                track_id: play.track_id.clone(),
                play_id: play_id.clone(),
                union_bytes: play.union.total_bytes(),
                file_bytes: play.file_bytes,
                started_at: play.started_at,
            })
            .collect()
    }

    pub fn has_pending(&self) -> bool {
        self.plays.values().any(PlayMeter::pending)
    }

    /// Mark the windows the api answered terminally: each play's
    /// flushed total rises to the union that was sent for it (never
    /// above its current union — reports that landed mid-flight stay
    /// pending).
    pub fn mark_flushed(&mut self, sent: &[WindowReport], terminal_play_ids: &[String]) {
        for window in sent {
            if !terminal_play_ids.contains(&window.play_id) {
                continue;
            }
            if let Some(play) = self.plays.get_mut(&window.play_id) {
                play.flushed_bytes = play
                    .flushed_bytes
                    .max(window.union_bytes.min(play.union.total_bytes()));
            }
        }
    }

    /// Forget fully-flushed plays past the retention window.
    pub fn evict_flushed(&mut self, now_unix: u64) {
        self.plays.retain(|_, play| {
            play.pending() || now_unix.saturating_sub(play.updated_at) <= FLUSHED_RETENTION_SECS
        });
    }
}

/// One play's cumulative window on the wire. Field-for-field the api
/// side's `WindowReport` (`workers/api/src/meter_sink.rs`); the two
/// crates share no code, so the shape is pinned by tests on both ends
/// and by the signing vector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowReport {
    pub user_id: String,
    pub track_id: String,
    pub play_id: String,
    pub union_bytes: u64,
    pub file_bytes: Option<u64>,
    pub started_at: u64,
}

/// The flush request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushBatch {
    pub windows: Vec<WindowReport>,
}

/// The api ack: play ids answered terminally, split by disposition
/// for observability. Any id present in any list is done; absent ids
/// retry on the next alarm.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FlushAck {
    #[serde(default)]
    pub accepted: Vec<String>,
    #[serde(default)]
    pub fenced: Vec<String>,
    #[serde(default)]
    pub rejected: Vec<String>,
}

impl FlushAck {
    pub fn terminal_play_ids(&self) -> Vec<String> {
        let mut ids = self.accepted.clone();
        ids.extend(self.fenced.iter().cloned());
        ids.extend(self.rejected.iter().cloned());
        ids
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("hex digit"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("hex digit"));
    }
    out
}

/// The `Authorization` header value for one flush body:
/// `MeterSink <unix-secs>.<hex hmac-sha256(secret, "<unix-secs>.<body>")>`.
/// Returns `None` when the secret is unusable (absent or under
/// [`MIN_SECRET_BYTES`]) — the caller keeps the windows pending.
pub fn authorization_header(secret: Option<&[u8]>, now_unix: u64, body: &[u8]) -> Option<String> {
    let secret = secret.filter(|s| s.len() >= MIN_SECRET_BYTES)?;
    let mut signed = Vec::with_capacity(body.len() + 24);
    signed.extend_from_slice(now_unix.to_string().as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);
    let mac = hmac_sha256(secret, &signed);
    Some(format!("{SINK_AUTH_SCHEME} {now_unix}.{}", hex_lower(&mac)))
}

/// Serialize one flush batch for signing and sending.
pub fn batch_body(windows: &[WindowReport]) -> Option<Vec<u8>> {
    serde_json::to_vec(&FlushBatch {
        windows: windows.to_vec(),
    })
    .ok()
}

/// Parse the api ack body. Anything unreadable is a failed flush:
/// the windows stay pending and the alarm retries.
pub fn parse_ack(body: &[u8]) -> Option<FlushAck> {
    serde_json::from_slice(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"meter-sink-test-secret-32-bytes!!";

    fn interval(play: &str, track: &str, start: u64, end: u64) -> DeliveredInterval {
        DeliveredInterval {
            user_id: "user-1".to_string(),
            play_id: play.to_string(),
            track_id: track.to_string(),
            start,
            end,
            file_bytes: Some(1_000),
        }
    }

    #[test]
    fn union_merges_overlap_adjacency_and_duplicates() {
        let mut union = IntervalUnion::default();
        union.add(0, 100);
        union.add(50, 150);
        union.add(150, 200);
        assert_eq!(union.total_bytes(), 200);
        assert_eq!(union.span_count(), 1);
        // Exact duplicate and contained spans add zero.
        union.add(0, 200);
        union.add(40, 60);
        assert_eq!(union.total_bytes(), 200);
        // Seeking to a fresh span adds only the new audio.
        union.add(500, 650);
        assert_eq!(union.total_bytes(), 350);
        assert_eq!(union.span_count(), 2);
        // A span bridging the gap merges everything.
        union.add(190, 510);
        assert_eq!(union.total_bytes(), 650);
        assert_eq!(union.span_count(), 1);
        // Empty/inverted spans are ignored.
        union.add(10, 10);
        union.add(10, 5);
        assert_eq!(union.total_bytes(), 650);
    }

    #[test]
    fn seek_heavy_play_unions_below_the_file_size() {
        // Re-delivering the first minute ten times meters one minute.
        let mut book = MeterBook::new("user-1");
        for _ in 0..10 {
            assert!(book.record(&interval("play-1", "track-1", 0, 60_000), 100));
        }
        let windows = book.pending_windows();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].union_bytes, 60_000);
    }

    #[test]
    fn record_refuses_foreign_users_and_track_mismatch() {
        let mut book = MeterBook::new("user-1");
        let mut foreign = interval("play-1", "track-1", 0, 10);
        foreign.user_id = "user-2".to_string();
        assert!(!book.record(&foreign, 100));
        assert!(book.record(&interval("play-1", "track-1", 0, 10), 100));
        assert!(!book.record(&interval("play-1", "track-2", 10, 20), 101));
        assert_eq!(book.pending_windows()[0].union_bytes, 10);
    }

    #[test]
    fn flush_marks_terminal_and_keeps_the_rest_pending() {
        let mut book = MeterBook::new("user-1");
        assert!(book.record(&interval("play-1", "track-1", 0, 100), 100));
        assert!(book.record(&interval("play-2", "track-1", 0, 50), 101));
        let sent = book.pending_windows();
        assert_eq!(sent.len(), 2);
        book.mark_flushed(&sent, &["play-1".to_string()]);
        let pending = book.pending_windows();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].play_id, "play-2");
        // Growth after the ack re-pends with the new cumulative total.
        assert!(book.record(&interval("play-1", "track-1", 100, 140), 200));
        let pending = book.pending_windows();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .find(|w| w.play_id == "play-1")
                .expect("play-1 pending")
                .union_bytes,
            140
        );
    }

    #[test]
    fn flushed_plays_evict_after_retention() {
        let mut book = MeterBook::new("user-1");
        assert!(book.record(&interval("play-1", "track-1", 0, 10), 100));
        let sent = book.pending_windows();
        book.mark_flushed(&sent, &["play-1".to_string()]);
        book.evict_flushed(100 + FLUSHED_RETENTION_SECS);
        assert_eq!(book.plays.len(), 1, "retained inside the window");
        book.evict_flushed(101 + FLUSHED_RETENTION_SECS);
        assert!(book.plays.is_empty(), "forgotten past retention");
    }

    #[test]
    fn signing_matches_the_shared_vector() {
        // Python hmac oracle (independent of both crates), mirrored in
        // the api crate's meter_sink tests:
        // hmac-sha256(SECRET, "1893456000." + '{"windows":[]}').
        let header =
            authorization_header(Some(SECRET), 1_893_456_000, br#"{"windows":[]}"#).expect("signs");
        assert_eq!(
            header,
            "MeterSink 1893456000.\
             c4fd315da275680a274b8d3001f8df5e1633c11864cf0daf6dc65ecfab0ea3bb"
        );
        assert_eq!(
            batch_body(&[]).expect("serializes"),
            br#"{"windows":[]}"#.to_vec()
        );
    }

    #[test]
    fn signing_refuses_short_or_absent_secrets() {
        assert!(authorization_header(None, 1, b"{}").is_none());
        assert!(authorization_header(Some(b"short"), 1, b"{}").is_none());
        assert!(authorization_header(Some(SECRET), 1, b"{}").is_some());
    }

    #[test]
    fn ack_terminal_ids_span_all_dispositions() {
        let ack =
            parse_ack(br#"{"accepted":["p1"],"fenced":["p2"],"rejected":["p3"]}"#).expect("parses");
        assert_eq!(ack.terminal_play_ids(), ["p1", "p2", "p3"]);
        assert!(parse_ack(b"not json").is_none());
        assert_eq!(parse_ack(b"{}"), Some(FlushAck::default()));
    }

    #[test]
    fn constants_are_pinned() {
        assert_eq!(SINK_PATH, "/internal/v1/play-windows");
        assert_eq!(SINK_AUTH_SCHEME, "MeterSink");
        assert_eq!(METER_SINK_SECRET_VAR, "METER_SINK_SECRET");
        assert_eq!(MIN_SECRET_BYTES, 32);
        assert_eq!(MAX_BATCH_WINDOWS, 64);
    }
}
