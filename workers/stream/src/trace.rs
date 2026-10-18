//! Stream trace correlation: optional 128-bit trace IDs threaded through the
//! play path, plus a hand-written six-key JSON log envelope.
//!
//! This module renders the envelope by hand (no `serde`, no `nntune-logging`
//! in `trace.rs`). The `stream` crate itself already uses `serde`/`serde_json`
//! for the pool-probe and body JSON (`probe.rs`, `body.rs`) but does not
//! depend on `nntune-logging`. Trace IDs are therefore validated and rendered
//! by hand here; a future production Worker transport would reuse the same IDs
//! by constructing `nntune_logging::LogRecord::new(Worker::Stream, ...)` with
//! the trace ID this module normalizes. The two renderers agree on the contract in
//! `docs/logging-standard.md`: `stream` records carry the six shared fields
//! (`ts`, `level`, `worker`, `trace_id`, `event`, `fields`) as a superset —
//! legacy extras allowed — with the legacy lowercase `[a-z0-9_.]` event
//! vocabulary (no dot required).
//!
//! Analytics Engine shape is untouched: [`crate::meter::MeterEvent`] gains no
//! field. Trace correlation lives in the log envelope and in the
//! [`NormalizedTrace`] plumbing below (`PlayInput` → `PlayStream` → orphan
//! queue), so AE rows keep their exact fields while log lines sharing one
//! `trace_id` correlate grant authorize, pool lease, cache/miss and meter
//! emission for a single play.
//!
//! Privacy: `fields` carry only operational facts (opaque IDs, counters,
//! fixed labels). Construction rejects banned keys and secret/personal
//! values with [`FieldRejection`] — a rejected record is never a log line,
//! never an echoed value.

/// Worker name stamped in every envelope rendered here.
pub const WORKER_NAME: &str = "stream";

/// Replacement marker for a value that must be represented for diagnosis but
/// may contain secret or personal data. Callers substitute before logging;
/// the renderer never invents redaction at the sink.
pub const REDACTED: &str = "[REDACTED]";

/// Legacy stream log events (lowercase `[a-z0-9_.]`, no dot required). These
/// mirror the [`crate::meter::MeterKind`] vocabulary without renaming it:
/// the AE event set is unchanged, these are the log-envelope names.
pub mod event {
    /// Grant verified, play admitted (mirrors `MeterKind::PlayStart`).
    pub const PLAY_START: &str = "play_start";
    /// Served from cache (mirrors `MeterKind::CacheHit`).
    pub const CACHE_HIT: &str = "cache_hit";
    /// Cache missed, pool path taken (mirrors `MeterKind::CacheMiss`).
    pub const CACHE_MISS: &str = "cache_miss";
    /// Bytes handed to the client (mirrors `MeterKind::BytesServed`).
    pub const BYTES_SERVED: &str = "bytes_served";
    /// Next-track prefetch queued (mirrors `MeterKind::PrefetchEnqueued`).
    pub const PREFETCH_ENQUEUED: &str = "prefetch_enqueued";
    /// Provider connection failed (mirrors `MeterKind::ProviderError`).
    pub const PROVIDER_ERROR: &str = "provider_error";
}

/// A validated trace ID: exactly 32 lowercase hexadecimal characters
/// (128 bits). Anything else is invalid and must never be trusted or logged.
#[must_use]
pub fn is_valid_trace_id(raw: &str) -> bool {
    raw.len() == 32
        && raw
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Accept an inbound trace ID only when valid; otherwise drop it (return
/// `None`) so the caller generates or omits instead of logging garbage.
/// Mirrors `nntune_logging::resolve_trace_id` without the RNG: ID generation
/// belongs at the HTTP transport, which owns entropy.
#[must_use]
pub fn normalize_trace_id(raw: Option<&str>) -> Option<String> {
    match raw {
        Some(value) if is_valid_trace_id(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Marker for a log record whose build identity is unwired: the transport did
/// not supply a build SHA, so the line carries no release attribution.
/// Mirrors `nntune_logging::BUILD_SHA_UNKNOWN` — same literal, agreed by
/// contract instead of shared (the `stream` crate does not depend on
/// `nntune-logging`).
pub const BUILD_SHA_UNKNOWN: &str = "unknown";

/// True when the SHA is a real 40-char lowercase hex build identity, false
/// for [`BUILD_SHA_UNKNOWN`] and anything else unparseable.
/// Mirrors `nntune_logging::is_build_sha_known` exactly.
#[must_use]
pub fn is_build_sha_known(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Normalize a candidate build SHA to the log-safe form: a 40-char lowercase
/// hex commit SHA passes through, anything else (absent, empty, truncated,
/// uppercase, non-hex) becomes [`BUILD_SHA_UNKNOWN`]. Never logs the raw
/// candidate; an invalid value is dropped, never trusted. Mirrors
/// `nntune_logging::parse_build_sha`; a future stream transport calls this on
/// its build-SHA var before inserting `build_sha` into [`LogFields`].
#[must_use]
pub fn normalize_build_sha(raw: Option<&str>) -> String {
    match raw {
        Some(value) if is_build_sha_known(value) => value.to_string(),
        _ => BUILD_SHA_UNKNOWN.to_string(),
    }
}

/// Stream event rule from the standard: lowercase `[a-z0-9_.]`, non-empty, no
/// leading/trailing `.`/`_`, max 96 chars. No dot required (legacy
/// vocabulary such as `play_start` stays valid).
#[must_use]
pub fn is_valid_event(event: &str) -> bool {
    !event.is_empty()
        && event.len() <= 96
        && !event.starts_with(['.', '_'])
        && !event.ends_with(['.', '_'])
        && event
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// Level names from the standard.
#[must_use]
pub fn is_valid_level(level: &str) -> bool {
    matches!(level, "debug" | "info" | "warn" | "error")
}

/// Timestamp shape gate: RFC 3339 UTC ending in `Z` with a `T` separator.
/// Full calendar validation lives in the acceptance tests; this keeps a
/// malformed caller timestamp from becoming a conforming-looking line.
#[must_use]
pub fn is_valid_ts(ts: &str) -> bool {
    let bytes = ts.as_bytes();
    bytes.len() >= 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes.last() == Some(&b'Z')
}

const BANNED_KEY_PARTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "token",
    "cookie",
    "authorization",
    "private_key",
    "mnemonic",
    "ssn",
    "credit_card",
    "card_number",
    "cvv",
    "email",
    "session",
    "address",
    "phone",
    "payment",
    "postal",
];

const BANNED_NAME_PARTS: &[&str] = &[
    "username",
    "firstname",
    "lastname",
    "fullname",
    "displayname",
    "givenname",
    "familyname",
    "nickname",
    "surname",
];

/// A `fields` key is banned when it names personal data or credentials. Same
/// rule as the shared emitter: an `ip` path component (without flagging
/// `ship_id`/`zip_code`), a name component, or any credential fragment.
#[must_use]
pub fn is_banned_field_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let has_ip_component = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| part == "ip")
        || ["clientip", "remoteip", "ipaddress"]
            .iter()
            .any(|part| lower.contains(part));
    let has_name_component = lower == "name"
        || lower.starts_with("name_")
        || lower.contains("_name")
        || BANNED_NAME_PARTS.iter().any(|part| lower.contains(part));
    has_ip_component
        || has_name_component
        || BANNED_KEY_PARTS.iter().any(|part| lower.contains(part))
}

const SECRET_VALUE_PREFIXES: &[&str] = &[
    "bearer ", "token ", "sk-", "sk_live_", "sk_test_", "tok_", "ghp_", "gho_", "ghu_", "xoxb-",
    "xoxp-",
];

const SECRET_VALUE_MARKERS: &[&str] = &[
    "password=",
    "password:",
    "secret=",
    "secret:",
    "apikey=",
    "api_key=",
];

/// A string value is rejected when it opens with a known credential prefix or
/// embeds a `key=value` credential marker. Benign labels (`task-queued`)
/// pass, including the [`REDACTED`] marker itself.
#[must_use]
pub fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    SECRET_VALUE_PREFIXES
        .iter()
        .any(|marker| lower.starts_with(marker))
        || SECRET_VALUE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
}

/// A dotted-quad of valid octets is an IP address and is rejected;
/// out-of-range quads are not IPs.
#[must_use]
pub fn looks_like_ipv4_value(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 4 && parts.iter().all(|part| part.parse::<u8>().is_ok())
}

/// An `addr@domain.tld` shape is an email address and is rejected.
#[must_use]
pub fn looks_like_email_value(value: &str) -> bool {
    let mut parts = value.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => {
            !local.is_empty() && !domain.is_empty() && domain.contains('.')
        }
        _ => false,
    }
}

/// A hex-colon IP literal (`::1`, `2001:db8::1`, full 8-group form) is an IP
/// address and is rejected; colon-joined operational refs pass.
#[must_use]
pub fn looks_like_ipv6_value(value: &str) -> bool {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit() || b == b':') {
        return false;
    }
    value.contains("::") || value.split(':').count() == 8
}

/// Why a field was refused. Names the key for banned/nested cases only;
/// values are never echoed, so an error path cannot leak what it rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldRejection {
    BannedKey(String),
    SecretValue,
    PiiValue,
    IpValue,
}

/// A flat `fields` value: strings, integers, booleans, or null. No nesting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    Str(String),
    UInt(u64),
    Int(i64),
    Bool(bool),
    Null,
}

/// Validated flat field map. Insertions run the privacy backstop, so a
/// `LogFields` that exists is already safe to render.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFields {
    fields: Vec<(String, FieldValue)>,
}

impl LogFields {
    #[must_use]
    pub fn new() -> Self {
        Self { fields: Vec::new() }
    }

    pub fn insert_str(&mut self, key: &str, value: &str) -> Result<(), FieldRejection> {
        check_key(key)?;
        check_str_value(value)?;
        self.fields
            .push((key.to_string(), FieldValue::Str(value.to_string())));
        Ok(())
    }

    pub fn insert_uint(&mut self, key: &str, value: u64) -> Result<(), FieldRejection> {
        check_key(key)?;
        self.fields.push((key.to_string(), FieldValue::UInt(value)));
        Ok(())
    }

    pub fn insert_int(&mut self, key: &str, value: i64) -> Result<(), FieldRejection> {
        check_key(key)?;
        self.fields.push((key.to_string(), FieldValue::Int(value)));
        Ok(())
    }

    pub fn insert_bool(&mut self, key: &str, value: bool) -> Result<(), FieldRejection> {
        check_key(key)?;
        self.fields.push((key.to_string(), FieldValue::Bool(value)));
        Ok(())
    }

    pub fn insert_null(&mut self, key: &str) -> Result<(), FieldRejection> {
        check_key(key)?;
        self.fields.push((key.to_string(), FieldValue::Null));
        Ok(())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    fn render_into(&self, out: &mut String) {
        out.push('{');
        for (i, (key, value)) in self.fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_quoted(key, out);
            out.push(':');
            match value {
                FieldValue::Str(text) => push_quoted(text, out),
                FieldValue::UInt(n) => out.push_str(&n.to_string()),
                FieldValue::Int(n) => out.push_str(&n.to_string()),
                FieldValue::Bool(true) => out.push_str("true"),
                FieldValue::Bool(false) => out.push_str("false"),
                FieldValue::Null => out.push_str("null"),
            }
        }
        out.push('}');
    }
}

fn check_key(key: &str) -> Result<(), FieldRejection> {
    if key.is_empty() || is_banned_field_key(key) {
        return Err(FieldRejection::BannedKey(key.to_string()));
    }
    Ok(())
}

fn check_str_value(value: &str) -> Result<(), FieldRejection> {
    if looks_like_secret_value(value) {
        return Err(FieldRejection::SecretValue);
    }
    if looks_like_email_value(value) {
        return Err(FieldRejection::PiiValue);
    }
    if looks_like_ipv4_value(value) || looks_like_ipv6_value(value) {
        return Err(FieldRejection::IpValue);
    }
    Ok(())
}

fn push_quoted(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Render one newline-delimited JSON log line (no trailing newline) with
/// exactly the six schema keys in canonical order:
/// `ts`, `level`, `worker`, `trace_id`, `event`, `fields`.
///
/// Returns `None` — never a line — when the trace ID, event, level,
/// timestamp, or any field fails validation. `fields` were already checked
/// at insert time, so `None` here means a bad envelope argument.
#[must_use]
pub fn render_log_line(
    ts: &str,
    level: &str,
    trace_id: &str,
    evt: &str,
    fields: &LogFields,
) -> Option<String> {
    if !is_valid_ts(ts)
        || !is_valid_level(level)
        || !is_valid_trace_id(trace_id)
        || !is_valid_event(evt)
    {
        return None;
    }
    let mut out = String::with_capacity(192);
    out.push_str("{\"ts\":");
    push_quoted(ts, &mut out);
    out.push_str(",\"level\":");
    push_quoted(level, &mut out);
    out.push_str(",\"worker\":");
    push_quoted(WORKER_NAME, &mut out);
    out.push_str(",\"trace_id\":");
    push_quoted(trace_id, &mut out);
    out.push_str(",\"event\":");
    push_quoted(evt, &mut out);
    out.push_str(",\"fields\":");
    fields.render_into(&mut out);
    out.push('}');
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
    const TS: &str = "2026-10-15T20:00:00.123Z";

    fn fields_with_track() -> LogFields {
        let mut fields = LogFields::new();
        fields.insert_str("track_id", "track-a").unwrap();
        fields.insert_bool("cache_hit", false).unwrap();
        fields
    }

    #[test]
    fn trace_validation_matches_the_standard() {
        assert!(is_valid_trace_id(TRACE));
        assert!(is_valid_trace_id("00000000000000000000000000000000"));
        for bad in [
            "",
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6",      // 31 chars
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c0",    // 33 chars
            "9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C",     // uppercase
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6g",     // non-hex
            "9f3a4c2e-1b5d-4f6a-8c7e-9d0b1a2f4e6c", // dashes
            " Bearer abc123",                       // credential-like
        ] {
            assert!(!is_valid_trace_id(bad), "accepted {bad}");
        }
    }

    #[test]
    fn normalize_keeps_valid_ids_and_drops_the_rest() {
        assert_eq!(normalize_trace_id(Some(TRACE)).as_deref(), Some(TRACE));
        for header in [
            None,
            Some(""),
            Some("too-short"),
            Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"),
            Some("Bearer abc123"),
        ] {
            assert_eq!(normalize_trace_id(header), None);
        }
    }

    #[test]
    fn build_sha_normalizes_to_known_or_unknown() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        const SHA_41: &str = "0123456789abcdef0123456789abcdef012345670";
        const SHA_64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(normalize_build_sha(Some(SHA)), SHA);
        assert!(is_build_sha_known(SHA));
        assert!(is_build_sha_known(&normalize_build_sha(Some(SHA))));
        for raw in [
            None,
            Some(""),
            Some("unknown"),
            Some("0123456789abcdef"),                         // truncated
            Some("0123456789ABCDEF0123456789ABCDEF01234567"), // uppercase
            Some("0123456789abcdef0123456789abcdef0123456g"), // non-hex
            Some(SHA_41),                                     // over-length
            Some(SHA_64), // 64-hex digest, never a commit identity
        ] {
            assert_eq!(normalize_build_sha(raw), BUILD_SHA_UNKNOWN, "raw: {raw:?}");
            assert!(!is_build_sha_known(&normalize_build_sha(raw)));
        }
        assert!(!is_build_sha_known(BUILD_SHA_UNKNOWN));
        // Direct bounds: length alone must not confer identity, whatever the
        // caller passes without going through `normalize_build_sha`.
        for sha in [
            "",
            "0123456789abcdef",
            "0123456789ABCDEF0123456789ABCDEF01234567",
            SHA_41,
            SHA_64,
            BUILD_SHA_UNKNOWN,
        ] {
            assert!(!is_build_sha_known(sha), "must be unknown: {sha:?}");
        }
    }

    #[test]
    fn rendered_line_carries_build_sha_literally() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let mut fields = fields_with_track();
        fields
            .insert_str("build_sha", &normalize_build_sha(Some(SHA)))
            .expect("build_sha must pass field validation");
        let line = render_log_line(TS, "info", TRACE, event::PLAY_START, &fields)
            .expect("valid envelope must render");
        let want = format!("\"build_sha\":\"{SHA}\"");
        assert_eq!(line.matches(&want).count(), 1, "line: {line}");
        // The unwired marker renders too, so emitters can always carry the key.
        let mut unknown_fields = fields_with_track();
        unknown_fields
            .insert_str("build_sha", &normalize_build_sha(None))
            .expect("unknown marker must pass field validation");
        let unknown_line = render_log_line(TS, "info", TRACE, event::PLAY_START, &unknown_fields)
            .expect("valid envelope must render");
        assert_eq!(
            unknown_line.matches("\"build_sha\":\"unknown\"").count(),
            1,
            "line: {unknown_line}"
        );
    }

    #[test]
    fn stream_events_keep_the_legacy_vocabulary() {
        for evt in [
            event::PLAY_START,
            event::CACHE_HIT,
            event::CACHE_MISS,
            event::BYTES_SERVED,
            event::PREFETCH_ENQUEUED,
            event::PROVIDER_ERROR,
            "play_start",
            "stream.play.start",
        ] {
            assert!(is_valid_event(evt), "rejected legacy event {evt}");
        }
        // Stream keeps the legacy vocabulary: no worker namespace is enforced,
        // so another worker's dotted name is still a well-formed stream event.
        assert!(is_valid_event("api.x"));
        for bad in ["", ".play", "play_", "PLAY_START", "play start"] {
            assert!(!is_valid_event(bad), "accepted {bad}");
        }
    }

    #[test]
    fn envelope_renders_exactly_the_six_schema_keys() {
        let line = render_log_line(TS, "info", TRACE, event::PLAY_START, &fields_with_track())
            .expect("valid envelope must render");
        assert_eq!(
            line,
            format!(
                "{{\"ts\":\"{TS}\",\"level\":\"info\",\"worker\":\"stream\",\"trace_id\":\"{TRACE}\",\"event\":\"play_start\",\"fields\":{{\"track_id\":\"track-a\",\"cache_hit\":false}}}}"
            )
        );
    }

    #[test]
    fn envelope_rejects_bad_trace_event_level_and_ts() {
        let fields = fields_with_track();
        assert!(render_log_line(TS, "info", "short", event::PLAY_START, &fields).is_none());
        assert!(render_log_line(
            TS,
            "info",
            "9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C",
            event::PLAY_START,
            &fields
        )
        .is_none());
        assert!(render_log_line(TS, "info", TRACE, "", &fields).is_none());
        assert!(render_log_line(TS, "INFO", TRACE, event::PLAY_START, &fields).is_none());
        assert!(render_log_line("", "info", TRACE, event::PLAY_START, &fields).is_none());
        assert!(
            render_log_line("not-a-timestamp", "info", TRACE, event::PLAY_START, &fields).is_none()
        );
    }

    #[test]
    fn fields_reject_banned_keys() {
        for key in [
            "contact_email",
            "client_ip",
            "ip_address",
            "session_id",
            "name",
            "display_name",
            "password",
            "grant_token",
            "api_key",
            "home_address",
            "phone_number",
            "payment_method",
            "postal_code",
            "",
        ] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str(key, "safe").is_err(),
                "accepted banned key {key}"
            );
        }
        for key in [
            "ship_id",
            "zip_code",
            "filename",
            "namespace",
            "trip",
            "track_id",
        ] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str(key, "safe").is_ok(),
                "rejected safe key {key}"
            );
        }
    }

    #[test]
    fn fields_reject_secret_pii_and_ip_values() {
        for value in [
            "Bearer abc123",
            "Token abc123",
            "sk-live-example",
            "sk_live_example",
            "tok_example",
            "password=hunter2",
        ] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str("detail", value).is_err(),
                "accepted secret value {value}"
            );
        }
        let mut ok = LogFields::new();
        assert!(ok.insert_str("stage", "task-queued").is_ok());
        assert!(ok.insert_str("marker", REDACTED).is_ok());

        for value in ["a@example.com", "user+tag@mail.example.co"] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str("contact", value).is_err(),
                "accepted email value {value}"
            );
        }
        let mut fields = LogFields::new();
        assert!(fields.insert_str("contact", "not-an-email").is_ok());

        let mut fields = LogFields::new();
        assert!(fields.insert_str("host", "192.0.2.1").is_err());
        let mut fields = LogFields::new();
        assert!(fields.insert_str("host", "256.0.2.1").is_ok());
        for value in ["::1", "2001:db8::1"] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str("host", value).is_err(),
                "accepted IPv6 value {value}"
            );
        }
        for value in ["nzb:sha256:abc123", "12:00:00", "ingest:u1:abc:upload"] {
            let mut fields = LogFields::new();
            assert!(
                fields.insert_str("detail", value).is_ok(),
                "rejected operational value {value}"
            );
        }
    }

    #[test]
    fn rejection_never_echoes_values() {
        let mut fields = LogFields::new();
        let err = fields.insert_str("detail", "password=hunter2").unwrap_err();
        assert_eq!(err, FieldRejection::SecretValue);
        assert!(!format!("{err:?}").contains("hunter2"));
    }

    #[test]
    fn strings_escape_without_breaking_json_shape() {
        let mut fields = LogFields::new();
        fields.insert_str("note", "a\"b\\c").unwrap();
        fields.insert_uint("bytes", 7).unwrap();
        fields.insert_int("delta", -3).unwrap();
        fields.insert_bool("ok", true).unwrap();
        fields.insert_null("opt").unwrap();
        let line =
            render_log_line(TS, "info", TRACE, event::BYTES_SERVED, &fields).expect("must render");
        assert!(line.contains("\"note\":\"a\\\"b\\\\c\""));
        assert!(line.contains("\"bytes\":7"));
        assert!(line.contains("\"delta\":-3"));
        assert!(line.contains("\"ok\":true"));
        assert!(line.contains("\"opt\":null"));
    }
}
