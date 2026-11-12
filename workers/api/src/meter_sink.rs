//! Meter sink: the api side of the stream→api play-window flush
//! (replan spec §3.1).
//!
//! The stream Worker's per-user Durable Object merges delivered-byte
//! intervals into a cumulative union per play and flushes windows to
//! `POST /internal/v1/play-windows` over its `API` service binding.
//! The request is authenticated by a dedicated HMAC
//! (`METER_SINK_SECRET`, raw string bytes on both Workers):
//! `Authorization: MeterSink <unix-secs>.<hex hmac-sha256("<secs>.<body>")>`,
//! verified here with a ±300 s freshness window. This endpoint is
//! internal-only: no session or bearer authenticates it, and it is
//! deliberately absent from the browser-visible route set
//! (no preflight, no known-path entry).
//!
//! Each window's seconds are derived server-side from its delivered
//! union ([`crate::ledger::prepare_play_window`]) — client-reported
//! play time is never an input — and written through the monotonic
//! play-window upsert, so duplicate or stale flushes add zero.
//! Track metadata for the derivation resolves via
//! [`track_meter_meta_app_statement`] and [`track_meter_meta_mirror_statement`]:
//! the confirming file size prefers
//! the stream-reported blob length, then the cached object's
//! `byte_size`; duration walks track → MusicBrainz recording →
//! bitrate ([`crate::ledger::resolve_duration_ms`]).
//!
//! The ack names each window's play id under its disposition:
//! `accepted` (written or already covered), `fenced` (the writer
//! fence refused it — the account is deleting or gone; terminal, the
//! DO must never retry behind a deletion), or `rejected` (the window
//! can never prepare: bad identity, empty or over-cap union).
//! Anything else — an unreadable batch, a store failure — fails the
//! whole request so the DO keeps the windows pending and retries on
//! its alarm.

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Env var/secret name holding the shared sink HMAC key, raw string
/// bytes (never base64-decoded), provisioned on both Workers.
pub const METER_SINK_SECRET_VAR: &str = "METER_SINK_SECRET";
/// Minimum accepted secret length; unprovisioned or shorter fails
/// the sink closed without verifying anything.
pub const MIN_SECRET_BYTES: usize = 32;
/// The internal route's path (also the stream side's `SINK_PATH`).
pub const SINK_PATH: &str = "/internal/v1/play-windows";
/// Authorization scheme carrying `<unix-secs>.<hex-signature>`.
pub const SINK_AUTH_SCHEME: &str = "MeterSink";
/// Freshness tolerance for the signed timestamp, in seconds, either
/// way: generous next to the two Workers' shared edge clock, far too
/// small to replay old batches into a new month.
pub const MAX_TIMESTAMP_SKEW_SECS: u64 = 300;
/// Windows per batch. The stream DO caps its batches identically; a
/// larger batch is a broken caller, not a retry candidate.
pub const MAX_BATCH_WINDOWS: usize = 64;

/// Why a sink request failed authentication or parsing. Every variant
/// renders the same generic wire answer (401, or 422 for a signed but
/// malformed batch); the split exists for tests and logs that stay
/// server-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkError {
    /// No usable secret provisioned: fail closed, verify nothing.
    NotConfigured,
    /// Header absent/malformed, signature mismatch, or stale.
    Unauthorized,
    /// Signature valid but the batch is unreadable or oversized.
    Malformed,
}

/// One play's cumulative window on the wire. Field-for-field the
/// stream side's `WindowReport` (`workers/stream/src/meter_sink.rs`);
/// the crates share no code, so the shape is pinned by tests on both
/// ends and by the signing vector.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowReport {
    pub user_id: String,
    pub track_id: String,
    pub play_id: String,
    pub union_bytes: u64,
    pub file_bytes: Option<u64>,
    pub started_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlushBatch {
    windows: Vec<WindowReport>,
}

/// Metadata the derivation needs beyond the window itself, resolved
/// per (user, track): media rows are per-user, so the play's user
/// scopes the bitrate/file-size rungs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackMeterMeta {
    pub track_duration_ms: Option<u64>,
    pub mb_duration_ms: Option<u64>,
    pub bitrate_kbps: Option<u64>,
    pub file_bytes: Option<u64>,
}

/// What one window write did, for the ack dispositions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayWindowOutcome {
    /// The upsert inserted or advanced the row.
    Written,
    /// A stored row already covers this union (duplicate/older flush).
    Covered,
    /// The writer fence refused it (deleting or nonexistent user).
    Fenced,
}

/// App-side metadata for a local track id or recording MBID ($1) and user ($2).
/// Exact local ids take precedence. A recording-only match with multiple
/// occurrences is ambiguous, so callers must suppress its artifact rungs.
/// `track_mbid` is the nullable key for the separate mirror lookup.
pub fn track_meter_meta_app_statement() -> &'static str {
    "SELECT t.duration_ms AS track_duration_ms, t.mbid::text AS track_mbid, \
     (SELECT m.bitrate FROM track_media m WHERE m.track_id = t.id AND m.user_id = $2 \
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS bitrate_kbps, \
     (SELECT c.byte_size FROM track_media m JOIN cache_objects c \
     ON c.content_hash = m.content_hash WHERE m.track_id = t.id AND m.user_id = $2 \
     ORDER BY m.verified_at DESC, m.content_hash ASC LIMIT 1) AS file_bytes, \
     (t.id <> $1::uuid AND (SELECT COUNT(*) FROM tracks t2 WHERE t2.mbid = $1::uuid) > 1) \
     AS occurrence_ambiguous \
     FROM tracks t WHERE t.id = $1::uuid OR t.mbid = $1::uuid \
     ORDER BY (t.id = $1::uuid) DESC, t.id LIMIT 1"
}

/// Mirror-side recording duration for the recording MBID ($1).
pub fn track_meter_meta_mirror_statement() -> &'static str {
    "SELECT duration_ms AS mb_duration_ms FROM mb_recording WHERE mbid = $1::uuid"
}

/// True when `track_id` is shaped like the UUID `tracks.id` carries
/// (8-4-4-4-12 hex). Non-UUID track ids skip the metadata read and
/// meter on the fallback rung instead of failing a cast.
pub fn is_uuid_shaped(track_id: &str) -> bool {
    let bytes = track_id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

fn signature_hex(secret: &[u8], timestamp: u64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex_lower(&mac.finalize().into_bytes())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).expect("hex digit"));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).expect("hex digit"));
    }
    out
}

/// Build the `Authorization` header value for a flush body — the
/// stream signer mirrored here so the wire scheme is pinned on both
/// ends (and handler tests can mint real requests). `None` when the
/// secret is unusable, exactly like the stream side.
pub fn authorization_header(secret: Option<&[u8]>, timestamp: u64, body: &[u8]) -> Option<String> {
    let secret = secret.filter(|s| s.len() >= MIN_SECRET_BYTES)?;
    Some(format!(
        "{SINK_AUTH_SCHEME} {timestamp}.{}",
        signature_hex(secret, timestamp, body)
    ))
}

/// Verify one sink request: provisioned secret, well-formed header,
/// fresh timestamp, constant-time MAC comparison over
/// `"<timestamp>.<body>"`.
pub fn verify(
    secret: Option<&[u8]>,
    authorization: Option<&str>,
    body: &[u8],
    now_unix: u64,
) -> Result<(), SinkError> {
    let Some(secret) = secret.filter(|s| s.len() >= MIN_SECRET_BYTES) else {
        return Err(SinkError::NotConfigured);
    };
    let Some(header) = authorization else {
        return Err(SinkError::Unauthorized);
    };
    let Some(credentials) = header.strip_prefix(SINK_AUTH_SCHEME) else {
        return Err(SinkError::Unauthorized);
    };
    let Some(credentials) = credentials.strip_prefix(' ') else {
        return Err(SinkError::Unauthorized);
    };
    let Some((timestamp, signature)) = credentials.split_once('.') else {
        return Err(SinkError::Unauthorized);
    };
    let Ok(timestamp) = timestamp.parse::<u64>() else {
        return Err(SinkError::Unauthorized);
    };
    if now_unix.abs_diff(timestamp) > MAX_TIMESTAMP_SKEW_SECS {
        return Err(SinkError::Unauthorized);
    }
    let expected = signature_hex(secret, timestamp, body);
    if expected.as_bytes().ct_eq(signature.as_bytes()).into() {
        Ok(())
    } else {
        Err(SinkError::Unauthorized)
    }
}

/// Parse a verified flush body into its windows. Signed-but-malformed
/// batches (bad JSON, unknown fields, over the window cap) refuse
/// whole: partially applying a broken batch would make the ack lie
/// about which windows landed.
pub fn parse_batch(body: &[u8]) -> Result<Vec<WindowReport>, SinkError> {
    let batch: FlushBatch = serde_json::from_slice(body).map_err(|_| SinkError::Malformed)?;
    if batch.windows.len() > MAX_BATCH_WINDOWS {
        return Err(SinkError::Malformed);
    }
    Ok(batch.windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"meter-sink-test-secret-32-bytes!!";
    const NOW: u64 = 1_893_456_000;

    #[test]
    fn signing_matches_the_shared_vector() {
        // Python hmac oracle (independent of both crates), mirrored
        // in the stream crate's meter_sink tests:
        // hmac-sha256(SECRET, "1893456000." + '{"windows":[]}').
        let header = authorization_header(Some(SECRET), NOW, br#"{"windows":[]}"#).expect("signs");
        assert_eq!(
            header,
            "MeterSink 1893456000.\
             c4fd315da275680a274b8d3001f8df5e1633c11864cf0daf6dc65ecfab0ea3bb"
        );
    }

    #[test]
    fn verify_accepts_fresh_and_refuses_the_rest() {
        let body = br#"{"windows":[]}"#;
        let header = authorization_header(Some(SECRET), NOW, body).expect("signs");
        assert_eq!(verify(Some(SECRET), Some(&header), body, NOW), Ok(()));
        // Boundary freshness both ways.
        assert_eq!(
            verify(
                Some(SECRET),
                Some(&header),
                body,
                NOW + MAX_TIMESTAMP_SKEW_SECS
            ),
            Ok(())
        );
        assert_eq!(
            verify(
                Some(SECRET),
                Some(&header),
                body,
                NOW + MAX_TIMESTAMP_SKEW_SECS + 1
            ),
            Err(SinkError::Unauthorized)
        );
        assert_eq!(
            verify(
                Some(SECRET),
                Some(&header),
                body,
                NOW - MAX_TIMESTAMP_SKEW_SECS - 1
            ),
            Err(SinkError::Unauthorized)
        );
        // Tampered body and wrong secret.
        assert_eq!(
            verify(Some(SECRET), Some(&header), br#"{"windows":[1]}"#, NOW),
            Err(SinkError::Unauthorized)
        );
        assert_eq!(
            verify(
                Some(b"another-meter-sink-secret-32-byte"),
                Some(&header),
                body,
                NOW
            ),
            Err(SinkError::Unauthorized)
        );
        // Absent/malformed headers.
        for bad in [
            None,
            Some(""),
            Some("Bearer x"),
            Some("MeterSink "),
            Some("MeterSink soon.00"),
            Some("MeterSink 1893456000"),
        ] {
            assert_eq!(
                verify(Some(SECRET), bad, body, NOW),
                Err(SinkError::Unauthorized),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn verify_fails_closed_without_a_provisioned_secret() {
        let body = br#"{"windows":[]}"#;
        let header = authorization_header(Some(SECRET), NOW, body).expect("signs");
        assert_eq!(
            verify(None, Some(&header), body, NOW),
            Err(SinkError::NotConfigured)
        );
        assert_eq!(
            verify(Some(b"short"), Some(&header), body, NOW),
            Err(SinkError::NotConfigured)
        );
        assert!(authorization_header(None, NOW, body).is_none());
        assert!(authorization_header(Some(b"short"), NOW, body).is_none());
    }

    #[test]
    fn parse_batch_bounds_and_shapes() {
        let one = br#"{"windows":[{"user_id":"u1","track_id":"t1","play_id":"p1",
            "union_bytes":42,"file_bytes":1000,"started_at":7}]}"#;
        let windows = parse_batch(one).expect("parses");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].union_bytes, 42);
        assert_eq!(windows[0].file_bytes, Some(1000));
        // file_bytes may be absent (null) but unknown fields refuse.
        let null_file = br#"{"windows":[{"user_id":"u1","track_id":"t1","play_id":"p1",
            "union_bytes":42,"file_bytes":null,"started_at":7}]}"#;
        assert_eq!(parse_batch(null_file).expect("parses")[0].file_bytes, None);
        let extra = br#"{"windows":[{"user_id":"u1","track_id":"t1","play_id":"p1",
            "union_bytes":42,"file_bytes":null,"started_at":7,"seconds":9}]}"#;
        assert_eq!(parse_batch(extra), Err(SinkError::Malformed));
        assert_eq!(parse_batch(b"not json"), Err(SinkError::Malformed));
        assert_eq!(parse_batch(br#"{"windows":[]}"#), Ok(vec![]));
    }

    #[test]
    fn uuid_shape_gate() {
        assert!(is_uuid_shaped("3f6f1b2a-9c4d-4e5f-8a7b-1c2d3e4f5a6b"));
        assert!(is_uuid_shaped("3F6F1B2A-9C4D-4E5F-8A7B-1C2D3E4F5A6B"));
        for bad in [
            "track-1",
            "",
            "3f6f1b2a9c4d4e5f8a7b1c2d3e4f5a6b",
            "3f6f1b2a-9c4d-4e5f-8a7b-1c2d3e4f5a6",
            "zf6f1b2a-9c4d-4e5f-8a7b-1c2d3e4f5a6b",
        ] {
            assert!(!is_uuid_shaped(bad), "{bad}");
        }
    }

    #[test]
    fn meta_statements_are_pinned() {
        let app = track_meter_meta_app_statement();
        assert!(!app.contains("mb_recording"));
        assert!(app.contains("FROM tracks t WHERE t.id = $1::uuid"));
        assert!(app.contains("m.user_id = $2"));
        assert!(app.contains("OR t.mbid = $1::uuid"));
        assert!(app.contains("ORDER BY (t.id = $1::uuid) DESC, t.id LIMIT 1"));
        assert!(app.contains("COUNT(*) FROM tracks t2 WHERE t2.mbid = $1::uuid"));
        for alias in [
            "track_duration_ms",
            "track_mbid",
            "bitrate_kbps",
            "file_bytes",
            "occurrence_ambiguous",
        ] {
            assert!(app.contains(&format!("AS {alias}")), "{alias}");
        }
        let mirror = track_meter_meta_mirror_statement();
        assert!(mirror.contains("FROM mb_recording WHERE mbid = $1::uuid"));
        assert!(mirror.contains("AS mb_duration_ms"));
        assert!(!mirror.contains("tracks t"));
    }

    #[test]
    fn constants_are_pinned() {
        assert_eq!(METER_SINK_SECRET_VAR, "METER_SINK_SECRET");
        assert_eq!(SINK_PATH, "/internal/v1/play-windows");
        assert_eq!(SINK_AUTH_SCHEME, "MeterSink");
        assert_eq!(MIN_SECRET_BYTES, 32);
        assert_eq!(MAX_TIMESTAMP_SKEW_SECS, 300);
        assert_eq!(MAX_BATCH_WINDOWS, 64);
    }
}
