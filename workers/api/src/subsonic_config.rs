//! Pure, bounded loader for the `SUBSONIC_INDEX_PEPPER` Worker-secret
//! keyring (ADR 0002 rule 2). No env access, no I/O: the Worker entry reads
//! the secret string and hands it to [`SubsonicIndexConfig::parse`].
//!
//! Format (JSON object, unknown fields rejected):
//!
//! ```json
//! {"current_version":2,"previous_version":1,
//!  "keys":{"2":"<unpadded base64url, 32..=64 bytes>","1":"<same>"}}
//! ```
//!
//! `previous_version` is optional (absent or `null`). `keys` holds exactly
//! the referenced versions and nothing else, so at most two peppers are ever
//! loaded. Versions are canonical positive decimal `u32` strings. Everything
//! fails closed with a payload-free [`ConfigError`]; no error, `Debug` or
//! `Display` output ever carries input text or key material.

use std::collections::HashSet;
use std::fmt;

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::native_rest::Pepper;
use crate::subsonic_auth::{AuthError, IndexKeyring};

/// Upper bound on the raw secret string (two 64-byte keys encode to well
/// under 300 bytes; this leaves room for whitespace without allowing
/// unbounded parse work).
pub const MAX_CONFIG_BYTES: usize = 1024;
/// Minimum key length: the 256-bit generation target, matching
/// `IndexKeyring::new`.
pub const MIN_KEY_BYTES: usize = 32;
/// Maximum decoded key length.
pub const MAX_KEY_BYTES: usize = 64;
/// Per-key encoded length cap (base64url of `MAX_KEY_BYTES`).
const MAX_KEY_B64_CHARS: usize = 86;

/// Payload-free failure; the variants say what class failed, never why with
/// any input text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    TooLarge,
    Malformed,
    BadVersion,
    DuplicateVersion,
    MissingKey,
    UnreferencedKey,
    BadKey,
}

impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfigError::{self}")
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "TooLarge",
            Self::Malformed => "Malformed",
            Self::BadVersion => "BadVersion",
            Self::DuplicateVersion => "DuplicateVersion",
            Self::MissingKey => "MissingKey",
            Self::UnreferencedKey => "UnreferencedKey",
            Self::BadKey => "BadKey",
        })
    }
}

impl std::error::Error for ConfigError {}

/// Ordered `(version string, base64url key)` pairs; keeps duplicate JSON
/// keys visible (a `HashMap` would silently last-win) and caps entries at
/// two while streaming.
struct RawKeys(Vec<(String, Zeroizing<String>)>);

impl<'de> Deserialize<'de> for RawKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawKeys;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a keyring map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<RawKeys, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = m.next_entry::<String, String>()? {
                    if out.len() >= 2 {
                        return Err(serde::de::Error::custom("too many keys"));
                    }
                    out.push((k, Zeroizing::new(v)));
                }
                Ok(RawKeys(out))
            }
        }
        d.deserialize_map(V)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    current_version: u64,
    #[serde(default)]
    previous_version: Option<u64>,
    keys: RawKeys,
}

/// Owned, validated keyring material. Key bytes are zeroized on drop and
/// `Debug` shows only the versions.
pub struct SubsonicIndexConfig {
    current_version: u32,
    current_key: Zeroizing<Vec<u8>>,
    previous: Option<(u32, Zeroizing<Vec<u8>>)>,
}

impl fmt::Debug for SubsonicIndexConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsonicIndexConfig")
            .field("current_version", &self.current_version)
            .field("previous_version", &self.previous.as_ref().map(|p| p.0))
            .finish()
    }
}

/// Canonical positive decimal u32: digits only, no sign, no leading zero.
fn parse_version_key(s: &str) -> Result<u32, ConfigError> {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) || b[0] == b'0' {
        return Err(ConfigError::BadVersion);
    }
    s.parse::<u32>().map_err(|_| ConfigError::BadVersion)
}

fn version_value(v: u64) -> Result<u32, ConfigError> {
    match u32::try_from(v) {
        Ok(n) if n != 0 => Ok(n),
        _ => Err(ConfigError::BadVersion),
    }
}

fn decode_key(raw: &str) -> Result<Zeroizing<Vec<u8>>, ConfigError> {
    if raw.len() > MAX_KEY_B64_CHARS {
        return Err(ConfigError::BadKey);
    }
    let bytes = Zeroizing::new(crate::b64::decode(raw).map_err(|_| ConfigError::BadKey)?);
    if !(MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&bytes.len()) {
        return Err(ConfigError::BadKey);
    }
    Ok(bytes)
}

impl SubsonicIndexConfig {
    /// Parse and fully validate the secret string. Both referenced entries
    /// are validated here so a bad transition never reaches a request path.
    pub fn parse(secret: &str) -> Result<Self, ConfigError> {
        if secret.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge);
        }
        let raw: Raw = serde_json::from_str(secret).map_err(|_| ConfigError::Malformed)?;

        let current = version_value(raw.current_version)?;
        let previous = raw.previous_version.map(version_value).transpose()?;
        if previous == Some(current) {
            return Err(ConfigError::DuplicateVersion);
        }

        // Canonical, non-duplicate versions across the key map.
        let mut seen = HashSet::new();
        let mut entries: Vec<(u32, Zeroizing<String>)> = Vec::with_capacity(2);
        for (k, v) in raw.keys.0 {
            let ver = parse_version_key(&k)?;
            if !seen.insert(ver) {
                return Err(ConfigError::DuplicateVersion);
            }
            entries.push((ver, v));
        }

        // Exactly the referenced versions: nothing missing, nothing extra.
        let wanted = 1 + usize::from(previous.is_some());
        if entries.len() > wanted
            || entries
                .iter()
                .any(|(v, _)| *v != current && Some(*v) != previous)
        {
            return Err(ConfigError::UnreferencedKey);
        }
        let find = |ver: u32| entries.iter().find(|(v, _)| *v == ver).map(|(_, k)| k);
        let current_key = decode_key(find(current).ok_or(ConfigError::MissingKey)?)?;
        let previous = match previous {
            Some(p) => Some((p, decode_key(find(p).ok_or(ConfigError::MissingKey)?)?)),
            None => None,
        };
        if let Some((_, pk)) = &previous {
            if **pk == *current_key {
                return Err(ConfigError::DuplicateVersion);
            }
        }
        Ok(Self {
            current_version: current,
            current_key,
            previous,
        })
    }

    pub fn current_version(&self) -> u32 {
        self.current_version
    }

    pub fn previous_version(&self) -> Option<u32> {
        self.previous.as_ref().map(|p| p.0)
    }

    /// Borrowing view for the auth core. The keyring holds no copy.
    pub fn keyring(&self) -> Result<IndexKeyring<'_>, AuthError> {
        IndexKeyring::new(
            Pepper {
                version: self.current_version,
                bytes: &self.current_key,
            },
            self.previous.as_ref().map(|(v, k)| Pepper {
                version: *v,
                bytes: k,
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(fill: u8, len: usize) -> String {
        crate::b64::encode(&vec![fill; len])
    }

    fn cfg(current: u64, previous: Option<u64>, keys: &[(&str, String)]) -> String {
        let keys = keys
            .iter()
            .map(|(v, key)| format!("\"{v}\":\"{key}\""))
            .collect::<Vec<_>>()
            .join(",");
        let prev = previous.map_or(String::new(), |p| format!("\"previous_version\":{p},"));
        format!("{{\"current_version\":{current},{prev}\"keys\":{{{keys}}}}}")
    }

    fn err(s: &str) -> ConfigError {
        SubsonicIndexConfig::parse(s).err().expect("must fail")
    }

    #[test]
    fn current_only_loads() {
        let c = SubsonicIndexConfig::parse(&cfg(1, None, &[("1", k(7, 32))])).unwrap();
        assert_eq!(c.current_version(), 1);
        assert_eq!(c.previous_version(), None);
        assert!(c.keyring().is_ok());
    }

    #[test]
    fn current_and_previous_select_exact_versions() {
        let s = cfg(3, Some(2), &[("2", k(1, 32)), ("3", k(2, 48))]);
        let c = SubsonicIndexConfig::parse(&s).unwrap();
        assert_eq!(c.current_version(), 3);
        assert_eq!(c.previous_version(), Some(2));
        assert_eq!(&*c.current_key, &vec![2u8; 48]);
        assert_eq!(&*c.previous.as_ref().unwrap().1, &vec![1u8; 32]);
        assert!(c.keyring().is_ok());
    }

    #[test]
    fn null_previous_is_absent() {
        let s = format!(
            "{{\"current_version\":1,\"previous_version\":null,\"keys\":{{\"1\":\"{}\"}}}}",
            k(1, 32)
        );
        assert_eq!(
            SubsonicIndexConfig::parse(&s).unwrap().previous_version(),
            None
        );
    }

    #[test]
    fn malformed_inputs_fail_closed() {
        for s in [
            "",
            "{}",
            "[]",
            "null",
            "not json",
            "{\"current_version\":\"1\",\"keys\":{}}",
            "{\"current_version\":1}",
            "{\"current_version\":1,\"keys\":[]}",
            "{\"current_version\":-1,\"keys\":{}}",
            "{\"current_version\":1.5,\"keys\":{}}",
        ] {
            assert_eq!(err(s), ConfigError::Malformed, "{s}");
        }
        let extra = format!(
            "{{\"current_version\":1,\"keys\":{{\"1\":\"{}\"}},\"x\":1}}",
            k(1, 32)
        );
        assert_eq!(err(&extra), ConfigError::Malformed);
    }

    #[test]
    fn oversize_is_rejected_before_parsing() {
        let s = " ".repeat(MAX_CONFIG_BYTES + 1);
        assert_eq!(err(&s), ConfigError::TooLarge);
    }

    #[test]
    fn zero_and_out_of_range_versions_rejected() {
        assert_eq!(
            err(&cfg(0, None, &[("0", k(1, 32))])),
            ConfigError::BadVersion
        );
        assert_eq!(
            err(&cfg(2, Some(0), &[("2", k(1, 32)), ("0", k(2, 32))])),
            ConfigError::BadVersion
        );
        assert_eq!(
            err(&cfg(4_294_967_296, None, &[("1", k(1, 32))])),
            ConfigError::BadVersion
        );
    }

    #[test]
    fn noncanonical_map_versions_rejected() {
        for v in ["01", "+1", "-1", "1.0", "", " 1", "0x1", "99999999999"] {
            assert_eq!(
                err(&cfg(1, None, &[(v, k(1, 32))])),
                ConfigError::BadVersion,
                "{v:?}"
            );
        }
    }

    #[test]
    fn duplicate_versions_rejected() {
        // current == previous
        assert_eq!(
            err(&cfg(1, Some(1), &[("1", k(1, 32))])),
            ConfigError::DuplicateVersion
        );
        // duplicate JSON map key
        assert_eq!(
            err(&cfg(1, None, &[("1", k(1, 32)), ("1", k(2, 32))])),
            ConfigError::DuplicateVersion
        );
        // identical material under two versions
        assert_eq!(
            err(&cfg(2, Some(1), &[("1", k(9, 32)), ("2", k(9, 32))])),
            ConfigError::DuplicateVersion
        );
    }

    #[test]
    fn missing_referenced_versions_rejected() {
        assert_eq!(
            err(&cfg(2, None, &[("1", k(1, 32))])),
            ConfigError::UnreferencedKey
        );
        assert_eq!(err(&cfg(1, None, &[])), ConfigError::MissingKey);
        assert_eq!(
            err(&cfg(2, Some(1), &[("2", k(1, 32))])),
            ConfigError::MissingKey
        );
    }

    #[test]
    fn unreferenced_or_excess_keys_rejected() {
        assert_eq!(
            err(&cfg(2, None, &[("2", k(1, 32)), ("1", k(2, 32))])),
            ConfigError::UnreferencedKey
        );
        let three = cfg(
            3,
            Some(2),
            &[("1", k(1, 32)), ("2", k(2, 32)), ("3", k(3, 32))],
        );
        assert_eq!(err(&three), ConfigError::Malformed);
        assert_eq!(
            err(&cfg(3, Some(2), &[("3", k(1, 32)), ("1", k(2, 32))])),
            ConfigError::UnreferencedKey
        );
    }

    #[test]
    fn short_long_and_undecodable_material_rejected() {
        assert_eq!(err(&cfg(1, None, &[("1", k(1, 31))])), ConfigError::BadKey);
        assert_eq!(err(&cfg(1, None, &[("1", k(1, 65))])), ConfigError::BadKey);
        assert_eq!(
            err(&cfg(1, None, &[("1", String::new())])),
            ConfigError::BadKey
        );
        assert_eq!(
            err(&cfg(1, None, &[("1", "!!!".into())])),
            ConfigError::BadKey
        );
        // padded base64 is not the shared unpadded-base64url format
        let padded = format!("{}=", k(1, 32));
        assert_eq!(err(&cfg(1, None, &[("1", padded)])), ConfigError::BadKey);
        // short previous also fails the whole load
        assert_eq!(
            err(&cfg(2, Some(1), &[("2", k(1, 32)), ("1", k(2, 16))])),
            ConfigError::BadKey
        );
    }

    #[test]
    fn debug_and_display_never_expose_material_or_input() {
        let key = k(0xAB, 32);
        let s = cfg(2, Some(1), &[("2", key.clone()), ("1", k(0xCD, 32))]);
        let c = SubsonicIndexConfig::parse(&s).unwrap();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("current_version: 2"));
        assert!(!dbg.contains(&key));
        assert!(!dbg.contains(&k(0xCD, 32)));
        let kr = c.keyring().unwrap();
        let _ = &kr; // IndexKeyring deliberately has no Debug
        for e in [
            ConfigError::TooLarge,
            ConfigError::Malformed,
            ConfigError::BadVersion,
            ConfigError::DuplicateVersion,
            ConfigError::MissingKey,
            ConfigError::UnreferencedKey,
            ConfigError::BadKey,
        ] {
            assert!(!format!("{e:?}{e}").contains(&key));
        }
        let bad = SubsonicIndexConfig::parse(&cfg(1, None, &[("1", key.clone() + "!")]))
            .err()
            .unwrap();
        assert!(!format!("{bad:?} {bad}").contains(&key));
    }
}
