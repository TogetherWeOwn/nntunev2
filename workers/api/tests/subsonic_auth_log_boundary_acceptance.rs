//! Subsonic credential confidentiality at the actual structured-log boundary.
//! Synthetic inputs only; this is not a capture of Worker-emitted/live logs.

use nntune_api::logging::fields_from_json;
use serde_json::{json, Map, Value};

const KEY: &str = "synthetic-subsonic-log-canary-only";

#[test]
fn credential_fields_and_raw_queries_never_become_structured_log_fields() {
    for name in [
        "apiKey",
        "api_key_hmac",
        "subsonic_credentials",
        "raw_query",
        "query_string",
    ] {
        let mut fields = Map::new();
        fields.insert(name.into(), Value::String(KEY.into()));
        for input in [Value::Object(fields.clone()), json!({"detail": fields})] {
            let error = fields_from_json(input).expect_err("credential fields must not be logged");
            for diagnostic in [
                format!("{error}"),
                format!("{error:?}"),
                format!("{error:#?}"),
            ] {
                assert!(
                    !diagnostic.contains(KEY),
                    "log rejection disclosed a credential"
                );
            }
        }
    }
}

#[test]
fn a_query_embedded_in_a_generic_diagnostic_is_also_refused_without_echo() {
    for diagnostic in [
        format!("GET /rest/ping?apiKey={KEY}"),
        format!("lookup failed: api_key={KEY}"),
    ] {
        let error = fields_from_json(json!({"detail": diagnostic}))
            .expect_err("a generic field name must not launder a raw credential query");
        assert!(!format!("{error}").contains(KEY));
        assert!(!format!("{error:?}").contains(KEY));
    }
}
