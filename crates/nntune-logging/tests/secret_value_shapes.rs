//! Regression pin: secret-value normalization shapes for `nntune-logging`.
//!
//! `looks_like_secret_value` normalizes with `trim_start().to_ascii_lowercase()`
//! before marker match (`src/lib.rs:350-358`), so padded (`"  Bearer abc"`)
//! and case-variant (`"PASSWORD=hunter2"`) shapes must reject. Fixture strings
//! only; no real credentials.

use nntune_logging::{fields_from_json, looks_like_secret_value};

/// Values that must reject: leading padding and case variants still match.
const REJECT: &[&str] = &[
    "  Bearer abc123",
    "\tToken x",
    "PASSWORD=hunter2",
    "Secret: hunter2",
    "API_KEY=xyz",
    "ghp_example",
    "xoxb-example",
];

/// Values that must accept: benign labels, including ones that contain a
/// credential word mid-string. Prefixes anchor at string start, not substring.
const ACCEPT: &[&str] = &["task-queued", "bearer-token-count"];

#[test]
fn padded_and_case_variant_shapes_reject() {
    for value in REJECT {
        assert!(
            looks_like_secret_value(value),
            "accepted secret-shaped value {value:?}"
        );
        assert!(
            fields_from_json(serde_json::json!({"detail": value})).is_err(),
            "accepted secret-shaped value {value:?}"
        );
    }
}

#[test]
fn benign_labels_still_accept() {
    for value in ACCEPT {
        assert!(
            !looks_like_secret_value(value),
            "rejected benign value {value:?}"
        );
        assert!(
            fields_from_json(serde_json::json!({"detail": value})).is_ok(),
            "rejected benign value {value:?}"
        );
    }
}
