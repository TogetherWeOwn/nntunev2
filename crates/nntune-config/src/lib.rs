//! Typed accessor for the versioned KV config schema.
//!
//! The schema source of truth is `config/schema.json`: every key declares
//! its type, default, and env-overridability. Workers must read config keys
//! through [`Config`], never with raw stringly KV lookups on config paths:
//! [`Config::load`] validates the whole key set at once (defaults applied,
//! missing required keys rejected, type mismatches rejected, undeclared keys
//! rejected), and [`Config::apply_env`] layers the documented per-key
//! environment overrides on top for local dev and `wrangler [vars]`.
//!
//! Secret values are never config keys: Worker secrets and rotation live
//! outside KV config entirely.
//!
//! Native + wasm32 compatible.

use std::collections::HashMap;
use std::fmt;

/// Version of `config/schema.json` this accessor implements. Bump together
/// with the schema file; the cross-check test fails on drift.
pub const SCHEMA_VERSION: u32 = 1;

/// Declared type of one config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigType {
    U64,
    Bool,
    Str,
}

impl fmt::Display for ConfigType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigType::U64 => write!(f, "u64"),
            ConfigType::Bool => write!(f, "bool"),
            ConfigType::Str => write!(f, "string"),
        }
    }
}

/// Static spec for one config key. `default` is the raw KV text form;
/// `None` means the key has no default and must be set per environment when
/// `required`. `env_var` names the override variable and is `Some` exactly
/// when `env_overridable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub name: &'static str,
    pub ty: ConfigType,
    pub default: Option<&'static str>,
    pub required: bool,
    pub env_overridable: bool,
    pub env_var: Option<&'static str>,
}

impl KeySpec {
    /// Parse one raw KV text value into the declared type. Numbers and bools
    /// tolerate surrounding whitespace; strings are kept exact (a hostname
    /// with a stray space must fail where it is used, not be silently fixed
    /// here) but empty/whitespace-only strings are rejected, matching the CI
    /// parity checker (`config_parity_check.py`), which rejects empty
    /// required strings. Bools accept only `true`/`false`
    /// (case-insensitive).
    fn parse(&self, raw: &str) -> Result<Value, ConfigError> {
        let invalid = || ConfigError::InvalidType {
            key: self.name,
            expected: self.ty,
        };
        match self.ty {
            ConfigType::U64 => {
                let trimmed = raw.trim();
                trimmed.parse().map(Value::U64).map_err(|_| invalid())
            }
            ConfigType::Bool => match raw.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(invalid()),
            },
            ConfigType::Str => {
                if raw.trim().is_empty() {
                    return Err(invalid());
                }
                Ok(Value::Str(raw.to_string()))
            }
        }
    }
}

/// The full key table, mirroring `config/schema.json`. Adding a key means
/// updating the schema file, both env fixtures, and this table together
/// (see `config/README.md`); the schema cross-check test enforces it.
pub static KEYS: &[KeySpec] = &[
    KeySpec {
        name: "auth.challenge_ttl_secs",
        ty: ConfigType::U64,
        default: Some("300"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "auth.invite_redemption_max_attempts",
        ty: ConfigType::U64,
        default: Some("10"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "auth.invite_redemption_window_secs",
        ty: ConfigType::U64,
        default: Some("300"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "auth.magic_link_ttl_secs",
        ty: ConfigType::U64,
        default: Some("900"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "auth.rp_id",
        ty: ConfigType::Str,
        default: None,
        required: true,
        env_overridable: true,
        env_var: Some("RP_ID"),
    },
    KeySpec {
        name: "auth.rp_origin",
        ty: ConfigType::Str,
        default: None,
        required: true,
        env_overridable: true,
        env_var: Some("RP_ORIGIN"),
    },
    KeySpec {
        name: "ingest.max_nzb_bytes",
        ty: ConfigType::U64,
        default: Some("2097152"),
        required: true,
        env_overridable: true,
        env_var: Some("INGEST_MAX_NZB_BYTES"),
    },
    KeySpec {
        name: "ingest.payload_retry_attempts",
        ty: ConfigType::U64,
        default: Some("4"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "ingest.payload_ttl_secs",
        ty: ConfigType::U64,
        default: Some("2592000"),
        required: true,
        env_overridable: true,
        env_var: Some("INGEST_PAYLOAD_TTL_SECS"),
    },
    KeySpec {
        name: "mb.lookup_cache_ttl_s",
        ty: ConfigType::U64,
        default: Some("86400"),
        required: true,
        env_overridable: true,
        env_var: Some("MB_LOOKUP_CACHE_TTL_S"),
    },
    KeySpec {
        name: "mb.max_query_chars",
        ty: ConfigType::U64,
        default: Some("100"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "mb.search_cache_ttl_s",
        ty: ConfigType::U64,
        default: Some("300"),
        required: true,
        env_overridable: true,
        env_var: Some("MB_SEARCH_CACHE_TTL_S"),
    },
    KeySpec {
        name: "mb.search_limit",
        ty: ConfigType::U64,
        default: Some("25"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "session.ttl_secs",
        ty: ConfigType::U64,
        default: Some("86400"),
        required: true,
        env_overridable: true,
        env_var: Some("SESSION_TTL_SECS"),
    },
    KeySpec {
        name: "stream.r2_min_part_bytes",
        ty: ConfigType::U64,
        default: Some("5242880"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
    KeySpec {
        name: "vault.indexer_allow_redirects",
        ty: ConfigType::Bool,
        default: Some("false"),
        required: true,
        env_overridable: false,
        env_var: None,
    },
];

fn spec_for(name: &str) -> Option<&'static KeySpec> {
    KEYS.iter().find(|spec| spec.name == name)
}

/// Typed config value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    U64(u64),
    Bool(bool),
    Str(String),
}

/// Fail-closed load/overlay errors. Key names are reported; values never
/// are, so error text is safe for CI logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingRequired(&'static str),
    InvalidType {
        key: &'static str,
        expected: ConfigType,
    },
    UnknownKey(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingRequired(key) => write!(f, "missing required config key: {key}"),
            ConfigError::InvalidType { key, expected } => {
                write!(
                    f,
                    "config key {key} has the wrong type (expected {expected})"
                )
            }
            ConfigError::UnknownKey(key) => write!(f, "undeclared config key: {key}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Validated, typed view over one environment's KV config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    values: HashMap<String, Value>,
}

impl Config {
    /// Load and validate a full KV key set at once: undeclared keys fail,
    /// defaults fill unset keys, required keys without a default must be
    /// present, and every value must parse as its declared type.
    pub fn load(kv: &HashMap<String, String>) -> Result<Self, ConfigError> {
        for name in kv.keys() {
            if spec_for(name).is_none() {
                return Err(ConfigError::UnknownKey(name.clone()));
            }
        }
        let mut values = HashMap::with_capacity(KEYS.len());
        for spec in KEYS {
            match kv.get(spec.name) {
                Some(raw) => {
                    values.insert(spec.name.to_owned(), spec.parse(raw)?);
                }
                None => match spec.default {
                    Some(default) => {
                        values.insert(spec.name.to_owned(), spec.parse(default)?);
                    }
                    None if spec.required => return Err(ConfigError::MissingRequired(spec.name)),
                    None => {}
                },
            }
        }
        Ok(Config { values })
    }

    /// Layer per-key environment overrides on top of KV. Only keys declared
    /// `env_overridable` are consulted, and only through their documented
    /// `env_var` name; every other entry in `env` is ignored. Precedence is
    /// KV first, environment second (local dev and `wrangler [vars]` win).
    /// The overlay is atomic: every override is parsed first and the config
    /// is only mutated when all of them are valid, so a failing overlay
    /// leaves the previous values untouched (fail-closed).
    pub fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        let mut staged = Vec::new();
        for spec in KEYS.iter().filter(|spec| spec.env_overridable) {
            if let Some(var) = spec.env_var {
                if let Some(raw) = env.get(var) {
                    staged.push((spec.name.to_owned(), spec.parse(raw)?));
                }
            }
        }
        for (name, value) in staged {
            self.values.insert(name, value);
        }
        Ok(())
    }

    /// Typed getters. `None` means the key is absent (only possible for
    /// non-required keys without a default) or held under another type,
    /// which cannot happen after [`Config::load`] validated the set.
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        match self.values.get(key) {
            Some(Value::U64(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some(Value::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.values.get(key) {
            Some(Value::Str(value)) => Some(value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// Minimal valid KV: the two identity keys, which have no defaults.
    fn identity() -> HashMap<String, String> {
        kv(&[
            ("auth.rp_id", "v2-staging.nntune.com"),
            ("auth.rp_origin", "https://v2-staging.nntune.com"),
        ])
    }

    #[test]
    fn defaults_apply_for_unset_keys() {
        let config = Config::load(&identity()).expect("identity loads");
        assert_eq!(config.get_u64("session.ttl_secs"), Some(86400));
        assert_eq!(config.get_u64("auth.magic_link_ttl_secs"), Some(900));
        assert_eq!(config.get_u64("auth.challenge_ttl_secs"), Some(300));
        assert_eq!(config.get_u64("mb.lookup_cache_ttl_s"), Some(86400));
        assert_eq!(config.get_u64("mb.search_cache_ttl_s"), Some(300));
        assert_eq!(config.get_u64("mb.search_limit"), Some(25));
        assert_eq!(config.get_u64("ingest.payload_ttl_secs"), Some(2592000));
        assert_eq!(
            config.get_bool("vault.indexer_allow_redirects"),
            Some(false)
        );
        assert_eq!(config.get_str("auth.rp_id"), Some("v2-staging.nntune.com"));
    }

    #[test]
    fn explicit_kv_values_win_over_defaults() {
        let mut raw = identity();
        raw.insert("session.ttl_secs".to_string(), "3600".to_string());
        raw.insert(
            "vault.indexer_allow_redirects".to_string(),
            "true".to_string(),
        );
        let config = Config::load(&raw).expect("explicit values load");
        assert_eq!(config.get_u64("session.ttl_secs"), Some(3600));
        assert_eq!(config.get_bool("vault.indexer_allow_redirects"), Some(true));
    }

    #[test]
    fn missing_required_key_without_default_fails() {
        let mut raw = identity();
        raw.remove("auth.rp_id");
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::MissingRequired("auth.rp_id"))
        );
        assert_eq!(
            Config::load(&HashMap::new()),
            Err(ConfigError::MissingRequired("auth.rp_id"))
        );
    }

    #[test]
    fn invalid_types_rejected() {
        // u64 keys reject non-numeric, fractional, negative, and empty text.
        for bad in ["not-a-number", "86.4", "-1", ""] {
            let mut raw = identity();
            raw.insert("session.ttl_secs".to_string(), bad.to_string());
            assert_eq!(
                Config::load(&raw),
                Err(ConfigError::InvalidType {
                    key: "session.ttl_secs",
                    expected: ConfigType::U64,
                }),
                "bad value {bad:?} must fail"
            );
        }
        // Bool keys reject anything outside true/false.
        let mut raw = identity();
        raw.insert(
            "vault.indexer_allow_redirects".to_string(),
            "perhaps".to_string(),
        );
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::InvalidType {
                key: "vault.indexer_allow_redirects",
                expected: ConfigType::Bool,
            }),
        );
    }

    #[test]
    fn bool_parsing_is_strict_but_case_insensitive() {
        for good in ["true", "TRUE", " True "] {
            let mut raw = identity();
            raw.insert(
                "vault.indexer_allow_redirects".to_string(),
                good.to_string(),
            );
            let config = Config::load(&raw).expect("bool spelling loads");
            assert_eq!(config.get_bool("vault.indexer_allow_redirects"), Some(true));
        }
        for bad in ["yes", "1", "on", ""] {
            let mut raw = identity();
            raw.insert("vault.indexer_allow_redirects".to_string(), bad.to_string());
            assert!(
                Config::load(&raw).is_err(),
                "bool spelling {bad:?} must fail"
            );
        }
    }

    #[test]
    fn unknown_kv_key_rejected() {
        let mut raw = identity();
        raw.insert("debug.backdoor".to_string(), "true".to_string());
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::UnknownKey("debug.backdoor".to_string()))
        );
    }

    #[test]
    fn env_overlay_only_touches_overridable_keys() {
        let mut config = Config::load(&identity()).expect("identity loads");
        let env = kv(&[
            ("SESSION_TTL_SECS", "3600"),
            ("MB_SEARCH_LIMIT", "50"),
            ("SOME_UNRELATED_VAR", "ignored"),
        ]);
        config.apply_env(&env).expect("overlay applies");
        // Declared overridable: env wins over KV/default.
        assert_eq!(config.get_u64("session.ttl_secs"), Some(3600));
        // Not overridable: the env entry is ignored, default stands.
        assert_eq!(config.get_u64("mb.search_limit"), Some(25));
    }

    #[test]
    fn env_overlay_type_errors_fail_closed() {
        let mut config = Config::load(&identity()).expect("identity loads");
        let env = kv(&[("SESSION_TTL_SECS", "soon")]);
        assert_eq!(
            config.apply_env(&env),
            Err(ConfigError::InvalidType {
                key: "session.ttl_secs",
                expected: ConfigType::U64,
            }),
        );
        // The failed overlay leaves the previous value in place.
        assert_eq!(config.get_u64("session.ttl_secs"), Some(86400));
    }

    #[test]
    fn env_overlay_is_atomic_across_vars() {
        // One good override plus one bad override: the whole overlay fails
        // and NONE of the values change (no partial application).
        let mut config = Config::load(&identity()).expect("identity loads");
        let env = kv(&[("RP_ID", "evil"), ("SESSION_TTL_SECS", "soon")]);
        assert_eq!(
            config.apply_env(&env),
            Err(ConfigError::InvalidType {
                key: "session.ttl_secs",
                expected: ConfigType::U64,
            }),
        );
        assert_eq!(
            config.get_str("auth.rp_id"),
            Some("v2-staging.nntune.com"),
            "rp_id must not be mutated by a failed overlay"
        );
        assert_eq!(config.get_u64("session.ttl_secs"), Some(86400));
    }

    #[test]
    fn empty_and_blank_strings_rejected() {
        // The CI parity checker rejects empty required strings; the typed
        // accessor must agree so a blank KV value cannot load at runtime.
        for bad in ["", "   ", "\t\n "] {
            let mut raw = identity();
            raw.insert("auth.rp_id".to_string(), bad.to_string());
            assert_eq!(
                Config::load(&raw),
                Err(ConfigError::InvalidType {
                    key: "auth.rp_id",
                    expected: ConfigType::Str,
                }),
                "blank rp_id {bad:?} must fail"
            );
            let mut raw = identity();
            raw.insert("auth.rp_origin".to_string(), bad.to_string());
            assert_eq!(
                Config::load(&raw),
                Err(ConfigError::InvalidType {
                    key: "auth.rp_origin",
                    expected: ConfigType::Str,
                }),
                "blank rp_origin {bad:?} must fail"
            );
        }
        // Blank env overrides fail closed too, without mutating anything.
        let mut config = Config::load(&identity()).expect("identity loads");
        let env = kv(&[("RP_ID", "   ")]);
        assert_eq!(
            config.apply_env(&env),
            Err(ConfigError::InvalidType {
                key: "auth.rp_id",
                expected: ConfigType::Str,
            }),
        );
        assert_eq!(config.get_str("auth.rp_id"), Some("v2-staging.nntune.com"));
    }

    #[test]
    fn typed_getters_return_none_for_absent_or_other_typed_keys() {
        let config = Config::load(&identity()).expect("identity loads");
        assert_eq!(config.get_u64("auth.rp_id"), None);
        assert_eq!(config.get_str("session.ttl_secs"), None);
        assert_eq!(config.get_bool("no.such.key"), None);
    }

    #[test]
    fn schema_file_matches_code_table() {
        let path = format!("{}/../../config/schema.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).expect("schema.json readable");
        let schema: serde_json::Value = serde_json::from_str(&text).expect("schema.json parses");
        assert_eq!(
            schema
                .get("schema_version")
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(SCHEMA_VERSION)),
            "schema_version drift"
        );
        let keys = schema
            .get("keys")
            .and_then(serde_json::Value::as_object)
            .expect("keys object");
        let schema_names: std::collections::HashSet<_> = keys.keys().map(String::as_str).collect();
        let code_names: std::collections::HashSet<_> = KEYS.iter().map(|spec| spec.name).collect();
        assert_eq!(code_names.len(), KEYS.len(), "duplicate key in code table");
        assert_eq!(schema_names, code_names, "schema/code key set drift");
        for spec in KEYS {
            let entry = keys
                .get(spec.name)
                .unwrap_or_else(|| panic!("schema missing {}", spec.name));
            let ty = entry
                .get("type")
                .and_then(serde_json::Value::as_str)
                .expect("type string");
            assert_eq!(ty, spec.ty.to_string(), "type drift for {}", spec.name);
            assert_eq!(
                entry.get("required").and_then(serde_json::Value::as_bool),
                Some(spec.required),
                "required drift for {}",
                spec.name
            );
            assert_eq!(
                entry
                    .get("env_overridable")
                    .and_then(serde_json::Value::as_bool),
                Some(spec.env_overridable),
                "env_overridable drift for {}",
                spec.name
            );
            match entry.get("env_var") {
                Some(serde_json::Value::String(var)) => assert_eq!(
                    Some(var.as_str()),
                    spec.env_var,
                    "env_var drift for {}",
                    spec.name
                ),
                Some(serde_json::Value::Null) | None => {
                    assert_eq!(None, spec.env_var, "env_var drift for {}", spec.name)
                }
                other => panic!("bad env_var shape for {}: {other:?}", spec.name),
            }
            // The declared default must re-parse through the same typed
            // path the accessor uses at load time.
            match (entry.get("default"), spec.default) {
                (Some(serde_json::Value::Null) | None, None) => {}
                (Some(json_default), Some(raw_default)) => {
                    let expected = match spec.ty {
                        ConfigType::U64 => json_default.as_u64().map(|n| n.to_string()),
                        ConfigType::Bool => json_default.as_bool().map(|b| b.to_string()),
                        ConfigType::Str => {
                            json_default.as_str().map(std::string::ToString::to_string)
                        }
                    }
                    .unwrap_or_else(|| {
                        panic!("schema default has wrong JSON type for {}", spec.name)
                    });
                    assert_eq!(expected, raw_default, "default drift for {}", spec.name);
                    spec.parse(raw_default).unwrap_or_else(|_| {
                        panic!("code default fails to parse for {}", spec.name)
                    });
                }
                (json_default, code_default) => panic!(
                    "default drift for {}: schema {json_default:?} vs code {code_default:?}",
                    spec.name
                ),
            }
        }
    }
}
