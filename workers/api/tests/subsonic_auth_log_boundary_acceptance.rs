//! Credential confidentiality at the ACTUAL shared structured-log boundary.
//! Synthetic inputs only, not captures of Worker-emitted or live logs. These
//! tests fail if a generic field/diagnostic launders Subsonic credential data.

use nntune_api::logging::{fields_from_json, Level, LogRecord, Worker};
use serde_json::{json, Map, Value};

const KEY: &str = "synthetic-subsonic-log-canary-only";
const HMAC: &str = "524f316146ebc41db98bf8e3881dd7038659624f1061d24b30d8ab1a1d576a05";
const PEPPER: &str = "synthetic-subsonic-index-pepper-v1!";
const USER: &str = "synthetic-subsonic-user-canary";
const PASSWORD: &str = "synthetic-subsonic-password-canary";
const TOKEN: &str = "synthetic-subsonic-token-canary";
const SALT: &str = "synthetic-subsonic-salt-canary";

fn assert_private(diagnostic: &str) {
    for value in [KEY, HMAC, PEPPER, USER, PASSWORD, TOKEN, SALT] {
        assert!(
            !diagnostic.contains(value),
            "log rejection disclosed credential material"
        );
    }
}

fn assert_refused(input: Value) {
    let error =
        fields_from_json(input).expect_err("credential fields/diagnostics must not be logged");
    for diagnostic in [
        format!("{error}"),
        format!("{error:?}"),
        format!("{error:#?}"),
        format!("{:#?}", Some(&error)),
    ] {
        assert_private(&diagnostic);
    }
}

#[test]
fn credential_fields_and_raw_queries_never_become_structured_log_fields() {
    for (name, value) in [
        ("u", USER),
        ("p", PASSWORD),
        ("t", TOKEN),
        ("s", SALT),
        ("apiKey", KEY),
        ("api_key", KEY),
        ("api_key_hmac", HMAC),
        ("subsonic_credentials", KEY),
        ("SUBSONIC_INDEX_PEPPER", PEPPER),
        ("raw_query", KEY),
        ("query_string", KEY),
    ] {
        let mut fields = Map::new();
        fields.insert(name.into(), Value::String(value.into()));
        for input in [
            Value::Object(fields.clone()),
            json!({"detail": fields}),
            json!({"detail": [{"cause": fields}]}),
        ] {
            assert_refused(input);
        }
    }
}

#[test]
fn a_query_embedded_in_a_generic_diagnostic_is_refused_without_echo() {
    for diagnostic in [
        format!("GET /rest/ping?apiKey={KEY}"),
        format!("lookup failed: api_key={KEY}"),
        format!("lookup failed: api_key_hmac={HMAC}"),
        format!("configuration failed: SUBSONIC_INDEX_PEPPER={PEPPER}"),
        format!("GET /rest/getPlaylists?u={USER}&p={PASSWORD}&t={TOKEN}&s={SALT}"),
        format!(r#"upstream diagnostic {{"apiKey":"{KEY}","p":"{PASSWORD}"}}"#),
    ] {
        assert_refused(json!({"detail": diagnostic}));
    }
}

#[test]
fn nested_keyrings_and_credential_bearing_error_causes_are_not_loggable() {
    for input in [
        json!({"detail": {"current_version":1, "keys":{"1":PEPPER}}}),
        json!({"cause": {"error": {"api_key_hmac": HMAC, "apiKey": KEY}}}),
        json!({"diagnostics": [{"u":USER,"p":PASSWORD,"t":TOKEN,"s":SALT}]}),
    ] {
        assert_refused(input);
    }
}

#[test]
fn credential_free_operational_fault_classes_can_reach_the_existing_log_emitter() {
    // This exercises the actual emitter with safe fields, not live Worker
    // alert delivery. Typed auth faults are asserted by the persisted bank.
    for fault in ["keyring", "configuration", "store", "limiter"] {
        let fields = fields_from_json(json!({"component":"subsonic_auth", "fault":fault})).unwrap();
        let record = LogRecord::new(
            Worker::Api,
            Level::Error,
            "9f3a4c2e1b5d4f6a8c7e9d0b1a2f4e6c",
            "api.subsonic.auth.failed",
            fields,
            1_800_000_000_000,
        );
        let record = record.expect("credential-free operational event must validate");
        assert_private(&format!("{record:?}\n{record:#?}"));
        let serialized = record.to_json_line();
        assert_private(&serialized);
        assert!(serialized.contains(fault));
    }
}
