//! Pin the operator-facing `ConfigError` / `ConfigType` Display strings.
//!
//! The renderer reports key names only; values must never be echoed, so the
//! text is safe for deploy logs and CI output. These pins fail on any
//! rewording or value-echo regression.

use nntune_config::{Config, ConfigError, ConfigType};
use std::collections::HashMap;

#[test]
fn missing_required_display_is_pinned() {
    assert_eq!(
        ConfigError::MissingRequired("auth.rp_id").to_string(),
        "missing required config key: auth.rp_id",
    );
}

#[test]
fn invalid_type_display_is_pinned() {
    assert_eq!(
        ConfigError::InvalidType {
            key: "session.ttl_secs",
            expected: ConfigType::U64,
        }
        .to_string(),
        "config key session.ttl_secs has the wrong type (expected u64)",
    );
    assert_eq!(
        ConfigError::InvalidType {
            key: "test.bool_flag",
            expected: ConfigType::Bool,
        }
        .to_string(),
        "config key test.bool_flag has the wrong type (expected bool)",
    );
    assert_eq!(
        ConfigError::InvalidType {
            key: "auth.rp_id",
            expected: ConfigType::Str,
        }
        .to_string(),
        "config key auth.rp_id has the wrong type (expected string)",
    );
}

#[test]
fn unknown_key_display_is_pinned() {
    assert_eq!(
        ConfigError::UnknownKey("debug.backdoor".to_string()).to_string(),
        "undeclared config key: debug.backdoor",
    );
}

#[test]
fn config_type_display_is_pinned() {
    assert_eq!(ConfigType::U64.to_string(), "u64");
    assert_eq!(ConfigType::Bool.to_string(), "bool");
    assert_eq!(ConfigType::Str.to_string(), "string");
}

#[test]
fn invalid_type_rendering_never_echoes_the_submitted_value() {
    // Fixture-only secret-looking value; never a real credential.
    let submitted = "s3cr3t-fixture-value-9f8e7d6c5b4a";
    let rendered = ConfigError::InvalidType {
        key: "auth.rp_origin",
        expected: ConfigType::Str,
    }
    .to_string();
    assert_eq!(
        rendered,
        "config key auth.rp_origin has the wrong type (expected string)",
    );
    assert!(
        !rendered.contains(submitted),
        "error rendering must never echo the submitted value",
    );

    // End to end: a secret-looking submission rejected by `load` still
    // renders without the value.
    let mut kv = HashMap::new();
    kv.insert(
        "auth.rp_id".to_string(),
        "v2-staging.nntune.com".to_string(),
    );
    kv.insert(
        "auth.rp_origin".to_string(),
        "https://v2-staging.nntune.com".to_string(),
    );
    kv.insert("session.ttl_secs".to_string(), submitted.to_string());
    let err = Config::load(&kv).expect_err("secret-looking u64 must fail");
    let rendered = err.to_string();
    assert_eq!(
        rendered,
        "config key session.ttl_secs has the wrong type (expected u64)",
    );
    assert!(
        !rendered.contains(submitted),
        "load-path error rendering must never echo the submitted value",
    );
}
