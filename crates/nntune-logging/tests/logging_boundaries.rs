use std::collections::BTreeMap;

use nntune_logging::{fields_from_json, Level, LogError, LogRecord, Worker, REDACTED};
use serde_json::{json, Value};

const TRACE: &str = "0123456789abcdef0123456789abcdef";
const EVENT: &str = "api.request.served";

fn record(
    event: &str,
    fields: BTreeMap<String, Value>,
    millis: u64,
) -> Result<LogRecord, LogError> {
    LogRecord::new(Worker::Api, Level::Info, TRACE, event, fields, millis)
}

fn assert_rejected(fields: Value) {
    let direct = fields.as_object().unwrap().clone().into_iter().collect();
    assert!(fields_from_json(fields).is_err(), "JSON accepted fields");
    assert!(record(EVENT, direct, 0).is_err(), "record accepted fields");
}

#[test]
fn both_field_paths_reject_sensitive_keys_and_values() {
    for key in [
        "api-key",
        "requestBody",
        "artist.name",
        "raw.query",
        "ghp_fixture",
        "192.0.2.1",
    ] {
        assert_rejected(json!({key: "fixture"}));
    }
    for value in [
        "upstream returned BEARER\tfixture",
        "upstream returned ghp_fixture",
        "api_key : fixture",
        r#"{"password" : "fixture"}"#,
        "one@example.test and two@example.test",
        "https://192.0.2.1:443/path",
        "https://[2001:db8::1]:443/path",
        "::ffff:192.0.2.1",
        "fe80::1%eth0",
    ] {
        assert_rejected(json!({"detail": value}));
    }
}

#[test]
fn rejected_input_is_absent_from_error_display_and_debug() {
    const INPUT: &str = "password=fixture-canary\nnext-line";
    let invalid_event = record(INPUT, BTreeMap::new(), 0).unwrap_err();
    let malformed_key = fields_from_json(json!({INPUT: "fixture"})).unwrap_err();
    let banned_key = fields_from_json(json!({"secret-fixture-canary": "fixture"})).unwrap_err();
    let nested = fields_from_json(json!({"fixture-canary": [1]})).unwrap_err();
    for err in [invalid_event, malformed_key, banned_key, nested] {
        for message in [err.to_string(), format!("{err:?}")] {
            assert!(!message.contains("fixture-canary"));
            assert!(!message.contains('\n'));
        }
    }
}

#[test]
fn both_field_paths_enforce_byte_and_count_limits() {
    let at_limit: BTreeMap<_, _> = (0..32)
        .map(|i| (format!("item_{i:059}"), json!("é".repeat(128))))
        .collect();
    let json_fields = serde_json::to_value(&at_limit).unwrap();
    assert_eq!(fields_from_json(json_fields).unwrap(), at_limit);
    assert!(record(EVENT, at_limit.clone(), 0).is_ok());

    let mut over = at_limit;
    over.insert("extra".into(), json!(null));
    assert_rejected(serde_json::to_value(over).unwrap());
    let long_key = "k".repeat(65);
    assert_rejected(json!({long_key: true}));
    assert_rejected(json!({"detail": format!("{}a", "é".repeat(128))}));
    for key in ["", "bad key", "key\nline", "clé"] {
        assert_rejected(json!({key: null}));
    }
}

#[test]
fn timestamps_with_years_outside_rfc3339_are_rejected() {
    const LAST_MILLIS: u64 = 253_402_300_799_999;
    let fields = json!({"note": REDACTED, "detail": "ready\r\nnext \"step\" \\"});
    let map = fields_from_json(fields.clone()).unwrap();
    let line = record(EVENT, map, LAST_MILLIS).unwrap().to_json_line();
    assert!(!line.contains(['\r', '\n']));
    let parsed: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed.as_object().unwrap().len(), 6);
    assert_eq!(parsed["fields"], fields);
    assert_eq!(parsed["ts"], "9999-12-31T23:59:59.999Z");
    for millis in [LAST_MILLIS + 1, u64::MAX] {
        assert!(record(EVENT, BTreeMap::new(), millis).is_err());
    }
}
