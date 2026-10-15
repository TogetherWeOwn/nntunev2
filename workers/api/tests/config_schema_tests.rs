//! Acceptance tests for the typed KV config accessor.
//!
//! Written first (TDD RED): these fail until `nntune_api::config` exists.
//! Every test asserts real behavior against the versioned schema fixture;
//! no live KV namespace is touched, and no secret material appears here
//! (secret values must never enter the KV schema — see the schema test
//! below). These test files are owned by the acceptance-test card: the
//! paired build card implements against them and never edits them.
//!
//! Contract pinned for the Builder (additive extensions allowed, the
//! signatures below must hold):
//! - `nntune_api::config::ConfigSchema::from_json(&str)`
//!   parses `tests/fixtures/config-schema.json`
//!   (`version: u64`, `keys` each with `name`, `kind` (`"string"`,
//!   `"bool"` or `"u64"`), optional `default`, `required`, `env_overridable`).
//! - `ConfigSchema::version(&self) -> u64` returns the schema version.
//! - `ConfigSchema::key_names(&self) -> Vec<String>` lists every declared key.
//! - `nntune_api::config::TypedConfig::from_map(&schema, &map)`
//!   validates eagerly: absent keys fall back to their schema default,
//!   a missing required key with no default is an error, and a present
//!   value that does not parse as its declared kind is an error.
//! - `TypedConfig::get_string(&self, key) -> Result<String, ConfigError>`,
//!   `get_bool(&self, key) -> Result<bool, ConfigError>`,
//!   `get_u64(&self, key) -> Result<u64, ConfigError>`.
//! - `nntune_api::config::ConfigError` implements `Debug` + `Display`,
//!   and its `Display` names the offending key.

use nntune_api::config::{ConfigSchema, TypedConfig};
use std::collections::HashMap;

fn schema() -> ConfigSchema {
    let json = include_str!("fixtures/config-schema.json");
    ConfigSchema::from_json(json).expect("schema fixture parses")
}

fn kv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn schema_fixture_is_versioned_and_declares_every_key() {
    let schema = schema();
    assert_eq!(schema.version(), 1, "schema carries a version");
    let names = schema.key_names();
    for expected in [
        "PUBLIC_BASE_URL",
        "STREAM_CACHE_TTL_SECS",
        "READ_ONLY_MODE",
        "MAX_CONNECTIONS",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "schema declares {expected}; got {names:?}"
        );
    }
}

#[test]
fn schema_declares_no_secret_keys() {
    // Secret values (session secrets, KEKs, provider credentials, API keys)
    // must never enter the KV config schema: they live in Worker secrets,
    // never in versioned config. Any key that looks like a secret fails.
    for name in schema().key_names() {
        let upper = name.to_uppercase();
        for banned in ["SECRET", "KEK", "PASSWORD", "PRIVATE_KEY", "API_KEY"] {
            assert!(
                !upper.contains(banned),
                "schema key {name} looks like a secret ({banned}); secrets stay out of KV config"
            );
        }
    }
}

#[test]
fn defaults_applied_when_kv_omits_optional_keys() {
    let schema = schema();
    // Only the required key is set; every optional key falls back to default.
    let cfg = TypedConfig::from_map(&schema, &kv(&[("PUBLIC_BASE_URL", "https://example.com")]))
        .expect("defaults fill optional keys");
    assert_eq!(cfg.get_u64("STREAM_CACHE_TTL_SECS").expect("default"), 300);
    assert!(!cfg.get_bool("READ_ONLY_MODE").expect("default"));
    assert_eq!(cfg.get_u64("MAX_CONNECTIONS").expect("default"), 4);
    assert_eq!(
        cfg.get_string("PUBLIC_BASE_URL").expect("set"),
        "https://example.com"
    );
}

#[test]
fn overrides_win_over_defaults() {
    let schema = schema();
    let cfg = TypedConfig::from_map(
        &schema,
        &kv(&[
            ("PUBLIC_BASE_URL", "https://shop.example.com"),
            ("STREAM_CACHE_TTL_SECS", "600"),
            ("READ_ONLY_MODE", "true"),
            ("MAX_CONNECTIONS", "8"),
        ]),
    )
    .expect("valid overrides parse");
    assert_eq!(
        cfg.get_string("PUBLIC_BASE_URL").expect("override"),
        "https://shop.example.com"
    );
    assert_eq!(cfg.get_u64("STREAM_CACHE_TTL_SECS").expect("override"), 600);
    assert!(cfg.get_bool("READ_ONLY_MODE").expect("override"));
    assert_eq!(cfg.get_u64("MAX_CONNECTIONS").expect("override"), 8);
}

#[test]
fn invalid_u64_value_rejected() {
    let schema = schema();
    let err = TypedConfig::from_map(
        &schema,
        &kv(&[
            ("PUBLIC_BASE_URL", "https://example.com"),
            ("STREAM_CACHE_TTL_SECS", "not-a-number"),
        ]),
    )
    .expect_err("non-numeric u64 must be rejected");
    assert!(
        err.to_string().contains("STREAM_CACHE_TTL_SECS"),
        "error names the offending key: {err}"
    );
}

#[test]
fn invalid_bool_value_rejected() {
    let schema = schema();
    let err = TypedConfig::from_map(
        &schema,
        &kv(&[
            ("PUBLIC_BASE_URL", "https://example.com"),
            ("READ_ONLY_MODE", "maybe"),
        ]),
    )
    .expect_err("non-boolean bool must be rejected");
    assert!(
        err.to_string().contains("READ_ONLY_MODE"),
        "error names the offending key: {err}"
    );
}

#[test]
fn missing_required_key_rejected() {
    let schema = schema();
    // PUBLIC_BASE_URL is required with no default: omitting it is an error.
    let err = TypedConfig::from_map(&schema, &kv(&[("READ_ONLY_MODE", "false")]))
        .expect_err("missing required key must be rejected");
    assert!(
        err.to_string().contains("PUBLIC_BASE_URL"),
        "error names the missing key: {err}"
    );
}
