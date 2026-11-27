//! Structured worker log emitter and trace-ID propagation.
//!
//! Implements the target contract in `docs/logging-standard.md` for the
//! `api`, `ingest`, and `mb` workers: newline-delimited JSON records with
//! exactly the six shared fields, 128-bit trace IDs carried on `x-trace-id`,
//! and construction-time privacy validation so a banned key or secret-like
//! value is a rejected record, never a leaked log line.
//!
//! Native + wasm32 compatible. No Worker runtime imports here: callers pass a
//! unix-millis timestamp (production workers use `worker::Date::now()`)
//! and emit the rendered line through `console_log!` themselves.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// Inbound/outbound trace header name. The same ID travels unchanged in
/// downstream HTTP requests, queued messages, and consumers.
pub const TRACE_HEADER: &str = "x-trace-id";

/// Replacement marker for a value that must be represented for diagnosis but
/// may contain secret or personal data. Callers substitute before logging;
/// the emitter never invents redaction at the sink.
pub const REDACTED: &str = "[REDACTED]";

/// Stable worker names from the logging standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Worker {
    Api,
    Ingest,
    Mb,
    Stream,
}

impl Worker {
    /// Wire name used in the `worker` field and as the event namespace prefix.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Worker::Api => "api",
            Worker::Ingest => "ingest",
            Worker::Mb => "mb",
            Worker::Stream => "stream",
        }
    }

    /// Parse a wire name. Used for config/CLI surfaces, never for trusting a
    /// caller-supplied worker label inside a log record.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Worker> {
        match raw {
            "api" => Some(Worker::Api),
            "ingest" => Some(Worker::Ingest),
            "mb" => Some(Worker::Mb),
            "stream" => Some(Worker::Stream),
            _ => None,
        }
    }
}

/// Log severity levels from the logging standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Level> {
        match raw {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// Construction failure. Variants carry only the rejection reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogError {
    BadTraceId,
    BadEvent,
    BadTimestamp,
    FieldsNotAnObject,
    NestedField,
    BannedField,
    SecretValue,
    PiiValue,
    IpAddressValue,
    /// `fields` exceeds the maximum number of entries.
    TooManyFields,
    /// A key is empty, too long, or contains unsupported characters.
    BadFieldKey,
    /// A field string exceeds the maximum UTF-8 byte length.
    FieldValueTooLong,
    NoEntropy,
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogError::BadTraceId => write!(f, "trace_id must be 32 lowercase hex chars"),
            LogError::BadEvent => write!(f, "invalid event name"),
            LogError::BadTimestamp => write!(f, "timestamp exceeds RFC 3339 year range"),
            LogError::FieldsNotAnObject => write!(f, "fields must be a JSON object"),
            LogError::NestedField => write!(f, "nested fields value"),
            LogError::BannedField => write!(f, "banned fields key"),
            LogError::SecretValue => write!(f, "fields value looks like a credential"),
            LogError::PiiValue => write!(f, "fields value looks like personal data"),
            LogError::IpAddressValue => write!(f, "fields value looks like an IP address"),
            LogError::TooManyFields => write!(f, "too many fields"),
            LogError::BadFieldKey => write!(f, "invalid fields key"),
            LogError::FieldValueTooLong => write!(f, "fields value is too long"),
            LogError::NoEntropy => write!(f, "trace-id randomness unavailable"),
        }
    }
}

impl std::error::Error for LogError {}

/// A trace ID is exactly 32 lowercase hexadecimal characters (128 bits).
/// Anything else — absent, short, uppercase, non-hex — is invalid and must
/// never be trusted or logged; callers generate a fresh ID instead.
#[must_use]
pub fn is_valid_trace_id(raw: &str) -> bool {
    raw.len() == 32
        && raw
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Generate a cryptographically random 128-bit trace ID as 32 lowercase
/// hex chars. `getrandom` with the `js` feature serves Web Crypto on wasm
/// targets and the OS RNG natively.
pub fn generate_trace_id() -> Result<String, LogError> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| LogError::NoEntropy)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(out)
}

/// Resolve the request trace: accept the inbound `x-trace-id` header value
/// only when it is a valid 128-bit ID, otherwise generate a fresh one. An
/// invalid header value is dropped, never trusted or logged.
#[must_use]
pub fn resolve_trace_id(header: Option<&str>) -> String {
    match header {
        Some(value) if is_valid_trace_id(value) => value.to_string(),
        _ => generate_trace_id().unwrap_or_else(|_| fallback_trace_id()),
    }
}

/// Last-resort ID when the entropy source fails (production targets always
/// provide entropy; this keeps a handler total instead of panicking the
/// isolate). Fixed prefix marks the weakness so a flood of `f`-prefixed IDs
/// in logs points straight at the RNG, not at a caller. A process-local
/// atomic counter supplies the distinct low bits: unlike wall-clock time it
/// works on every target (`SystemTime::now` panics on wasm32), and it keeps
/// IDs distinct inside one isolate. Obviously not random.
fn fallback_trace_id() -> String {
    static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    // 16 fixed chars + 16 hex chars of counter: full 128-bit shape, valid
    // under `is_valid_trace_id` so downstream propagation never breaks.
    format!("ffffffffffffffff{seq:016x}")
}

/// Explicit marker when the build carried no identity (dev server, preview
/// without env, misconfigured CI). Mirrors the web build-identity surface
/// (`apps/web/src/lib/build.ts`): the `sha` is either a 40-char lowercase
/// hex commit or this exact string, so log grouping never sees a missing key.
pub const BUILD_SHA_UNKNOWN: &str = "unknown";

/// Normalize a candidate build SHA to the log-safe form: a 40-char
/// lowercase hex commit SHA passes through, anything else (absent, empty,
/// truncated, uppercase, non-hex) becomes [`BUILD_SHA_UNKNOWN`]. Never logs
/// the raw candidate; an invalid value is dropped, never trusted.
#[must_use]
pub fn parse_build_sha(raw: Option<&str>) -> String {
    match raw {
        Some(value) if is_build_sha_known(value) => value.to_string(),
        _ => BUILD_SHA_UNKNOWN.to_string(),
    }
}

/// True when the SHA is a real 40-char lowercase hex build identity, false
/// for [`BUILD_SHA_UNKNOWN`] and anything else unparseable.
#[must_use]
pub fn is_build_sha_known(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Format unix millis as an RFC 3339 UTC timestamp ending in `Z`, always with
/// millisecond precision (`2026-10-15T20:00:00.123Z`). Pure civil-date math,
/// no date library, so it behaves identically on native and wasm targets.
#[must_use]
pub fn unix_millis_to_rfc3339(millis: u64) -> String {
    let secs = millis / 1_000;
    let ms = (millis % 1_000) as i64;
    let days = (secs / 86_400) as i64;
    let sod = (secs % 86_400) as i64;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        sod / 3_600,
        (sod % 3_600) / 60,
        sod % 60,
    )
}

/// Howard Hinnant's civil-from-days algorithm: days since 1970-01-01 to
/// (year, month, day). Uses Euclidean division so pre-1970 inputs stay
/// correct (callers never send those, but the math holds).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    if m <= 2 {
        y += 1;
    }
    (y, m, d)
}

/// Event-name rule from the standard: lowercase `[a-z0-9_.]`, non-empty, no
/// leading/trailing `.`/`_`, max 96 chars. `api`, `ingest`, and `mb` events
/// are additionally namespaced (`<worker>.` prefix, which implies the dot);
/// `stream` keeps its legacy vocabulary (no dot required) during migration.
#[must_use]
pub fn is_valid_event_for_worker(worker: Worker, event: &str) -> bool {
    if event.is_empty() || event.len() > 96 {
        return false;
    }
    if !event
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    {
        return false;
    }
    if event.starts_with(['.', '_']) || event.ends_with(['.', '_']) {
        return false;
    }
    match worker {
        Worker::Stream => true,
        _ => {
            // Length-guard before indexing: an exact-prefix event (`api`) is
            // invalid, not a panic — the constructor stays fail-closed.
            let prefix = worker.as_str();
            event.len() > prefix.len()
                && event.starts_with(prefix)
                && event.as_bytes()[prefix.len()] == b'.'
        }
    }
}

const BANNED_KEY_PARTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "apikey",
    "token",
    "credential",
    "passphrase",
    "cookie",
    "authorization",
    "privatekey",
    "mnemonic",
    "ssn",
    "creditcard",
    "cardnumber",
    "cvv",
    "paymentdetails",
    "paymentmethod",
    "email",
    "session",
    "postal",
    "address",
    "phone",
    // NOTE: no bare "payment" — payment_id must stay loggable.
    "street",
    "postcode",
    "zipcode",
    "requestbody",
    "responsebody",
    "headers",
    "rawquery",
    "querystring",
    "nzbcontents",
    "nzbxml",
];

// The key matcher removes separators before checking these name variants.
// Bare name components are checked separately to preserve keys like filename.
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

/// A `fields` key is banned when it names personal data or credentials:
/// an `ip` path component (without flagging `ship_id`/`zip_code`), a name
/// component (`name`, `*_name`, known full-name variants), or any credential
/// fragment (`api_key`, `grant_token`, `cookie`, ...).
#[must_use]
pub fn is_banned_field_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let compact: String = lower.chars().filter(char::is_ascii_alphanumeric).collect();
    let has_sensitive_component = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| matches!(part, "ip" | "name"));
    has_sensitive_component
        || compact == "ip"
        || ["clientip", "remoteip", "ipv4", "ipv6"]
            .iter()
            .any(|part| compact.contains(part))
        || lower.contains("_name")
        || BANNED_NAME_PARTS.iter().any(|part| compact.contains(part))
        || BANNED_KEY_PARTS.iter().any(|part| compact.contains(part))
        || looks_like_secret_value(key)
        || looks_like_ipv4_value(key)
}

const SECRET_VALUE_PREFIXES: &[&str] = &[
    "sk-", "sk_live_", "sk_test_", "tok_", "ghp_", "gho_", "ghu_", "xoxb-", "xoxp-",
];
const CREDENTIAL_LABELS: &[&str] = &[
    "password",
    "secret",
    "apikey",
    "api_key",
    "authorization",
    "cookie",
    "token",
    "passwd",
    "passphrase",
    "credential",
    "private_key",
];

/// Detect known credential prefixes, schemes, and assignments in diagnostic text.
#[must_use]
pub fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_'))
        .any(|word| {
            SECRET_VALUE_PREFIXES
                .iter()
                .any(|prefix| word.starts_with(prefix))
        })
        || has_credential_marker(&lower)
}

#[must_use]
pub fn looks_like_ipv4_value(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '+' | '_'))
        .any(is_dotted_quad)
}

/// Detect an email-shaped span at any `@` in the diagnostic text.
/// Domain matching stops at non-host punctuation and ignores trailing dots.
/// Check each candidate independently when a diagnostic mentions more than one.
#[must_use]
pub fn looks_like_email_value(value: &str) -> bool {
    let parts = value.split('@');
    parts.clone().zip(parts.skip(1)).any(|(local, domain)| {
        let host = domain
            .split(|c: char| !c.is_alphanumeric() && !matches!(c, '.' | '-'))
            .next()
            .unwrap_or_default()
            .trim_end_matches('.');
        !local.is_empty() && !host.starts_with('.') && host.contains('.')
    })
}

/// Detect IPv6 literals, including bracketed, scoped and IPv4-mapped forms.
/// Parse candidates as IPs so clock fragments and operational labels still pass.
#[must_use]
pub fn looks_like_ipv6_value(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, ':' | '.' | '_'))
        .any(|word| word.trim_matches('.').parse::<std::net::Ipv6Addr>().is_ok())
}

/// Validate both JSON objects and maps supplied directly to the constructor.
fn validate_fields<'a>(
    fields: impl ExactSizeIterator<Item = (&'a String, &'a Value)>,
) -> Result<(), LogError> {
    if fields.len() > MAX_FIELDS {
        return Err(LogError::TooManyFields);
    }
    for (key, value) in fields {
        if !is_valid_field_key(key) {
            return Err(LogError::BadFieldKey);
        }
        if is_banned_field_key(key) {
            return Err(LogError::BannedField);
        }
        match value {
            Value::String(text) => validate_field_string(text)?,
            Value::Number(_) | Value::Bool(_) | Value::Null => {}
            Value::Array(_) | Value::Object(_) => return Err(LogError::NestedField),
        }
    }
    Ok(())
}

const MAX_FIELDS: usize = 32;
const MAX_FIELD_KEY_BYTES: usize = 64;
const MAX_FIELD_VALUE_BYTES: usize = 256;

/// Convert a JSON object into a validated field map with up to 32 flat entries.
/// Keys use 1–64 ASCII letters, digits, underscores, dots or hyphens. String
/// values are limited to 256 UTF-8 bytes.
pub fn fields_from_json(value: Value) -> Result<BTreeMap<String, Value>, LogError> {
    match value {
        Value::Object(map) => {
            validate_fields(map.iter())?;
            Ok(map.into_iter().collect())
        }
        _ => Err(LogError::FieldsNotAnObject),
    }
}

/// One structured log record: exactly the six shared fields from the
/// standard. Construction validates the trace ID, the worker-namespaced
/// event, and every field; anything invalid is an `Err`, never a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    ts: String,
    level: Level,
    worker: Worker,
    trace_id: String,
    event: String,
    fields: BTreeMap<String, Value>,
}

impl LogRecord {
    /// Build a record with the same field limits as [`fields_from_json`].
    /// The timestamp is Unix milliseconds through the end of year 9999 UTC.
    pub fn new(
        worker: Worker,
        level: Level,
        trace_id: &str,
        event: &str,
        fields: BTreeMap<String, Value>,
        ts_millis: u64,
    ) -> Result<LogRecord, LogError> {
        if !is_valid_trace_id(trace_id) {
            return Err(LogError::BadTraceId);
        }
        if !is_valid_event_for_worker(worker, event) {
            return Err(LogError::BadEvent);
        }
        validate_fields(fields.iter())?;
        Ok(LogRecord {
            ts: (ts_millis <= MAX_RFC3339_MILLIS)
                .then(|| unix_millis_to_rfc3339(ts_millis))
                .ok_or(LogError::BadTimestamp)?,
            level,
            worker,
            trace_id: trace_id.to_string(),
            event: event.to_string(),
            fields,
        })
    }

    /// Render the six-key JSON record without a trailing newline.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        let mut map = serde_json::Map::with_capacity(6);
        map.insert("ts".to_string(), Value::String(self.ts.clone()));
        map.insert(
            "level".to_string(),
            Value::String(self.level.as_str().to_string()),
        );
        map.insert(
            "worker".to_string(),
            Value::String(self.worker.as_str().to_string()),
        );
        map.insert("trace_id".to_string(), Value::String(self.trace_id.clone()));
        map.insert("event".to_string(), Value::String(self.event.clone()));
        map.insert(
            "fields".to_string(),
            Value::Object(
                self.fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
        Value::Object(map).to_string()
    }

    /// Request trace carried by this record.
    #[must_use]
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Event name carried by this record.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }
}

const MAX_RFC3339_MILLIS: u64 = 253_402_300_799_999;

fn is_valid_field_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_FIELD_KEY_BYTES
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn validate_field_string(value: &str) -> Result<(), LogError> {
    if value.len() > MAX_FIELD_VALUE_BYTES {
        return Err(LogError::FieldValueTooLong);
    }
    if looks_like_secret_value(value) {
        return Err(LogError::SecretValue);
    }
    if looks_like_email_value(value) {
        return Err(LogError::PiiValue);
    }
    if looks_like_ipv4_value(value) || looks_like_ipv6_value(value) {
        return Err(LogError::IpAddressValue);
    }
    Ok(())
}

fn has_credential_marker(value: &str) -> bool {
    let has_scheme = ["bearer", "token"].iter().any(|scheme| {
        value.match_indices(scheme).any(|(start, _)| {
            let before = value[..start].chars().next_back();
            let after = &value[start + scheme.len()..];
            before.is_none_or(|c| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-'))
                && after.starts_with(char::is_whitespace)
        })
    });
    has_scheme
        || value.split_inclusive(['=', ':']).any(|part| {
            part.strip_suffix(['=', ':']).is_some_and(|label| {
                let label = label.trim_end().trim_end_matches(['"', '\'']);
                CREDENTIAL_LABELS
                    .iter()
                    .any(|suffix| label.ends_with(suffix))
            })
        })
}

fn is_dotted_quad(word: &str) -> bool {
    let mut octets = word.trim_matches('.').split('.');
    (0..4).all(|_| octets.next().is_some_and(|part| part.parse::<u8>().is_ok()))
        && octets.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c";
    /// 2026-10-15T20:00:00.123Z, the fixture instant from the standard.
    const FIXTURE_MILLIS: u64 = 1_792_094_400_123;

    fn fields(value: Value) -> BTreeMap<String, Value> {
        fields_from_json(value).expect("test fields must validate")
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
    fn generated_ids_are_valid_and_unique() {
        let a = generate_trace_id().expect("entropy must be available in tests");
        let b = generate_trace_id().expect("entropy must be available in tests");
        assert!(is_valid_trace_id(&a));
        assert!(is_valid_trace_id(&b));
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_keeps_valid_ids_and_regenerates_the_rest() {
        assert_eq!(resolve_trace_id(Some(TRACE)), TRACE);
        for header in [
            None,
            Some(""),
            Some("too-short"),
            Some("9F3A4C2E1B5D4F6A8C7E9D0B1A2F4E6C"),
            Some("Bearer abc123"),
        ] {
            let resolved = resolve_trace_id(header);
            assert!(is_valid_trace_id(&resolved), "bad resolve of {header:?}");
            assert_ne!(resolved.as_str(), header.unwrap_or(""));
        }
    }

    #[test]
    fn build_sha_normalizes_to_known_or_unknown() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        const SHA_41: &str = "0123456789abcdef0123456789abcdef012345670";
        const SHA_64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(parse_build_sha(Some(SHA)), SHA);
        assert!(is_build_sha_known(SHA));
        assert!(is_build_sha_known(&parse_build_sha(Some(SHA))));
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
            assert_eq!(parse_build_sha(raw), BUILD_SHA_UNKNOWN, "raw: {raw:?}");
            assert!(!is_build_sha_known(&parse_build_sha(raw)));
        }
        assert!(!is_build_sha_known(BUILD_SHA_UNKNOWN));
        // Direct bounds: length alone must not confer identity, whatever the
        // caller passes without going through `parse_build_sha`.
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
        // Both spellings survive field validation, so emitters can always
        // carry the key (known SHA or explicit unknown).
        assert!(fields(serde_json::json!({"build_sha": SHA})).contains_key("build_sha"));
        assert!(
            fields(serde_json::json!({"build_sha": BUILD_SHA_UNKNOWN})).contains_key("build_sha")
        );
    }

    #[test]
    fn timestamps_format_as_utc_rfc3339_with_millis() {
        assert_eq!(unix_millis_to_rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(unix_millis_to_rfc3339(1_000), "1970-01-01T00:00:01.000Z");
        assert_eq!(
            unix_millis_to_rfc3339(951_782_400_000),
            "2000-02-29T00:00:00.000Z"
        );
        assert_eq!(
            unix_millis_to_rfc3339(FIXTURE_MILLIS),
            "2026-10-15T20:00:00.123Z"
        );
    }

    #[test]
    fn event_names_are_worker_namespaced() {
        assert!(is_valid_event_for_worker(Worker::Api, "api.auth.login"));
        assert!(is_valid_event_for_worker(
            Worker::Ingest,
            "ingest.job.queued"
        ));
        assert!(is_valid_event_for_worker(Worker::Mb, "mb.lookup.hit"));
        // Another worker's namespace is rejected, not just dotted-ness.
        assert!(!is_valid_event_for_worker(Worker::Api, "ingest.job.queued"));
        // `api..x` is ugly but satisfies the documented contract (only
        // leading/trailing separators are banned), so it stays accepted.
        // An exact-prefix event (`api`) is invalid — and must return false,
        // never panic on the namespace index.
        for bad in ["", "nodot", "API.AUTH.LOGIN", ".api.x", "api.x_", "api"] {
            assert!(
                !is_valid_event_for_worker(Worker::Api, bad),
                "accepted {bad}"
            );
        }
        assert!(is_valid_event_for_worker(
            Worker::Api,
            &format!("api.{}", "x".repeat(92))
        ));
        assert!(!is_valid_event_for_worker(
            Worker::Api,
            &format!("api.{}", "x".repeat(93))
        ));
        // Stream keeps its legacy vocabulary: no dot required.
        assert!(is_valid_event_for_worker(Worker::Stream, "play_start"));
        assert!(is_valid_event_for_worker(
            Worker::Stream,
            "stream.play.start"
        ));
    }

    #[test]
    fn record_renders_exactly_the_six_schema_keys() {
        let record = LogRecord::new(
            Worker::Api,
            Level::Info,
            TRACE,
            "api.auth.login",
            fields(serde_json::json!({"status_code": 200, "duration_ms": 12})),
            FIXTURE_MILLIS,
        )
        .expect("valid record must build");
        let parsed: serde_json::Value =
            serde_json::from_str(&record.to_json_line()).expect("line must be JSON");
        let object = parsed.as_object().expect("line must be an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["event", "fields", "level", "trace_id", "ts", "worker"]
        );
        assert_eq!(object["ts"], "2026-10-15T20:00:00.123Z");
        assert_eq!(object["level"], "info");
        assert_eq!(object["worker"], "api");
        assert_eq!(object["trace_id"], TRACE);
        assert_eq!(object["event"], "api.auth.login");
        assert_eq!(object["fields"]["status_code"], 200);
        assert_eq!(record.trace_id(), TRACE);
        assert_eq!(record.event(), "api.auth.login");
    }

    #[test]
    fn record_rejects_invalid_trace_and_event() {
        let ok_fields = || fields(serde_json::json!({"job_id": "job_01"}));
        assert_eq!(
            LogRecord::new(
                Worker::Ingest,
                Level::Info,
                "short",
                "ingest.job.queued",
                ok_fields(),
                0
            ),
            Err(LogError::BadTraceId)
        );
        assert_eq!(
            LogRecord::new(
                Worker::Ingest,
                Level::Info,
                TRACE,
                "job.queued",
                ok_fields(),
                0
            ),
            Err(LogError::BadEvent)
        );
    }

    #[test]
    fn record_rejects_banned_keys_secret_values_ips_and_nesting() {
        // Banned keys never reach a record: conversion itself fails.
        for key in [
            "contact_email",
            "client_ip",
            "ip_address",
            "ipv4",
            "ipv6",
            "client_ipv4",
            "session_id",
            "name",
            "display_name",
            "password",
            "grant_token",
            "api_key",
            "user_credentials",
            "passphrase",
            "privatekey",
            "home_address",
            "phone_number",
            "payment_details",
            "payment_method",
            "postal_code",
            "zip_code",
            "street_name",
            "request_body",
            "response_body",
            "complete_headers",
            "raw_query",
            "query_string",
            "nzb_contents",
            "nzb_xml",
        ] {
            assert!(
                fields_from_json(serde_json::json!({key: "safe"})).is_err(),
                "accepted banned key {key}"
            );
        }
        // Over-broad matches stay allowed (`payment_id` is an opaque internal
        // identifier, not payment details).
        for key in [
            "ship_id",
            "filename",
            "namespace",
            "trip",
            "nzb_id",
            "query_ms",
            "body_bytes",
            "header_count",
            "payment_id",
            "gzip_ratio",
        ] {
            assert!(
                fields_from_json(serde_json::json!({key: "safe"})).is_ok(),
                "rejected safe key {key}"
            );
        }
        // Credential-like and IP values are rejected; benign labels pass.
        for value in [
            "Bearer abc123",
            "Token abc123",
            "sk-live-example",
            "sk_live_example",
            "tok_example",
            "password=hunter2",
        ] {
            assert!(
                fields_from_json(serde_json::json!({"detail": value})).is_err(),
                "accepted secret value {value}"
            );
        }
        assert!(fields_from_json(serde_json::json!({"stage": "task-queued"})).is_ok());
        assert!(fields_from_json(serde_json::json!({"host": "192.0.2.1"})).is_err());
        assert!(fields_from_json(serde_json::json!({"host": "256.0.2.1"})).is_ok());
    }

    #[test]
    fn repair_is_outside_the_shared_worker_vocabulary() {
        // The shared schema reserves worker names for emitters that have
        // adopted the logging standard and its golden fixtures.
        assert_eq!(Worker::parse("repair"), None);
    }

    #[test]
    fn record_rejects_email_addresses_ipv6_and_nesting() {
        // Email-shaped values are personal data, not just keys.
        for value in ["a@example.com", "user+tag@mail.example.co"] {
            assert!(
                fields_from_json(serde_json::json!({"contact": value})).is_err(),
                "accepted email value {value}"
            );
        }
        assert!(fields_from_json(serde_json::json!({"contact": "not-an-email"})).is_ok());
        // IPv6 literals are IP addresses too; colon-joined operational refs
        // (`nzb:sha256:…`) and clock fragments still pass.
        for value in [
            "::1",
            "2001:db8::1",
            "2001:0db8:0000:0000:0000:0000:0000:0001",
        ] {
            assert!(
                fields_from_json(serde_json::json!({"host": value})).is_err(),
                "accepted IPv6 value {value}"
            );
        }
        for value in [
            "nzb:sha256:abc123",
            "12:00:00",
            "task-queued",
            "ingest:u1:abc:upload",
        ] {
            assert!(
                fields_from_json(serde_json::json!({"detail": value})).is_ok(),
                "rejected operational value {value}"
            );
        }
        // Nested values and non-objects are rejected.
        assert!(fields_from_json(serde_json::json!({"a": {"b": 1}})).is_err());
        assert!(fields_from_json(serde_json::json!({"a": [1]})).is_err());
        assert!(fields_from_json(serde_json::json!([1, 2])).is_err());
        // A valid mb record still builds through the same path.
        assert!(LogRecord::new(
            Worker::Mb,
            Level::Info,
            TRACE,
            "mb.lookup.hit",
            fields(serde_json::json!({"kind": "artist", "cache": "hit"})),
            0,
        )
        .is_ok());
    }

    #[test]
    fn worker_and_level_names_roundtrip() {
        for (worker, name) in [
            (Worker::Api, "api"),
            (Worker::Ingest, "ingest"),
            (Worker::Mb, "mb"),
            (Worker::Stream, "stream"),
        ] {
            assert_eq!(worker.as_str(), name);
            assert_eq!(Worker::parse(name), Some(worker));
        }
        for (level, name) in [
            (Level::Debug, "debug"),
            (Level::Info, "info"),
            (Level::Warn, "warn"),
            (Level::Error, "error"),
        ] {
            assert_eq!(level.as_str(), name);
            assert_eq!(Level::parse(name), Some(level));
        }
        assert_eq!(Level::parse("INFO"), None);
    }

    #[test]
    fn error_display_never_echoes_values() {
        let err = LogError::SecretValue;
        assert!(!err.to_string().contains("hunter2"));
        assert_eq!(LogError::BannedField.to_string(), "banned fields key");
    }

    #[test]
    fn entropy_fallback_trace_ids_stay_valid_distinct_and_marked() {
        let first = fallback_trace_id();
        let second = fallback_trace_id();
        for id in [&first, &second] {
            assert!(
                id.starts_with("ffffffffffffffff"),
                "fallback id must carry the f-prefix: {id}"
            );
            assert!(is_valid_trace_id(id), "fallback id must stay valid: {id}");
        }
        assert_ne!(first, second, "fallback ids must be distinct across calls");
    }

    #[test]
    fn no_entropy_display_contract() {
        assert_eq!(
            LogError::NoEntropy.to_string(),
            "trace-id randomness unavailable"
        );
    }
}
