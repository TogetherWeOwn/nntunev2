//! Acceptance tests: per-worker structured logging standard with trace IDs.
//!
//! Slice contract for the `api` worker. The fixture is illustrative; this test
//! validates only the documented logging envelope and obvious privacy hazards.
//!
//! Static fixtures only: no live Cloudflare tail, no network, and no new deps.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const EXPECTED_WORKER: &str = "api";
const FIXTURE_REL: &str = "tests/fixtures/logging_golden.json";
const DOC_REL: &str = "../../docs/logging-standard.md";
const REQUIRED_FIELDS: &[&str] = &["ts", "level", "worker", "trace_id", "event", "fields"];
const ALLOWED_LEVELS: &[&str] = &["debug", "info", "warn", "error"];
const BANNED_KEY_PARTS: &[&str] = &[
    "password", "passwd", "secret", "api_key", "apikey", "token", "cookie",
    "authorization", "private_key", "mnemonic", "ssn", "credit_card", "card_number",
    "cvv", "email", "session",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_artifact(rel: &str, what: &str) -> String {
    let path = manifest_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("RED (expected before the build): missing {what} at {rel} (resolved to {})", path.display())
    })
}

#[derive(Debug, PartialEq)]
enum JsonValue {
    Object(BTreeMap<String, JsonValue>),
    Array,
    String(String),
    Number,
    Bool,
    Null,
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn parse(input: &'a str) -> Result<JsonValue, String> {
        let mut parser = Self { bytes: input.as_bytes(), pos: 0 };
        let value = parser.value()?;
        parser.ws();
        if parser.pos != parser.bytes.len() {
            return Err("trailing data".into());
        }
        Ok(value)
    }

    fn ws(&mut self) {
        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
    }

    fn byte(&mut self, expected: u8) -> Result<(), String> {
        self.ws();
        if self.bytes.get(self.pos) == Some(&expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", expected as char, self.pos))
        }
    }

    fn value(&mut self) -> Result<JsonValue, String> {
        self.ws();
        match self.bytes.get(self.pos).copied() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(JsonValue::String),
            Some(b't') => { self.literal(b"true")?; Ok(JsonValue::Bool) }
            Some(b'f') => { self.literal(b"false")?; Ok(JsonValue::Bool) }
            Some(b'n') => { self.literal(b"null")?; Ok(JsonValue::Null) }
            Some(b'-' | b'0'..=b'9') => { self.number()?; Ok(JsonValue::Number) }
            _ => Err(format!("invalid value at byte {}", self.pos)),
        }
    }

    fn literal(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.pos..self.pos + expected.len()) == Some(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.byte(b'"')?;
        let mut out = String::new();
        loop {
            let byte = *self.bytes.get(self.pos).ok_or("unterminated string")?;
            self.pos += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = *self.bytes.get(self.pos).ok_or("unterminated escape")?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'), b'\\' => out.push('\\'), b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'), b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'), b'r' => out.push('\r'), b't' => out.push('\t'),
                        b'u' => {
                            let code = self.hex_quad()?;
                            let scalar = if (0xD800..=0xDBFF).contains(&code) {
                                if self.bytes.get(self.pos..self.pos + 2) != Some(b"\\u") {
                                    return Err("missing low surrogate".into());
                                }
                                self.pos += 2;
                                let low = self.hex_quad()?;
                                if !(0xDC00..=0xDFFF).contains(&low) { return Err("invalid low surrogate".into()); }
                                0x10000 + ((u32::from(code) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
                            } else { u32::from(code) };
                            out.push(char::from_u32(scalar).ok_or("invalid unicode scalar")?);
                        }
                        _ => return Err("invalid string escape".into()),
                    }
                }
                0..=0x1f => return Err("control byte in string".into()),
                0x20..=0x7f => out.push(char::from(byte)),
                _ => {
                    self.pos -= 1;
                    let tail = std::str::from_utf8(&self.bytes[self.pos..]).map_err(|_| "invalid utf-8")?;
                    let ch = tail.chars().next().ok_or("invalid utf-8")?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn hex_quad(&mut self) -> Result<u16, String> {
        let digits = self.bytes.get(self.pos..self.pos + 4).ok_or("short unicode escape")?;
        let text = std::str::from_utf8(digits).map_err(|_| "invalid unicode escape")?;
        let value = u16::from_str_radix(text, 16).map_err(|_| "invalid unicode escape")?;
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<(), String> {
        let start = self.pos;
        if self.bytes.get(self.pos) == Some(&b'-') { self.pos += 1; }
        match self.bytes.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => { self.pos += 1; while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) { self.pos += 1; } }
            _ => return Err("invalid number".into()),
        }
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let fraction = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) { self.pos += 1; }
            if fraction == self.pos { return Err("empty number fraction".into()); }
        }
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) { self.pos += 1; }
            let exponent = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) { self.pos += 1; }
            if exponent == self.pos { return Err("empty number exponent".into()); }
        }
        if start == self.pos { Err("invalid number".into()) } else { Ok(()) }
    }

    fn object(&mut self) -> Result<JsonValue, String> {
        self.byte(b'{')?;
        let mut object = BTreeMap::new();
        self.ws();
        if self.bytes.get(self.pos) == Some(&b'}') { self.pos += 1; return Ok(JsonValue::Object(object)); }
        loop {
            self.ws();
            let key = self.string()?;
            self.byte(b':')?;
            let value = self.value()?;
            if object.insert(key, value).is_some() { return Err("duplicate object key".into()); }
            self.ws();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'}') => { self.pos += 1; break; }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
            }
        }
        Ok(JsonValue::Object(object))
    }

    fn array(&mut self) -> Result<JsonValue, String> {
        self.byte(b'[')?;
        self.ws();
        if self.bytes.get(self.pos) == Some(&b']') { self.pos += 1; return Ok(JsonValue::Array); }
        loop {
            self.value()?;
            self.ws();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => { self.pos += 1; break; }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
            }
        }
        Ok(JsonValue::Array)
    }
}

fn parse_line(line: &str) -> BTreeMap<String, JsonValue> {
    let JsonValue::Object(object) = JsonParser::parse(line).expect("fixture must be valid JSON") else {
        panic!("golden fixture must be one JSON object");
    };
    object
}

fn timestamp_is_valid_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':' || bytes.last() != Some(&b'Z') { return false; }
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let text = std::str::from_utf8(bytes.get(range)?).ok()?;
        if text.bytes().all(|b| b.is_ascii_digit()) { text.parse().ok() } else { None }
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) =
        (number(0..4), number(5..7), number(8..10), number(11..13), number(14..16), number(17..19)) else { return false; };
    if year == 0 || !(1..=12).contains(&month) || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute) || !(0..=59).contains(&second) { return false; }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month { 2 if leap => 29, 2 => 28, 4 | 6 | 9 | 11 => 30, _ => 31 };
    if day == 0 || day > days { return false; }
    let mut pos = 19;
    if bytes.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) { pos += 1; }
        if pos == start { return false; }
    }
    pos == bytes.len() - 1
}

fn valid_event(value: &str) -> bool {
    !value.is_empty() && value.len() <= 96 && value.contains('.')
        && !value.starts_with(['.', '_']) && !value.ends_with(['.', '_'])
        && value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

fn has_ipv4_like_value(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 4 && parts.iter().all(|part| part.parse::<u8>().is_ok())
}

fn has_obvious_secret_value(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    ["bearer ", "token ", "sk-", "sk_live_", "sk_test_", "tok_", "ghp_", "gho_", "ghu_", "xoxb-", "xoxp-"]
        .iter().any(|marker| lower.starts_with(marker))
        || ["password=", "password:", "secret=", "secret:", "apikey=", "api_key="]
            .iter().any(|marker| lower.contains(marker))
}

fn has_banned_key(fields: &BTreeMap<String, JsonValue>) -> Option<String> {
    fields.keys().find(|key| {
        let lower = key.to_ascii_lowercase();
        let has_ip_component = lower.split(|ch: char| !ch.is_ascii_alphanumeric()).any(|part| part == "ip")
            || ["clientip", "remoteip", "ipaddress"].iter().any(|part| lower.contains(part));
        let has_name_component = lower == "name" || lower.starts_with("name_") || lower.contains("_name")
            || ["username", "firstname", "lastname", "fullname", "displayname", "givenname", "familyname", "nickname", "surname"]
                .iter().any(|part| lower.contains(part));
        has_ip_component || has_name_component || BANNED_KEY_PARTS.iter().any(|part| lower.contains(part))
    }).cloned()
}

fn has_obvious_secret(fields: &BTreeMap<String, JsonValue>) -> bool {
    fields.values().any(|value| match value {
        JsonValue::String(text) => has_obvious_secret_value(text),
        _ => false,
    })
}

fn has_ipv4_value(fields: &BTreeMap<String, JsonValue>) -> bool {
    fields.values().any(|value| matches!(value, JsonValue::String(text) if has_ipv4_like_value(text)))
}

#[test]
fn golden_line_conforms_to_exact_schema() {
    let line = read_artifact(FIXTURE_REL, "golden log-line fixture");
    let object = parse_line(&line);
    let expected: BTreeSet<_> = REQUIRED_FIELDS.iter().copied().collect();
    let actual: BTreeSet<_> = object.keys().map(String::as_str).collect();
    assert_eq!(actual, expected, "fixture must have exactly the six schema keys");
    assert!(matches!(object.get("ts"), Some(JsonValue::String(ts)) if timestamp_is_valid_utc(ts)), "ts must be a valid RFC3339 UTC timestamp");
    assert!(matches!(object.get("level"), Some(JsonValue::String(level)) if ALLOWED_LEVELS.contains(&level.as_str())), "level must be a recognized string");
    assert!(matches!(object.get("worker"), Some(JsonValue::String(worker)) if worker == EXPECTED_WORKER), "worker must match this package");
    assert!(matches!(object.get("trace_id"), Some(JsonValue::String(id)) if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())), "trace_id must be 32 lowercase hex chars");
    assert!(matches!(object.get("event"), Some(JsonValue::String(event)) if valid_event(event) && event.starts_with(&format!("{}.", EXPECTED_WORKER))), "event must be a lowercase api.* name");
    let Some(JsonValue::Object(fields)) = object.get("fields") else { panic!("fields must be a JSON object"); };
    assert!(fields.values().all(|value| matches!(value, JsonValue::String(_) | JsonValue::Number | JsonValue::Bool | JsonValue::Null)), "fields values must be flat JSON primitives");
}

#[test]
fn timestamp_validation_rejects_impossible_dates_and_times() {
    for timestamp in ["2099-13-45T99:99:99Z", "2099-02-29T12:00:00Z", "2099-01-01T24:00:00Z", "2099-01-01T12:60:00Z", "2099-01-01T12:00:60Z"] {
        assert!(!timestamp_is_valid_utc(timestamp), "accepted impossible timestamp {timestamp}");
    }
    assert!(timestamp_is_valid_utc("2000-02-29T23:59:59.123Z"));
    assert!(!timestamp_is_valid_utc("1900-02-29T00:00:00Z"));
}

#[test]
fn golden_line_has_no_prohibited_keys_or_obvious_secret_values() {
    let line = read_artifact(FIXTURE_REL, "golden log-line fixture");
    let object = parse_line(&line);
    let Some(JsonValue::Object(fields)) = object.get("fields") else { panic!("fields must be a JSON object"); };
    assert!(has_banned_key(fields).is_none(), "fields contain a prohibited key fragment");
    assert!(!has_obvious_secret(fields), "fields contain an obvious credential-like value");
    assert!(!has_ipv4_value(fields), "fields contain a dotted-quad IPv4 address");

    for key in ["contact_email", "client_ip", "ip_address", "session_id", "name", "display_name", "nickname", "surname", "password", "grant_token"] {
        let bad = BTreeMap::from([(key.to_string(), JsonValue::String("safe".into()))]);
        assert!(has_banned_key(&bad).is_some(), "did not reject prohibited key {key}");
    }
    for key in ["ship_id", "zip_code", "filename", "namespace", "trip"] {
        let acceptable = BTreeMap::from([(key.to_string(), JsonValue::String("safe".into()))]);
        assert!(has_banned_key(&acceptable).is_none(), "over-broad IP key match rejected {key}");
    }
    for value in ["Bearer abc123", "Token abc123", "sk-live-example", "sk_live_example", "tok_example"] {
        let bad = BTreeMap::from([("value".to_string(), JsonValue::String(value.into()))]);
        assert!(has_obvious_secret(&bad), "did not reject obvious secret value {value}");
    }
    assert!(!has_obvious_secret_value("task-queued"), "flagged a benign task label as a credential");
    let ip_value = BTreeMap::from([("host".to_string(), JsonValue::String("192.0.2.1".into()))]);
    assert!(has_ipv4_value(&ip_value), "did not detect a dotted-quad IPv4 value");
    assert!(!has_ipv4_like_value("256.0.2.1"), "accepted an invalid IPv4 octet");
}

#[test]
fn logging_standard_doc_pins_schema() {
    let doc = read_artifact(DOC_REL, "logging-standard doc");
    for field in REQUIRED_FIELDS { assert!(doc.contains(field), "logging-standard.md must pin `{field}`"); }
    let lower = doc.to_lowercase();
    assert!(lower.contains("x-trace-id") || (lower.contains("trace") && lower.contains("propagat")), "doc must state trace-id propagation");
    assert!(lower.contains("redact") || lower.contains("pii") || lower.contains("secret"), "doc must state its redaction rule");
}
