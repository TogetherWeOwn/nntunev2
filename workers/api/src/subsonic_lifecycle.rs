//! Pure Subsonic credential issuance for native-session-owned lifecycle
//! handlers. Produces a storable [`CredentialRecord`] plus
//! the one-time display secrets; persistence, session/ownership checks,
//! per-user limits and audit belong to the caller.
//!
//! Safety properties:
//! - the username is a random generated non-PII handle, never an email or
//!   the label;
//! - API key and token secret are independent 32-byte CSPRNG values, so
//!   `Both` never shares a reversible API secret: the API key is stored only
//!   as a current-pepper HMAC, the token secret only inside a field envelope
//!   pinned to the exact context `subsonic_auth` re-derives at verify time;
//! - staging bytes and displayed secrets are `Zeroizing`; no type here has a
//!   `Debug` that reveals credential material, and errors are payload-free.

use std::fmt;

use serde::Serialize;
use zeroize::Zeroizing;

use crate::{
    field_envelope::{self, Context},
    native_rest::{credential_hmac, Pepper},
    subsonic_auth::{AuthMode, CredentialRecord, TokenEnvelope},
};

const MAX_USER_ID_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 64;
const API_KEY_PREFIX: &str = "nsk_";
const TOKEN_SECRET_PREFIX: &str = "nst_";
const USERNAME_PREFIX: &str = "nsu_";

/// Payload-free failure classes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LifecycleError {
    /// Malformed caller input or configuration versions.
    Invalid,
    /// Randomness or sealing unavailable.
    Unavailable,
    /// Per-user credential limit reached (reserved for the caller's
    /// persistence step; `issue` itself is stateless).
    Limit,
}

impl fmt::Debug for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LifecycleError")
    }
}

impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("credential lifecycle error")
    }
}

impl std::error::Error for LifecycleError {}

/// Row material to persist. Not `Debug`-revealing: the record carries an
/// HMAC and an encrypted envelope.
pub struct PreparedCredential {
    pub record: CredentialRecord,
    pub label: String,
    pub created_at: u64,
}

impl fmt::Debug for PreparedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedCredential { surface: subsonic }")
    }
}

/// Prepared row plus one-time secrets (shown to the user exactly once).
pub struct IssuedCredential {
    pub prepared: PreparedCredential,
    pub api_key: Option<Zeroizing<String>>,
    pub token_secret: Option<Zeroizing<String>>,
}

impl fmt::Debug for IssuedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedCredential")
            .field("api_key", &self.api_key.is_some())
            .field("token_secret", &self.token_secret.is_some())
            .finish()
    }
}

/// List projection: deliberately no username and no secret material.
#[derive(Clone, Serialize, Debug)]
pub struct ListedCredential {
    pub id: String,
    pub label: String,
    pub auth_mode: String,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

fn valid_version(v: u32) -> bool {
    (1..=i32::MAX as u32).contains(&v)
}

fn no_controls(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

fn random_secret(prefix: &str) -> Result<Zeroizing<String>, LifecycleError> {
    let bytes = Zeroizing::new(
        crate::random::secure_random_bytes::<32>().map_err(|_| LifecycleError::Unavailable)?,
    );
    let encoded = Zeroizing::new(crate::b64::encode(&*bytes));
    let mut out = String::with_capacity(prefix.len() + encoded.len());
    out.push_str(prefix);
    out.push_str(&encoded);
    Ok(Zeroizing::new(out))
}

/// Random (version 4, RFC 4122 variant) UUID in canonical hyphenated form.
fn new_uuid_v4() -> Result<String, LifecycleError> {
    let mut b = Zeroizing::new(
        crate::random::secure_random_bytes::<16>().map_err(|_| LifecycleError::Unavailable)?,
    );
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

fn new_username() -> Result<String, LifecycleError> {
    let bytes =
        crate::random::secure_random_bytes::<16>().map_err(|_| LifecycleError::Unavailable)?;
    Ok(format!("{USERNAME_PREFIX}{}", crate::b64::encode(&bytes)))
}

/// Issue a new active Subsonic credential for `user_id`. `pepper` is the
/// CURRENT index pepper (used for API-key modes only); `kek` seals the token
/// secret (token modes only) under `kek_version`.
pub fn issue(
    user_id: &str,
    label: &str,
    mode: AuthMode,
    now: u64,
    pepper: &Pepper<'_>,
    kek_version: u32,
    kek: &[u8; 32],
) -> Result<IssuedCredential, LifecycleError> {
    if user_id.is_empty() || user_id.len() > MAX_USER_ID_BYTES || !no_controls(user_id) {
        return Err(LifecycleError::Invalid);
    }
    let label = label.trim();
    if label.is_empty() || label.len() > MAX_LABEL_BYTES || !no_controls(label) {
        return Err(LifecycleError::Invalid);
    }
    let wants_key = matches!(mode, AuthMode::ApiKey | AuthMode::Both);
    let wants_token = matches!(mode, AuthMode::Token | AuthMode::Both);
    if wants_key && (!valid_version(pepper.version) || pepper.bytes.len() < 32) {
        return Err(LifecycleError::Invalid);
    }
    if wants_token && !valid_version(kek_version) {
        return Err(LifecycleError::Invalid);
    }

    let id = new_uuid_v4()?;
    let username = new_username()?;

    let (api_key, api_key_hmac, pepper_version) = if wants_key {
        let key = random_secret(API_KEY_PREFIX)?;
        let digest = credential_hmac(&key, pepper.bytes);
        (Some(key), Some(digest), Some(pepper.version))
    } else {
        (None, None, None)
    };

    let (token_secret, token) = if wants_token {
        let secret = random_secret(TOKEN_SECRET_PREFIX)?;
        let context = Context {
            table: "subsonic_credentials",
            field: "token_secret_encrypted",
            scope: user_id,
            row: &id,
            version: 2,
            format: 1,
        };
        let envelope = field_envelope::seal(kek, kek_version, &context, secret.as_bytes())
            .map_err(|_| LifecycleError::Unavailable)?;
        (
            Some(secret),
            Some(TokenEnvelope {
                envelope_version: 2,
                payload_format: 1,
                kek_version,
                envelope,
            }),
        )
    } else {
        (None, None)
    };

    Ok(IssuedCredential {
        prepared: PreparedCredential {
            record: CredentialRecord {
                id,
                user_id: user_id.to_string(),
                username,
                scope: "subsonic".to_string(),
                auth_mode: mode,
                revoked: false,
                owner_active: true,
                api_key_hmac,
                pepper_version,
                token,
            },
            label: label.to_string(),
            created_at: now,
        },
        api_key,
        token_secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rotation::KekSource;
    use crate::subsonic_auth::{authenticate, AuthError, CredentialStore, IndexKeyring, Reindex};
    use nntune_subsonic::auth::{md5_hex, Credentials};

    const PEPPER: [u8; 32] = [0x51; 32];
    const OTHER_PEPPER: [u8; 32] = [0x52; 32];
    const KEK: [u8; 32] = [0x61; 32];
    const NOW: u64 = 1_800_000_000;

    struct Keks;
    impl KekSource for Keks {
        fn kek(&self, version: u32) -> Option<&[u8; 32]> {
            (version == 3).then_some(&KEK)
        }
    }

    struct Store(CredentialRecord);
    impl CredentialStore for Store {
        async fn by_key(
            &self,
            v: u32,
            digest: &str,
        ) -> Result<Option<CredentialRecord>, AuthError> {
            Ok(
                (self.0.pepper_version == Some(v)
                    && self.0.api_key_hmac.as_deref() == Some(digest))
                .then(|| self.0.clone()),
            )
        }
        async fn by_username(&self, u: &str) -> Result<Option<CredentialRecord>, AuthError> {
            Ok((self.0.username == u).then(|| self.0.clone()))
        }
        async fn admit_attempt(&self, _: &CredentialRecord, _: u64) -> Result<bool, AuthError> {
            Ok(true)
        }
        async fn complete(
            &self,
            _: &CredentialRecord,
            _: Option<&Reindex>,
            _: u64,
        ) -> Result<bool, AuthError> {
            Ok(true)
        }
    }

    fn pepper() -> Pepper<'static> {
        Pepper {
            version: 2,
            bytes: &PEPPER,
        }
    }

    fn ring() -> IndexKeyring<'static> {
        IndexKeyring::new(pepper(), None).unwrap()
    }

    fn mk(mode: AuthMode) -> IssuedCredential {
        issue("user-a", "Phone", mode, NOW, &pepper(), 3, &KEK).unwrap()
    }

    fn key_creds(k: &str) -> Credentials {
        Credentials {
            api_key: Some(k.into()),
            ..Credentials::default()
        }
    }

    fn token_creds(user: &str, secret: &str) -> Credentials {
        Credentials {
            username: Some(user.into()),
            token: Some(md5_hex(format!("{secret}salty").as_bytes())),
            salt: Some("salty".into()),
            ..Credentials::default()
        }
    }

    #[tokio::test]
    async fn api_key_mode_round_trips_through_core_authenticate() {
        let i = mk(AuthMode::ApiKey);
        let key = i.api_key.as_ref().unwrap();
        assert!(key.starts_with("nsk_") && i.token_secret.is_none());
        let r = &i.prepared.record;
        assert!(r.token.is_none());
        assert_eq!(r.pepper_version, Some(2));
        assert_eq!(r.scope, "subsonic");
        assert!(!r.revoked && r.owner_active);
        let store = Store(r.clone());
        let p = authenticate(&store, &key_creds(key), &ring(), &Keks, NOW)
            .await
            .unwrap();
        assert_eq!(p.user_id(), "user-a");
        assert_eq!(p.credential_id(), r.id);
        assert_eq!(
            authenticate(&store, &key_creds("nsk_wrong"), &ring(), &Keks, NOW).await,
            Err(AuthError::InvalidKey)
        );
    }

    #[tokio::test]
    async fn token_mode_round_trips_and_has_no_api_material() {
        let i = mk(AuthMode::Token);
        let secret = i.token_secret.as_ref().unwrap();
        assert!(secret.starts_with("nst_") && i.api_key.is_none());
        let r = &i.prepared.record;
        assert!(r.api_key_hmac.is_none() && r.pepper_version.is_none());
        let t = r.token.as_ref().unwrap();
        assert_eq!(
            (t.envelope_version, t.payload_format, t.kek_version),
            (2, 1, 3)
        );
        let store = Store(r.clone());
        authenticate(
            &store,
            &token_creds(&r.username, secret),
            &ring(),
            &Keks,
            NOW,
        )
        .await
        .unwrap();
        assert_eq!(
            authenticate(
                &store,
                &token_creds(&r.username, "nst_other"),
                &ring(),
                &Keks,
                NOW
            )
            .await,
            Err(AuthError::InvalidToken)
        );
    }

    #[tokio::test]
    async fn both_mode_has_independent_secrets_and_both_authenticate() {
        let i = mk(AuthMode::Both);
        let key = i.api_key.as_ref().unwrap();
        let secret = i.token_secret.as_ref().unwrap();
        assert_ne!(key.as_str(), secret.as_str());
        assert_ne!(key[4..], secret[4..]);
        let r = &i.prepared.record;
        // The stored HMAC is of the API key only, never the token secret.
        assert_eq!(r.api_key_hmac, Some(credential_hmac(key, &PEPPER)));
        assert_ne!(r.api_key_hmac, Some(credential_hmac(secret, &PEPPER)));
        assert_ne!(r.api_key_hmac, Some(credential_hmac(key, &OTHER_PEPPER)));
        let store = Store(r.clone());
        authenticate(&store, &key_creds(key), &ring(), &Keks, NOW)
            .await
            .unwrap();
        authenticate(
            &store,
            &token_creds(&r.username, secret),
            &ring(),
            &Keks,
            NOW,
        )
        .await
        .unwrap();
        // The API key is not accepted as the token secret.
        assert!(
            authenticate(&store, &token_creds(&r.username, key), &ring(), &Keks, NOW)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn token_envelope_is_bound_to_user_and_row() {
        let i = mk(AuthMode::Token);
        let secret = i.token_secret.as_ref().unwrap();
        let good = i.prepared.record.clone();
        for swap in 0..3 {
            let mut r = good.clone();
            match swap {
                0 => r.user_id = "user-b".into(),
                1 => r.id = "00000000-0000-4000-8000-000000000000".into(),
                _ => r.token.as_mut().unwrap().kek_version = 4,
            }
            let store = Store(r.clone());
            assert!(
                authenticate(
                    &store,
                    &token_creds(&r.username, secret),
                    &ring(),
                    &Keks,
                    NOW
                )
                .await
                .is_err(),
                "swap {swap}"
            );
        }
        // Envelope moved from another issued credential also fails.
        let other = mk(AuthMode::Token);
        let mut r = good.clone();
        r.token = other.prepared.record.token.clone();
        let store = Store(r.clone());
        assert!(authenticate(
            &store,
            &token_creds(&r.username, secret),
            &ring(),
            &Keks,
            NOW
        )
        .await
        .is_err());
    }

    #[test]
    fn identifiers_are_random_generated_and_non_pii() {
        let a = mk(AuthMode::Both);
        let b = mk(AuthMode::Both);
        let (ra, rb) = (&a.prepared.record, &b.prepared.record);
        assert_ne!(ra.id, rb.id);
        assert_ne!(ra.username, rb.username);
        assert_ne!(a.api_key, b.api_key);
        assert_eq!(ra.id.len(), 36);
        let bytes = ra.id.as_bytes();
        assert!([8, 13, 18, 23].iter().all(|i| bytes[*i] == b'-'));
        assert_eq!(bytes[14], b'4');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'));
        assert!(ra.username.starts_with("nsu_") && ra.username.len() == 4 + 22);
        assert!(!ra.username.contains("user-a") && !ra.username.contains("Phone"));
        assert!(ra
            .username
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'));
        assert_ne!(ra.username, ra.id);
    }

    #[test]
    fn label_is_trimmed_and_timestamp_recorded() {
        let i = issue("u", "  Car  ", AuthMode::ApiKey, 42, &pepper(), 3, &KEK).unwrap();
        assert_eq!(i.prepared.label, "Car");
        assert_eq!(i.prepared.created_at, 42);
        let max = "l".repeat(64);
        assert!(issue("u", &max, AuthMode::ApiKey, 1, &pepper(), 3, &KEK).is_ok());
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        let p = pepper();
        let long_user = "u".repeat(129);
        let long_label = "l".repeat(65);
        let bad: Vec<(&str, &str)> = vec![
            ("", "ok"),
            (&long_user, "ok"),
            ("u\n", "ok"),
            ("u", ""),
            ("u", "   "),
            ("u", &long_label),
            ("u", "a\u{0}b"),
            ("u", "a\tb"),
        ];
        for (u, l) in bad {
            assert_eq!(
                issue(u, l, AuthMode::Both, NOW, &p, 3, &KEK).err(),
                Some(LifecycleError::Invalid)
            );
        }
        for v in [0, i32::MAX as u32 + 1, u32::MAX] {
            let bad_pepper = Pepper {
                version: v,
                bytes: &PEPPER,
            };
            assert_eq!(
                issue("u", "l", AuthMode::ApiKey, NOW, &bad_pepper, 3, &KEK).err(),
                Some(LifecycleError::Invalid)
            );
            assert_eq!(
                issue("u", "l", AuthMode::Token, NOW, &p, v, &KEK).err(),
                Some(LifecycleError::Invalid)
            );
        }
        let short = Pepper {
            version: 2,
            bytes: &PEPPER[..31],
        };
        assert_eq!(
            issue("u", "l", AuthMode::Both, NOW, &short, 3, &KEK).err(),
            Some(LifecycleError::Invalid)
        );
        // Versions irrelevant to the chosen mode are not validated.
        assert!(issue("u", "l", AuthMode::Token, NOW, &short, 3, &KEK).is_ok());
        assert!(issue("u", "l", AuthMode::ApiKey, NOW, &p, 0, &KEK).is_ok());
        let max = Pepper {
            version: i32::MAX as u32,
            bytes: &PEPPER,
        };
        assert!(issue("u", "l", AuthMode::Both, NOW, &max, i32::MAX as u32, &KEK).is_ok());
    }

    #[test]
    fn debug_and_errors_never_reveal_material() {
        let i = mk(AuthMode::Both);
        let key = i.api_key.as_ref().unwrap().to_string();
        let secret = i.token_secret.as_ref().unwrap().to_string();
        let r = &i.prepared.record;
        let shown = format!("{i:?}{:?}", i.prepared);
        for needle in [
            key.as_str(),
            secret.as_str(),
            r.username.as_str(),
            r.id.as_str(),
            r.api_key_hmac.as_deref().unwrap(),
            "Phone",
            "user-a",
        ] {
            assert!(!shown.contains(needle));
        }
        for e in [
            LifecycleError::Invalid,
            LifecycleError::Unavailable,
            LifecycleError::Limit,
        ] {
            assert_eq!(format!("{e:?}"), "LifecycleError");
            assert_eq!(format!("{e}"), "credential lifecycle error");
        }
        let listed = ListedCredential {
            id: r.id.clone(),
            label: "Phone".into(),
            auth_mode: "both".into(),
            created_at: NOW,
            last_used_at: None,
            revoked_at: None,
        };
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("username") && !json.contains(&r.username));
        assert!(!json.contains(&key) && !json.contains(&secret));
    }
}
