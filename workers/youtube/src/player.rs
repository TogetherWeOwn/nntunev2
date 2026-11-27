//! `/player` response parsing: playability, stream URLs, `ip=` extraction.
//!
//! The Day-1 measurement records, per video, the player `status` (usually
//! `OK`, or `LOGIN_REQUIRED` when the extraction IP is bot-walled) and the
//! `ip=` query value of the returned stream URLs, then fetches byte ranges
//! from Worker egress. This module parses that record out of the raw
//! `/player` JSON. It never fetches; the Worker entry does the I/O.

use crate::client::is_url_token;

/// Playability classes the gate cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Playability {
    /// `status == "OK"`: the response may carry stream URLs.
    Ok,
    /// `status == "LOGIN_REQUIRED"`: the extraction IP is bot-walled (or
    /// the video needs auth); retry through the proxy for `/player`.
    LoginRequired,
    /// `status == "ERROR"`: the video is unavailable (deleted, private,
    /// region-blocked); no retry.
    Error,
    /// Any other or missing status.
    #[default]
    Unknown,
}

impl Playability {
    /// Classify a raw `playabilityStatus.status` string.
    pub fn from_status(status: &str) -> Self {
        match status {
            "OK" => Playability::Ok,
            "LOGIN_REQUIRED" => Playability::LoginRequired,
            "ERROR" => Playability::Error,
            _ => Playability::Unknown,
        }
    }
}

/// One parsed stream URL with the flags the gate correlates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFormat {
    /// Innertube itag (0 when the response omits it).
    pub itag: i64,
    /// Innertube `bitrate` in bits per second, when present. The audio
    /// selector ranks fallback Opus formats by this.
    pub bitrate: Option<u64>,
    /// MIME type with codecs, when present.
    pub mime: Option<String>,
    /// Plain stream URL; `None` when the format only carries a
    /// `signatureCipher` (VISIONOS must never hit that path).
    pub url: Option<String>,
    /// The format carries a `signatureCipher` instead of (or besides) a URL.
    pub has_signature_cipher: bool,
    /// The URL carries an `n` parameter (throttling challenge).
    pub has_n_param: bool,
    /// The URL looks like SABR (see [`is_sabr_url`]).
    pub has_sabr: bool,
    /// The URL's `ip=` query value (extraction-IP echo).
    pub ip: Option<String>,
}

impl StreamFormat {
    pub(crate) fn has_plain_url(&self) -> bool {
        !self.has_signature_cipher
            && !self.has_n_param
            && !self.has_sabr
            && self.url.as_deref().is_some_and(|url| {
                stream_path(url) == Some("videoplayback")
                    && !query_pairs(url)
                        .any(|(key, _)| !is_url_token(key) || matches!(key, "n" | "sabr"))
            })
    }
}

/// The parsed per-video extraction record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerOutcome {
    /// Raw `playabilityStatus.status` (`"UNKNOWN"` when missing).
    pub status: String,
    /// Classified playability.
    pub playability: Playability,
    /// `playabilityStatus.reason`, when the response gives one.
    pub reason: Option<String>,
    /// `streamingData.formats` then `streamingData.adaptiveFormats`.
    pub formats: Vec<StreamFormat>,
}

/// Parse a `/player` JSON response into the Day-1 measurement record.
pub fn parse_player_response(body: &serde_json::Value) -> PlayerOutcome {
    let status = body
        .get("playabilityStatus")
        .and_then(|s| s.get("status"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UNKNOWN");
    let reason = body
        .get("playabilityStatus")
        .and_then(|s| s.get("reason"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let playability = Playability::from_status(status);
    let mut formats = Vec::new();
    let streaming = body.get("streamingData");
    if let Some(streaming) = streaming.filter(|_| playability == Playability::Ok) {
        for key in ["formats", "adaptiveFormats"] {
            if let Some(list) = streaming.get(key).and_then(serde_json::Value::as_array) {
                formats.extend(
                    list.iter()
                        .filter(|format| format.is_object())
                        .map(parse_format),
                );
            }
        }
    }
    PlayerOutcome {
        status: status.to_string(),
        playability,
        reason,
        formats,
    }
}

fn parse_format(format: &serde_json::Value) -> StreamFormat {
    let url = format
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| stream_path(url).is_some())
        .map(str::to_string);
    StreamFormat {
        itag: format
            .get("itag")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        bitrate: format.get("bitrate").and_then(serde_json::Value::as_u64),
        mime: format
            .get("mimeType")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        has_signature_cipher: ["signatureCipher", "cipher"]
            .iter()
            .any(|key| format.get(key).is_some_and(|value| !value.is_null())),
        has_n_param: url
            .as_deref()
            .is_some_and(|u| query_param(u, "n").is_some()),
        has_sabr: url.as_deref().is_some_and(is_sabr_url),
        ip: url.as_deref().and_then(stream_ip),
        url,
    }
}

fn stream_path(url: &str) -> Option<&str> {
    if url.contains(|ch: char| !ch.is_ascii_graphic() || matches!(ch, '\\' | '#')) {
        return None;
    }
    let (host, target) = url.strip_prefix("https://")?.split_once('/')?;
    let prefix = host.strip_suffix(".googlevideo.com")?;
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.contains(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
    };
    if host.len() > 253 || !prefix.split('.').all(valid_label) {
        return None;
    }
    target.split('?').next()
}

fn query_pairs(url: &str) -> impl Iterator<Item = (&str, &str)> {
    let target = url.split_once('#').map_or(url, |(target, _)| target);
    target
        .split_once('?')
        .into_iter()
        .flat_map(|(_, query)| query.split('&'))
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
}

fn stream_ip(url: &str) -> Option<String> {
    let mut ips = query_pairs(url).filter_map(|(name, value)| (name == "ip").then_some(value));
    let value = ips.next()?.replace("%3A", ":").replace("%3a", ":");
    let ip = value.parse::<std::net::IpAddr>().ok()?;
    ips.next().is_none().then(|| ip.to_string())
}

/// First raw value of query parameter `key`, with a bare key treated as empty.
pub fn query_param(url: &str, key: &str) -> Option<String> {
    query_pairs(url)
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_string())
}

/// SABR heuristic: `/sabr/` in the path or a `sabr=1` query parameter.
/// Validate against live `/player` responses during the Day-1 measurement;
/// VISIONOS is expected to return zero SABR URLs.
pub fn is_sabr_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    if path.contains("/sabr/") {
        return true;
    }
    query_pairs(url).any(|(name, value)| name == "sabr" && value == "1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn playability_status_strings() {
        assert_eq!(Playability::from_status("OK"), Playability::Ok);
        assert_eq!(
            Playability::from_status("LOGIN_REQUIRED"),
            Playability::LoginRequired
        );
        assert_eq!(Playability::from_status("ERROR"), Playability::Error);
        assert_eq!(
            Playability::from_status("LIVE_STREAM_OFFLINE"),
            Playability::Unknown
        );
    }

    #[test]
    fn login_required_fixture_has_no_formats() {
        let body = json!({
            "playabilityStatus": {
                "status": "LOGIN_REQUIRED",
                "reason": "Sign in to confirm you’re not a bot"
            }
        });
        let outcome = parse_player_response(&body);
        assert_eq!(outcome.status, "LOGIN_REQUIRED");
        assert_eq!(outcome.playability, Playability::LoginRequired);
        assert_eq!(
            outcome.reason.as_deref(),
            Some("Sign in to confirm you’re not a bot")
        );
        assert!(outcome.formats.is_empty());
    }

    #[test]
    fn ok_fixture_parses_urls_and_flags() {
        let body = json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {
                "formats": [{
                    "itag": 18,
                    "bitrate": 500000,
                    "mimeType": "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"",
                    "url": "https://rr1---sn-abc123.googlevideo.com/videoplayback?expire=1&ip=203.0.113.7&n=xyz&id=1&itag=18"
                }],
                "adaptiveFormats": [{
                    "itag": 140,
                    "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                    "signatureCipher": "s=ABC&url=https%3A%2F%2Fexample.test%2Fv"
                }]
            }
        });
        let outcome = parse_player_response(&body);
        assert_eq!(outcome.playability, Playability::Ok);
        assert_eq!(outcome.formats.len(), 2);
        let progressive = &outcome.formats[0];
        assert_eq!(progressive.itag, 18);
        assert_eq!(progressive.bitrate, Some(500_000));
        assert!(progressive.url.is_some());
        assert!(!progressive.has_signature_cipher);
        assert!(progressive.has_n_param);
        assert!(!progressive.has_sabr);
        assert_eq!(progressive.ip.as_deref(), Some("203.0.113.7"));
        let audio = &outcome.formats[1];
        assert_eq!(audio.itag, 140);
        assert_eq!(audio.bitrate, None);
        assert_eq!(audio.url, None);
        assert!(audio.has_signature_cipher);
    }

    #[test]
    fn null_signature_cipher_counts_as_absent() {
        let body = json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {
                "formats": [{
                    "itag": 18,
                    "url": "https://rr1---sn-abc123.googlevideo.com/videoplayback?ip=203.0.113.7",
                    "signatureCipher": null
                }]
            }
        });
        let outcome = parse_player_response(&body);
        assert_eq!(outcome.formats.len(), 1);
        assert!(outcome.formats[0].url.is_some());
        assert!(!outcome.formats[0].has_signature_cipher);
    }

    #[test]
    fn missing_status_is_unknown() {
        let outcome = parse_player_response(&json!({}));
        assert_eq!(outcome.status, "UNKNOWN");
        assert_eq!(outcome.playability, Playability::Unknown);
        assert!(outcome.formats.is_empty());
    }

    #[test]
    fn query_param_extracts_ip() {
        let url = "https://host.test/videoplayback?expire=1&ip=203.0.113.7&n=xyz";
        assert_eq!(query_param(url, "ip").as_deref(), Some("203.0.113.7"));
        assert_eq!(query_param(url, "n").as_deref(), Some("xyz"));
        assert_eq!(query_param(url, "missing"), None);
        assert_eq!(query_param("https://host.test/noquery", "ip"), None);
    }

    #[test]
    fn sabr_heuristic() {
        assert!(is_sabr_url("https://host.test/videoplayback/sabr/foo?ip=1"));
        assert!(is_sabr_url("https://host.test/videoplayback?sabr=1&ip=1"));
        assert!(!is_sabr_url(
            "https://host.test/videoplayback?expire=1&ip=203.0.113.7"
        ));
    }
}
