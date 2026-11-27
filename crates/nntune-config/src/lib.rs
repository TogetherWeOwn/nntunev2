//! Typed accessor for the versioned KV config schema.
//!
//! The schema source of truth is `config/schema.json`: every key declares
//! its type, default, and env-overridability. Workers must read config keys
//! through [`Config`], never with raw stringly KV lookups on config paths:
//! [`Config::load`] validates the whole key set at once (defaults applied,
//! missing required and undeclared keys rejected, types and runtime bounds
//! checked), and [`Config::apply_env`] layers the documented per-key
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
    /// tolerate surrounding whitespace; strings keep their exact spelling
    /// for field-specific validation. Empty or whitespace-only strings are
    /// rejected, matching the CI parity checker (`config_parity_check.py`)
    /// for required strings. Bools accept only `true` and `false`, in any
    /// ASCII case.
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

/// Errors emitted by `Config` identify a declared key or redact the unknown name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingRequired(&'static str),
    InvalidType {
        key: &'static str,
        expected: ConfigType,
    },
    InvalidValue(&'static str),
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
            ConfigError::InvalidValue(key) => write!(f, "config key {key} has an invalid value"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Validated, typed view over one environment's KV config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    values: HashMap<String, Value>,
}

impl Config {
    /// Fill defaults and validate the complete KV set, including runtime
    /// bounds and RP identity. Missing required and undeclared keys fail.
    pub fn load(kv: &HashMap<String, String>) -> Result<Self, ConfigError> {
        for name in kv.keys() {
            if spec_for(name).is_none() {
                return Err(ConfigError::UnknownKey(REDACTED_KEY.to_owned()));
            }
        }
        let mut values = HashMap::with_capacity(KEYS.len());
        for spec in KEYS {
            match kv.get(spec.name) {
                Some(raw) => {
                    values.insert(spec.name.to_owned(), parse_config_value(spec, raw)?);
                }
                None => match spec.default {
                    Some(default) => {
                        values.insert(spec.name.to_owned(), parse_config_value(spec, default)?);
                    }
                    None if spec.required => return Err(ConfigError::MissingRequired(spec.name)),
                    None => {}
                },
            }
        }
        Config { values }.validate_identity()
    }

    /// Layer per-key environment overrides on top of KV. Only keys declared
    /// `env_overridable` are consulted, and only through their documented
    /// `env_var` name; every other entry in `env` is ignored. Precedence is
    /// KV first, environment second (local dev and `wrangler [vars]` win).
    /// The overlay is atomic: validate every override and the resulting
    /// RP identity before committing anything. A failing overlay leaves
    /// the previous config intact.
    pub fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        let mut candidate = self.clone();
        for spec in KEYS.iter().filter(|spec| spec.env_overridable) {
            if let Some(raw) = spec.env_var.and_then(|var| env.get(var)) {
                candidate
                    .values
                    .insert(spec.name.to_owned(), parse_config_value(spec, raw)?);
            }
        }
        // The RP and origin may both change in the same overlay.
        let validated = candidate.validate_identity()?;
        *self = validated;
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

    fn validate_identity(self) -> Result<Self, ConfigError> {
        let rp_id = self
            .get_str("auth.rp_id")
            .ok_or(ConfigError::MissingRequired("auth.rp_id"))?;
        let origin = self
            .get_str("auth.rp_origin")
            .ok_or(ConfigError::MissingRequired("auth.rp_origin"))?;
        let host = rp_origin_host(rp_id).ok_or(ConfigError::InvalidValue("auth.rp_id"))?;
        if !valid_rp_origin(origin, host) {
            return Err(ConfigError::InvalidValue("auth.rp_origin"));
        }
        Ok(self)
    }
}

// An undeclared name is arbitrary input, including potential secrets or newlines.
const REDACTED_KEY: &str = "<redacted>";

fn parse_config_value(spec: &KeySpec, raw: &str) -> Result<Value, ConfigError> {
    let value = spec.parse(raw)?;
    let Value::U64(number) = value else {
        return Ok(value);
    };
    let bounds = match spec.name {
        // Match the API's credential lifetimes and invite throttle.
        "auth.challenge_ttl_secs" => 1..=300,
        "auth.invite_redemption_max_attempts" => 1..=10,
        "auth.invite_redemption_window_secs" => 300..=300,
        "auth.magic_link_ttl_secs" => 1..=900,
        "session.ttl_secs" => 1..=86_400,
        "ingest.max_nzb_bytes" => 1..=2 * 1024 * 1024,
        // Retries match the Queue consumer budget. Payloads survive the
        // 15-day replay window and expire within the current 30-day retention.
        "ingest.payload_retry_attempts" => 4..=4,
        "ingest.payload_ttl_secs" => 15 * 86_400..=30 * 86_400,
        // KV requires at least 60 seconds. The MB cache interface takes u32.
        "mb.lookup_cache_ttl_s" | "mb.search_cache_ttl_s" => 60..=u64::from(u32::MAX),
        "mb.max_query_chars" => 1..=100,
        "mb.search_limit" => 1..=25,
        // The streaming buffer flushes one minimum-size R2 part at a time.
        "stream.r2_min_part_bytes" => 5 * 1024 * 1024..=5 * 1024 * 1024,
        _ => return Ok(value),
    };
    if !bounds.contains(&number) {
        return Err(ConfigError::InvalidValue(spec.name));
    }
    Ok(value)
}

// This config accepts conventional ASCII DNS names and the API's local-dev
// loopbacks. A letter-led final label avoids ambiguous numeric IPv4 spellings.
fn rp_origin_host(rp_id: &str) -> Option<&str> {
    match rp_id {
        "::1" => return Some("[::1]"),
        "localhost" | "127.0.0.1" => return Some(rp_id),
        _ => {}
    }
    let (_, tld) = rp_id.rsplit_once('.')?;
    (rp_id.len() <= 253
        && matches!(tld.as_bytes().first(), Some(b'a'..=b'z'))
        && rp_id.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && !label.starts_with("xn--")
                && label
                    .bytes()
                    .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        }))
    .then_some(rp_id)
}

fn valid_rp_origin(origin: &str, host: &str) -> bool {
    let (authority, default_port) = if let Some(authority) = origin.strip_prefix("https://") {
        (authority, 443)
    } else if let (true, Some(authority)) = (
        matches!(host, "localhost" | "127.0.0.1" | "[::1]"),
        origin.strip_prefix("http://"),
    ) {
        (authority, 80)
    } else {
        return false;
    };
    match authority.strip_prefix(host) {
        Some("") => true,
        Some(suffix) => suffix.strip_prefix(':').is_some_and(|port| {
            port.len() <= 5
                && !port.starts_with('0')
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port != default_port)
        }),
        None => false,
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
            ("auth.rp_id", "staging.nntune.com"),
            ("auth.rp_origin", "https://staging.nntune.com"),
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
        assert_eq!(config.get_str("auth.rp_id"), Some("staging.nntune.com"));
    }

    #[test]
    fn explicit_kv_values_win_over_defaults() {
        let mut raw = identity();
        raw.insert("session.ttl_secs".to_string(), "3600".to_string());
        let config = Config::load(&raw).expect("explicit values load");
        assert_eq!(config.get_u64("session.ttl_secs"), Some(3600));
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
    }

    #[test]
    fn u64_type_parser_accepts_endpoints_and_rejects_overflow() {
        // Endpoints pin current `trimmed.parse::<u64>()` behavior.
        let spec = spec_for("session.ttl_secs").unwrap();
        for (raw, expected) in [("0", 0u64), ("18446744073709551615", u64::MAX)] {
            assert_eq!(
                spec.parse(raw),
                Ok(Value::U64(expected)),
                "endpoint {raw:?} must parse"
            );
        }
        // Overflow and non-decimal shapes fail as InvalidType.
        for bad in ["18446744073709551616", "0x10", "+18446744073709551616"] {
            let mut kv_input = identity();
            kv_input.insert("session.ttl_secs".to_string(), bad.to_string());
            assert_eq!(
                Config::load(&kv_input),
                Err(ConfigError::InvalidType {
                    key: "session.ttl_secs",
                    expected: ConfigType::U64,
                }),
                "bad value {bad:?} must fail"
            );
        }
    }

    #[test]
    fn whitespace_padded_numerics_load() {
        let mut raw = identity();
        raw.insert("session.ttl_secs".to_string(), " 3600 ".to_string());
        let config = Config::load(&raw).expect("padded numeric loads");
        assert_eq!(config.get_u64("session.ttl_secs"), Some(3600));
    }

    #[test]
    fn identity_with_surrounding_spaces_is_rejected() {
        let spaced = "  staging.nntune.com  ";
        let mut raw = identity();
        raw.insert("auth.rp_id".to_string(), spaced.to_string());
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::InvalidValue("auth.rp_id"))
        );

        let mut config = Config::load(&identity()).expect("identity loads");
        let env = kv(&[("RP_ID", spaced)]);
        assert_eq!(
            config.apply_env(&env),
            Err(ConfigError::InvalidValue("auth.rp_id"))
        );
        assert_eq!(config.get_str("auth.rp_id"), Some("staging.nntune.com"));
    }

    #[test]
    fn unknown_kv_key_rejected() {
        let mut raw = identity();
        raw.insert("debug.backdoor".to_string(), "true".to_string());
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::UnknownKey(REDACTED_KEY.to_owned()))
        );

        raw.remove("debug.backdoor");
        raw.insert(
            "vault.indexer_allow_redirects".to_string(),
            "true".to_string(),
        );
        assert_eq!(
            Config::load(&raw),
            Err(ConfigError::UnknownKey(REDACTED_KEY.to_owned()))
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
        let env = kv(&[
            ("RP_ID", "other.example"),
            ("RP_ORIGIN", "https://other.example"),
            ("SESSION_TTL_SECS", "soon"),
        ]);
        assert_eq!(
            config.apply_env(&env),
            Err(ConfigError::InvalidType {
                key: "session.ttl_secs",
                expected: ConfigType::U64,
            }),
        );
        assert_eq!(
            config.get_str("auth.rp_id"),
            Some("staging.nntune.com"),
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
        assert_eq!(config.get_str("auth.rp_id"), Some("staging.nntune.com"));
    }

    #[test]
    fn synthetic_bool_key_parses_and_is_read_by_get_bool() {
        // Keep Bool parsing/getter coverage even while the shipped schema has
        // no bool key; this synthetic spec must not become a production key.
        let spec = KeySpec {
            name: "test.bool_flag",
            ty: ConfigType::Bool,
            default: Some("false"),
            required: false,
            env_overridable: false,
            env_var: None,
        };

        assert_eq!(spec.parse(" true "), Ok(Value::Bool(true)));
        assert_eq!(spec.parse("FALSE"), Ok(Value::Bool(false)));
        assert_eq!(
            spec.parse("yes"),
            Err(ConfigError::InvalidType {
                key: "test.bool_flag",
                expected: ConfigType::Bool,
            })
        );

        let mut config = Config::load(&identity()).unwrap();
        config
            .values
            .insert(spec.name.to_owned(), spec.parse("true").unwrap());
        assert_eq!(config.get_bool(spec.name), Some(true));
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

    /// Load one checked-in environment fixture (`config/{env}.json`) and
    /// convert each value into the exact string-valued KV input representation
    /// [`Config::load`] consumes: JSON strings stay exact, numbers/bools use
    /// their canonical text form (what KV would hold on the wire).
    fn fixture_kv(env: &str) -> HashMap<String, String> {
        let path = format!("{}/../../config/{env}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).expect("fixture readable");
        let json: serde_json::Value = serde_json::from_str(&text).expect("fixture parses");
        json.as_object()
            .expect("fixture is an object")
            .iter()
            .map(|(key, value)| {
                let raw = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    other => panic!("non-scalar fixture value for {key}: {other:?}"),
                };
                (key.clone(), raw)
            })
            .collect()
    }

    #[test]
    fn shipped_fixtures_load_through_typed_getters() {
        for env in ["staging", "prod"] {
            let kv = fixture_kv(env);
            // Every declared key must be present in the fixture and no extra
            // keys may ship: future additions fail here until covered.
            let fixture_names: std::collections::HashSet<_> =
                kv.keys().map(String::as_str).collect();
            let code_names: std::collections::HashSet<_> =
                KEYS.iter().map(|spec| spec.name).collect();
            assert_eq!(
                fixture_names, code_names,
                "{env} fixture key set drift vs code table"
            );
            let config = Config::load(&kv).expect("shipped fixture loads");
            // Every declared fixture value round-trips through its correct
            // typed getter via the same parse path `load` uses.
            for spec in KEYS {
                let raw = kv
                    .get(spec.name)
                    .unwrap_or_else(|| panic!("{env} fixture missing {}", spec.name));
                match spec.parse(raw).unwrap_or_else(|_| {
                    panic!("{env} fixture value fails to parse for {}", spec.name)
                }) {
                    Value::U64(expected) => assert_eq!(
                        config.get_u64(spec.name),
                        Some(expected),
                        "{env} fixture {}",
                        spec.name
                    ),
                    Value::Str(_) => assert_eq!(
                        config.get_str(spec.name),
                        Some(raw.as_str()),
                        "{env} fixture {}",
                        spec.name
                    ),
                    Value::Bool(expected) => assert_eq!(
                        config.get_bool(spec.name),
                        Some(expected),
                        "{env} fixture {}",
                        spec.name
                    ),
                }
            }
        }

        // Intentional staging/production identity divergence. Per
        // config/README.md, values may otherwise differ (per-env tuning is
        // the point) and the parity checker stays green on value-only drift,
        // so this test pins only the identity values plus literal spot
        // values — it must not require cross-env sameness.
        let staging = Config::load(&fixture_kv("staging")).expect("staging loads");
        let prod = Config::load(&fixture_kv("prod")).expect("prod loads");
        assert_eq!(staging.get_str("auth.rp_id"), Some("staging.nntune.com"));
        assert_eq!(prod.get_str("auth.rp_id"), Some("nntune.com"));
        assert_eq!(
            staging.get_str("auth.rp_origin"),
            Some("https://staging.nntune.com")
        );
        assert_eq!(prod.get_str("auth.rp_origin"), Some("https://nntune.com"));
        // Literal anchors (not derived from `spec.parse`): a parser mutant
        // returning n+1 must fail here. Both envs currently share these
        // tuning values; if per-env tuning intentionally diverges, split the
        // pins per env rather than reintroducing a blanket sameness loop.
        const EXPECTED_U64: &[(&str, u64)] = &[
            ("auth.challenge_ttl_secs", 300),
            ("auth.invite_redemption_max_attempts", 10),
            ("auth.invite_redemption_window_secs", 300),
            ("auth.magic_link_ttl_secs", 900),
            ("ingest.max_nzb_bytes", 2097152),
            ("ingest.payload_retry_attempts", 4),
            ("ingest.payload_ttl_secs", 2592000),
            ("mb.lookup_cache_ttl_s", 86400),
            ("mb.max_query_chars", 100),
            ("mb.search_cache_ttl_s", 300),
            ("mb.search_limit", 25),
            ("session.ttl_secs", 86400),
            ("stream.r2_min_part_bytes", 5242880),
        ];
        for (key, expected) in EXPECTED_U64 {
            assert_eq!(
                staging.get_u64(key),
                Some(*expected),
                "staging fixture {key}"
            );
            assert_eq!(prod.get_u64(key), Some(*expected), "prod fixture {key}");
        }
    }

    #[test]
    fn shipped_fixture_mutations_fail_closed() {
        // In-memory mutations only; the shipped JSON files stay untouched.
        for env in ["staging", "prod"] {
            let missing_identity = {
                let mut kv = fixture_kv(env);
                kv.remove("auth.rp_id");
                kv
            };
            assert_eq!(
                Config::load(&missing_identity),
                Err(ConfigError::MissingRequired("auth.rp_id")),
                "{env} fixture without identity must fail"
            );

            let wrong_type = {
                let mut kv = fixture_kv(env);
                kv.insert("session.ttl_secs".to_string(), "not-a-number".to_string());
                kv
            };
            assert_eq!(
                Config::load(&wrong_type),
                Err(ConfigError::InvalidType {
                    key: "session.ttl_secs",
                    expected: ConfigType::U64,
                }),
                "{env} fixture with wrong type must fail"
            );

            let unknown_key = {
                let mut kv = fixture_kv(env);
                kv.insert("debug.backdoor".to_string(), "true".to_string());
                kv
            };
            assert_eq!(
                Config::load(&unknown_key),
                Err(ConfigError::UnknownKey(REDACTED_KEY.to_owned())),
                "{env} fixture with unknown key must fail"
            );
        }
    }
}
