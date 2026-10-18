//! Structured JSON log records emitted by the `api` worker.
//!
//! Emit-side implementation of the target contract in
//! `docs/logging-standard.md`: every record is one newline-delimited JSON
//! object with exactly the six shared keys (`ts`, `level`, `worker`,
//! `trace_id`, `event`, `fields`). Nothing in this module changes what the
//! other workers emit; `workers/repair` stays out of scope per that doc.
//!
//! Privacy is enforced at the emitter, never delegated to the sink:
//!
//! - [`TraceId::from_header`] accepts an inbound `x-trace-id` value only
//!   when it is exactly 32 lowercase hexadecimal characters, else it
//!   generates a fresh random ID. An invalid value is never logged.
//! - [`LogRecord::field`] replaces the value of any key that names a
//!   secret or personal identifier with [`REDACTED`]. Callers must still
//!   pass only operational facts: untrusted values (bodies, headers,
//!   query strings, raw identifiers) must never reach `field`.
//! - `route` records a static `METHOD path` pair or the `unknown` sentinel
//!   only. The entry resolves both before calling: raw request paths (the
//!   catch-all 404 arm serves arbitrary probe input) must never reach it.
//!
//! Extra deployment facts (`build_sha`/`release`) are not attached here:
//! release tagging lands with the error-tracking slice, which owns the
//! build-identity plumbing. This module carries `route` and `error_class`
//! inside `fields` today.

/// Inbound header carrying the caller's trace ID (`docs/logging-standard.md`).
pub const TRACE_HEADER: &str = "x-trace-id";

/// Marker substituted for a value whose key names a secret or personal
/// identifier (`docs/logging-standard.md`: represent, don't omit, when the
/// field must stay diagnosable).
pub const REDACTED: &str = "[REDACTED]";

/// This worker's stable name in every emitted record.
const WORKER_NAME: &str = "api";

/// Key fragments that mark a field as secret-bearing. Mirrors the
/// `logging_standard` acceptance oracle so emit-side enforcement and the
/// fixture checks agree; keep both in step when either changes.
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
];

/// Personal-name key fragments (same oracle mirror as above).
const NAME_KEY_PARTS: &[&str] = &[
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

/// A 128-bit trace ID rendered as 32 lowercase hexadecimal characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceId(String);

impl TraceId {
    /// Accept the inbound `x-trace-id` value, or generate a fresh random
    /// ID when it is absent or invalid. An invalid value is dropped
    /// silently: it is never logged, echoed, or propagated.
    pub fn from_header(value: Option<&str>) -> Self {
        match value {
            Some(candidate) if is_valid_trace_id(candidate) => Self(candidate.to_string()),
            _ => Self::generate(),
        }
    }

    fn generate() -> Self {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("secure randomness unavailable");
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// The 32-character ID for log records and downstream propagation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_valid_trace_id(candidate: &str) -> bool {
    candidate.len() == 32
        && candidate
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Record severity. The served-request line uses `error` for 5xx and
/// `info` below that; background failures use `error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// A flat field value. The type keeps nested objects and arrays
/// unrepresentable, matching the `fields` contract.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Str(String),
    Int(i64),
    Bool(bool),
    Null,
}

impl FieldValue {
    fn json(&self) -> serde_json::Value {
        match self {
            FieldValue::Str(s) => serde_json::Value::String(s.clone()),
            FieldValue::Int(n) => serde_json::Value::from(*n),
            FieldValue::Bool(b) => serde_json::Value::from(*b),
            FieldValue::Null => serde_json::Value::Null,
        }
    }
}

impl From<&str> for FieldValue {
    fn from(value: &str) -> Self {
        FieldValue::Str(value.to_string())
    }
}

impl From<String> for FieldValue {
    fn from(value: String) -> Self {
        FieldValue::Str(value)
    }
}

impl From<i64> for FieldValue {
    fn from(value: i64) -> Self {
        FieldValue::Int(value)
    }
}

impl From<u16> for FieldValue {
    fn from(value: u16) -> Self {
        FieldValue::Int(i64::from(value))
    }
}

impl From<u64> for FieldValue {
    fn from(value: u64) -> Self {
        FieldValue::Int(value.min(i64::MAX as u64) as i64)
    }
}

impl From<bool> for FieldValue {
    fn from(value: bool) -> Self {
        FieldValue::Bool(value)
    }
}

/// Rejection reason for an event name that violates the envelope contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEvent;

impl std::fmt::Display for InvalidEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event must be a lowercase `api.*` name")
    }
}

fn valid_event(event: &str) -> bool {
    if event.is_empty() || event.len() > 96 || !event.starts_with("api.") {
        return false;
    }
    event
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
        && !event.starts_with(['.', '_'])
        && !event.ends_with(['.', '_'])
}

/// One structured log record. Build with [`LogRecord::new`], attach fields,
/// then [`LogRecord::render`] (tests) or [`LogRecord::emit`] (Worker).
#[derive(Clone, Debug, PartialEq)]
pub struct LogRecord {
    level: Level,
    event: String,
    trace_id: TraceId,
    fields: Vec<(String, FieldValue)>,
}

impl LogRecord {
    /// Start a record. `event` must be a lowercase `api.*` name per the
    /// logging standard (`api.auth.login`); anything else is rejected so a
    /// typo can never ship an off-contract event.
    pub fn new(level: Level, event: &str, trace_id: TraceId) -> Result<Self, InvalidEvent> {
        if !valid_event(event) {
            return Err(InvalidEvent);
        }
        Ok(Self {
            level,
            event: event.to_string(),
            trace_id,
            fields: Vec::new(),
        })
    }

    /// Attach a field. A key that names a secret or personal identifier
    /// keeps its key but the value is replaced with [`REDACTED`].
    pub fn field(mut self, key: &str, value: impl Into<FieldValue>) -> Self {
        let stored = if key_is_banned(key) {
            FieldValue::Str(REDACTED.to_string())
        } else {
            value.into()
        };
        self.fields.push((key.to_string(), stored));
        self
    }

    /// Record the served route as the static `METHOD path` pair. Pass only
    /// a known static path or the `unknown` sentinel (the entry resolves
    /// both before calling): raw request paths must never reach this, as
    /// the catch-all 404 arm serves arbitrary probe paths.
    pub fn route(self, method: &str, path: &str) -> Self {
        self.field("route", format!("{method} {path}"))
    }

    /// Grouping key for failures: the shared `{code, message, retryable}`
    /// registry code from `docs/api-errors.md`.
    pub fn error_class(self, code: &str) -> Self {
        self.field("error_class", code)
    }

    /// Render the six-key envelope as one JSON line. Keys sort
    /// alphabetically (`serde_json` map order), so output is deterministic.
    pub fn render(&self, ts_millis: u64) -> String {
        let mut fields = serde_json::Map::with_capacity(self.fields.len());
        for (key, value) in &self.fields {
            fields.insert(key.clone(), value.json());
        }
        serde_json::json!({
            "ts": format_ts(ts_millis),
            "level": self.level.as_str(),
            "worker": WORKER_NAME,
            "trace_id": self.trace_id.as_str(),
            "event": self.event,
            "fields": fields,
        })
        .to_string()
    }

    /// Render with the current clock and write to the Worker console.
    /// Native builds (tests) have no console: rendering is covered by
    /// [`LogRecord::render`] there instead.
    pub fn emit(&self) {
        let line = self.render(now_millis());
        #[cfg(target_arch = "wasm32")]
        worker::console_log!("{}", line);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = line;
    }
}

/// Emit-side privacy gate: true when the key names a secret or personal
/// identifier. Mirrors the `logging_standard` acceptance oracle (fragment
/// list plus the split-based `ip` guard and the `name` guard that lets
/// `filename`, `namespace`, `zip_code`, and `trip` through).
pub fn key_is_banned(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let has_ip_component = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|part| part == "ip")
        || ["clientip", "remoteip", "ipaddress"]
            .iter()
            .any(|part| lower.contains(part));
    let has_name_component = lower == "name"
        || lower.starts_with("name_")
        || lower.contains("_name")
        || NAME_KEY_PARTS.iter().any(|part| lower.contains(part));
    has_ip_component
        || has_name_component
        || BANNED_KEY_PARTS.iter().any(|part| lower.contains(part))
}

/// Wall-clock milliseconds for record timestamps.
#[cfg(target_arch = "wasm32")]
pub fn now_millis() -> u64 {
    worker::Date::now().as_millis()
}

/// Wall-clock milliseconds for record timestamps (native test builds).
#[cfg(not(target_arch = "wasm32"))]
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Format epoch milliseconds as RFC 3339 UTC with millis (`...SS.sssZ`).
pub fn format_ts(millis: u64) -> String {
    let secs = millis / 1000;
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        millis % 1000,
    )
}

/// Howard Hinnant's days-to-civil algorithm (proleptic Gregorian).
/// Input is days since 1970-01-01; the shift re-bases it to the
/// algorithm's 0000-03-01 epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic fixture trace ID (same shape as the doc fixtures; never a
    /// real request identifier).
    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";

    fn trace() -> TraceId {
        TraceId::from_header(Some(TRACE))
    }

    fn record() -> LogRecord {
        LogRecord::new(Level::Info, "api.auth.login", trace()).expect("fixture event is valid")
    }

    #[test]
    fn trace_id_accepts_exact_lowercase_hex_and_rejects_the_rest() {
        assert_eq!(TraceId::from_header(Some(TRACE)).as_str(), TRACE);
        // Uppercase, wrong length, non-hex, and padded values all fail.
        for bad in [
            "9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C",
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6",
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6cc",
            "zf3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
            " 9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
            "",
        ] {
            let id = TraceId::from_header(Some(bad)).as_str().to_string();
            assert_ne!(id, bad, "invalid header value must never be kept: {bad:?}");
            assert!(
                is_valid_trace_id(&id),
                "generated replacement must be valid: {id}"
            );
        }
        // Absent header generates a valid ID, and two generations differ.
        let first = TraceId::from_header(None);
        let second = TraceId::from_header(None);
        assert!(is_valid_trace_id(first.as_str()));
        assert_ne!(first, second, "generated trace IDs must not repeat");
    }

    #[test]
    fn event_names_must_be_namespaced_api_events() {
        for good in [
            "api.auth.login",
            "api.request.served",
            "api.db.connection_error",
            "api.start.misconfigured",
        ] {
            assert!(
                LogRecord::new(Level::Info, good, trace()).is_ok(),
                "must accept {good}"
            );
        }
        for bad in [
            "",
            "API.auth.login",
            "api.Auth.login",
            "api.auth-login",
            "api.auth login",
            "stream.play.start",
            "api",
            "api.",
            ".api.auth",
            "api.auth.",
            "other.worker.event",
        ] {
            assert!(
                LogRecord::new(Level::Info, bad, trace()).is_err(),
                "must reject {bad:?}"
            );
        }
        assert!(
            LogRecord::new(Level::Info, &format!("api.{}", "e".repeat(92)), trace()).is_ok(),
            "96-char event is the limit"
        );
        assert!(
            LogRecord::new(Level::Info, &format!("api.{}", "e".repeat(93)), trace()).is_err(),
            "97-char event exceeds the limit"
        );
    }

    #[test]
    fn banned_keys_keep_the_key_but_redact_the_value() {
        // Every key the acceptance oracle forbids redacts here too.
        for key in [
            "password",
            "grant_token",
            "api_key",
            "session_id",
            "contact_email",
            "client_ip",
            "ip_address",
            "name",
            "display_name",
            "nickname",
            "surname",
            "cookie",
            "authorization",
            "mnemonic",
            "private_key",
        ] {
            assert!(key_is_banned(key), "must ban {key}");
            let line = record().field(key, "some-value").render(0);
            assert!(
                line.contains(&format!("\"{key}\":\"{REDACTED}\"")),
                "value for {key} must redact, got: {line}"
            );
            assert!(
                !line.contains("some-value"),
                "secret value for {key} must not appear, got: {line}"
            );
        }
        // ... while operational keys the oracle allows pass through.
        for key in ["ship_id", "zip_code", "filename", "namespace", "trip"] {
            assert!(!key_is_banned(key), "must allow {key}");
        }
        let line = record()
            .field("status_code", 200i64)
            .field("duration_ms", 12i64)
            .render(0);
        assert!(line.contains("\"status_code\":200"));
        assert!(line.contains("\"duration_ms\":12"));
    }

    #[test]
    fn rendered_line_is_exactly_the_six_key_envelope() {
        let line = record()
            .route("POST", "/v1/auth/magic/start")
            .field("status_code", 200i64)
            .error_class("internal_error")
            .render(1_789_123_456_789);
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("record must be one JSON object");
        let object = parsed.as_object().expect("record must be an object");
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["event", "fields", "level", "trace_id", "ts", "worker"],
            "exactly the six envelope keys, got: {line}"
        );
        assert!(!line.contains('\n'), "record must stay one line");
        assert_eq!(object["worker"], "api");
        assert_eq!(object["trace_id"], TRACE);
        assert_eq!(object["event"], "api.auth.login");
        assert_eq!(object["level"], "info");
        assert_eq!(object["fields"]["route"], "POST /v1/auth/magic/start");
        assert_eq!(object["fields"]["error_class"], "internal_error");
    }

    #[test]
    fn timestamps_cover_epoch_leap_day_and_millis() {
        assert_eq!(format_ts(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_ts(1_789_123_456_789), "2026-09-11T10:44:16.789Z");
        // 2000-02-29 exists (leap year); 1900-02-29 must never render.
        assert_eq!(format_ts(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(format_ts(1_000), "1970-01-01T00:00:01.000Z");
    }
}
