use std::cell::Cell;

use nntune_api::native_rest::{credential_hmac, Pepper};
use nntune_api::rotation::KekSource;
use nntune_api::subsonic_auth::{
    authenticate, AuthError, AuthMode, CredentialRecord, CredentialStore, IndexKeyring, Reindex,
};
use nntune_api::subsonic_config::{ConfigError, SubsonicIndexConfig};
use nntune_api::subsonic_dispatch::{rejects_transcode, ProductionStore};
use nntune_api::subsonic_http::{
    self, parse_credentials, CredentialParseError, StagingBody, StagingBodyError,
};
use nntune_subsonic::auth::{md5_hex, AuthStore, Credentials, Identity};
use nntune_subsonic::construction_budget::ConstructionBudget;
use nntune_subsonic::endpoints::Params;
use nntune_subsonic::UserAccount;

// The Worker exposes its JS adapter only on wasm32. Exercise its pure
// header classifier natively without constructing a platform Request.
#[allow(dead_code)]
#[path = "../src/subsonic_request_metadata.rs"]
mod metadata;

const PEPPER: [u8; 32] = [7; 32];
const KEK: [u8; 32] = [9; 32];

#[test]
fn keyring_rejects_equivalent_hmac_peppers_and_invalid_bounds() {
    let mut padded = PEPPER.to_vec();
    padded.push(0);
    assert_eq!(
        credential_hmac("fixture", &PEPPER),
        credential_hmac("fixture", &padded)
    );
    let current = || Pepper {
        version: 2,
        bytes: &PEPPER,
    };
    let previous = |bytes| Pepper { version: 1, bytes };
    assert!(IndexKeyring::new(current(), Some(previous(&padded))).is_err());
    assert!(IndexKeyring::new(
        Pepper {
            version: i32::MAX as u32 + 1,
            bytes: &PEPPER
        },
        None
    )
    .is_err());
    assert!(IndexKeyring::new(current(), Some(previous(&[1; 65]))).is_err());

    let first = nntune_api::b64::encode(&PEPPER);
    let second = nntune_api::b64::encode(&padded);
    let config = |second: &str| {
        format!(
            r#"{{"current_version":2,"previous_version":1,"keys":{{"2":"{first}","1":"{second}"}}}}"#
        )
    };
    assert_eq!(
        SubsonicIndexConfig::parse(&config(&second)).unwrap_err(),
        ConfigError::DuplicateVersion
    );
    let positional = format!(r#"[2,null,{{"2":"{first}"}}]"#);
    assert_eq!(
        SubsonicIndexConfig::parse(&positional).unwrap_err(),
        ConfigError::Malformed
    );

    padded[32] = 1;
    let distinct = SubsonicIndexConfig::parse(&config(&nntune_api::b64::encode(&padded))).unwrap();
    assert!(distinct.keyring().is_ok());
}

struct TokenStore(CredentialRecord);

impl CredentialStore for TokenStore {
    async fn by_key(&self, _: u32, _: &str) -> Result<Option<CredentialRecord>, AuthError> {
        panic!("token auth attempted a key lookup");
    }

    async fn by_username(&self, name: &str) -> Result<Option<CredentialRecord>, AuthError> {
        Ok((self.0.username == name).then(|| self.0.clone()))
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

#[derive(Default)]
struct Keys(Cell<usize>);

impl KekSource for Keys {
    fn kek(&self, _: u32) -> Option<&[u8; 32]> {
        self.0.set(self.0.get() + 1);
        Some(&KEK)
    }
}

#[tokio::test]
async fn malformed_envelope_is_rejected_before_kek_lookup() {
    let pepper = Pepper {
        version: 1,
        bytes: &PEPPER,
    };
    let fixture = nntune_api::subsonic_lifecycle::issue(
        "user-a",
        "Fixture",
        AuthMode::Token,
        1,
        &pepper,
        1,
        &KEK,
    )
    .unwrap();
    let ring = IndexKeyring::new(pepper, None).unwrap();
    let row = fixture.prepared.record;
    let secret = fixture.token_secret.unwrap();
    let credentials = Credentials {
        username: Some(row.username.clone()),
        token: Some(md5_hex(format!("{}salt", secret.as_str()).as_bytes())),
        salt: Some("salt".into()),
        ..Credentials::default()
    };

    for invalid_version in [Some(0), Some(u32::MAX), None] {
        let mut malformed = row.clone();
        let token = malformed.token.as_mut().unwrap();
        if let Some(version) = invalid_version {
            token.kek_version = version;
        } else {
            token.envelope.dek_wrapped.push(0);
        }
        let keys = Keys::default();
        let result = authenticate(&TokenStore(malformed), &credentials, &ring, &keys, 1).await;
        assert_eq!(result.unwrap_err(), AuthError::InvalidToken);
        assert_eq!(keys.0.get(), 0);
    }

    let keys = Keys::default();
    let principal = authenticate(&TokenStore(row), &credentials, &ring, &keys, 1)
        .await
        .unwrap();
    assert_eq!(principal.user_id(), "user-a");
    assert_eq!(keys.0.get(), 1);
}

#[test]
fn credential_sources_reject_decoded_controls_in_parameter_names() {
    for (query, body) in [
        ("apiKey=fixture&apiKey%00=other", ""),
        ("apiKey=fixture", "p%0A=other"),
        ("apiKey=fixture", "u%C2%85=other"),
    ] {
        assert_eq!(
            parse_credentials(Some(query), Some(body.as_bytes())).unwrap_err(),
            CredentialParseError::MalformedEncoding
        );
    }
    let credentials = parse_credentials(Some("apiKey=fixture&%E9%9F%B3=value"), None).unwrap();
    assert_eq!(credentials.api_key.as_deref(), Some("fixture"));
}

#[test]
fn dispatch_rejects_conflicting_playback_options_and_token_users() {
    for raw in [
        "format=raw&f%6Frmat=mp3",
        "maxBitRate=0&maxBitRate=192",
        "maxBitRate=-1",
    ] {
        assert!(rejects_transcode(&Params::parse(raw)));
    }
    assert!(!rejects_transcode(&Params::parse(
        "format=RaW&format=flac&maxBitRate=0"
    )));
    let store = ProductionStore::new(
        Identity {
            username: "client-a".into(),
            email: String::new(),
            is_admin: false,
        },
        UserAccount {
            email: String::new(),
            is_admin: false,
            is_manager: false,
        },
        vec![],
        vec![],
        vec![],
        vec![],
        0,
    );
    assert!(store.verify_token("client-b", "fixture", "salt").is_none());
    assert!(store.verify_token("client-a", "fixture", "salt").is_some());
}

#[test]
fn form_admission_matches_media_type_and_refuses_combined_headers() {
    let classify = |value: &str| metadata::is_form_content_type(value.bytes());
    let form = "application/x-www-form-urlencoded";
    assert_eq!(
        classify("Application/X-WWW-Form-Urlencoded ; charset=\"a,b\""),
        Ok(true)
    );
    assert_eq!(classify(&format!("{form}-other")), Ok(false));
    assert_eq!(classify(&format!("text/plain; note=\"{form}\"")), Ok(false));
    for value in [
        format!("text/plain, {form}"),
        format!("{form}; charset=utf-8, {form}"),
        format!("{form}; charset=utf-8\", text/plain\""),
    ] {
        assert_eq!(classify(&value), Err(metadata::InputError::Unreadable));
    }
}

#[test]
fn too_many_empty_body_chunks_poison_the_collector() {
    let mut body = StagingBody::new(None, ConstructionBudget::new(0)).unwrap();
    for _ in 0..64 {
        body.push(Ok(b"")).unwrap();
    }
    assert_eq!(body.push(Ok(b"")), Err(StagingBodyError::TooLarge));
    assert_eq!(body.push(Ok(b"x=1")), Err(StagingBodyError::TooLarge));
    assert_eq!(body.finish(None).err(), Some(StagingBodyError::TooLarge));
}
