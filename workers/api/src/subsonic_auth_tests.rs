use super::*;
use std::cell::{Cell, RefCell};

const CURRENT: [u8; 32] = [0x31; 32];
const PREVIOUS: [u8; 32] = [0x32; 32];
const KEK: [u8; 32] = [0x41; 32];
const KEY: &str = "synthetic-api-key-not-a-live-credential";
const TOKEN_SECRET: &str = "synthetic-token-secret-distinct-from-api-key";
const NOW: u64 = 1_800_000_000;

struct Keks {
    lookups: RefCell<Vec<u32>>,
}

impl Keks {
    fn new() -> Self {
        Self {
            lookups: RefCell::new(Vec::new()),
        }
    }
}

impl KekSource for Keks {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        self.lookups.borrow_mut().push(version);
        (version == 7).then_some(&KEK)
    }
}

fn keyring() -> IndexKeyring<'static> {
    IndexKeyring::new(
        Pepper {
            version: 2,
            bytes: &CURRENT,
        },
        Some(Pepper {
            version: 1,
            bytes: &PREVIOUS,
        }),
    )
    .unwrap()
}

fn api_credentials() -> Credentials {
    Credentials {
        api_key: Some(KEY.into()),
        ..Credentials::default()
    }
}

fn token_credentials() -> Credentials {
    Credentials {
        username: Some("client_generated".into()),
        token: Some(md5_hex(format!("{TOKEN_SECRET}salt").as_bytes())),
        salt: Some("salt".into()),
        ..Credentials::default()
    }
}

fn api_record(version: u32) -> CredentialRecord {
    CredentialRecord {
        id: "credential-a".into(),
        user_id: "user-a".into(),
        username: "client_generated".into(),
        scope: "subsonic".into(),
        auth_mode: AuthMode::ApiKey,
        revoked: false,
        owner_active: true,
        api_key_hmac: Some(credential_hmac(
            KEY,
            if version == 2 { &CURRENT } else { &PREVIOUS },
        )),
        pepper_version: Some(version),
        token: None,
    }
}

fn token_record() -> CredentialRecord {
    let mut record = api_record(2);
    record.auth_mode = AuthMode::Token;
    record.api_key_hmac = None;
    record.pepper_version = None;
    record.token = Some(TokenEnvelope {
        envelope_version: 2,
        payload_format: 1,
        kek_version: 7,
        envelope: field_envelope::seal(
            &KEK,
            7,
            &token_context(&record.user_id, &record.id),
            TOKEN_SECRET.as_bytes(),
        )
        .unwrap(),
    });
    record
}

struct FakeStore {
    row: RefCell<Option<CredentialRecord>>,
    lookups: RefCell<Vec<u32>>,
    username_lookups: Cell<u32>,
    attempts: Cell<u32>,
    completions: Cell<u32>,
    last_used_at: Cell<Option<u64>>,
    deny_attempt: Cell<bool>,
    revoke_on_complete: Cell<bool>,
    rotate_on_complete: Cell<bool>,
    fail_completion: Cell<bool>,
    force_projection: Cell<bool>,
}

impl FakeStore {
    fn new(row: Option<CredentialRecord>) -> Self {
        Self {
            row: RefCell::new(row),
            lookups: RefCell::new(Vec::new()),
            username_lookups: Cell::new(0),
            attempts: Cell::new(0),
            completions: Cell::new(0),
            last_used_at: Cell::new(None),
            deny_attempt: Cell::new(false),
            revoke_on_complete: Cell::new(false),
            rotate_on_complete: Cell::new(false),
            fail_completion: Cell::new(false),
            force_projection: Cell::new(false),
        }
    }
}

fn snapshot_matches(row: &CredentialRecord, selected: &CredentialRecord) -> bool {
    let token_matches = match (&row.token, &selected.token) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.envelope_version == b.envelope_version
                && a.payload_format == b.payload_format
                && a.kek_version == b.kek_version
                && a.envelope.dek_wrapped_nonce == b.envelope.dek_wrapped_nonce
                && a.envelope.dek_wrapped == b.envelope.dek_wrapped
                && a.envelope.payload_nonce == b.envelope.payload_nonce
                && a.envelope.ciphertext == b.envelope.ciphertext
        }
        _ => false,
    };
    valid_record(row)
        && row.id == selected.id
        && row.user_id == selected.user_id
        && row.username == selected.username
        && row.auth_mode == selected.auth_mode
        && row.scope == selected.scope
        && row.pepper_version == selected.pepper_version
        && row.api_key_hmac == selected.api_key_hmac
        && token_matches
}

impl CredentialStore for FakeStore {
    async fn by_key(
        &self,
        version: u32,
        digest: &str,
    ) -> Result<Option<CredentialRecord>, AuthError> {
        self.lookups.borrow_mut().push(version);
        Ok(self
            .row
            .borrow()
            .as_ref()
            .filter(|r| {
                self.force_projection.get()
                    || (r.pepper_version == Some(version)
                        && r.api_key_hmac.as_deref() == Some(digest))
            })
            .cloned())
    }
    async fn by_username(&self, username: &str) -> Result<Option<CredentialRecord>, AuthError> {
        self.username_lookups.set(self.username_lookups.get() + 1);
        Ok(self
            .row
            .borrow()
            .as_ref()
            .filter(|r| self.force_projection.get() || r.username == username)
            .cloned())
    }
    async fn admit_attempt(
        &self,
        selected: &CredentialRecord,
        _now: u64,
    ) -> Result<bool, AuthError> {
        self.attempts.set(self.attempts.get() + 1);
        Ok(!self.deny_attempt.get()
            && self
                .row
                .borrow()
                .as_ref()
                .is_some_and(|r| snapshot_matches(r, selected)))
    }
    async fn complete(
        &self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> Result<bool, AuthError> {
        self.completions.set(self.completions.get() + 1);
        if self.fail_completion.get() {
            return Err(AuthError::Unavailable);
        }
        let mut row = self.row.borrow_mut();
        let Some(row) = row.as_mut() else {
            return Ok(false);
        };
        if self.revoke_on_complete.get() {
            row.revoked = true;
        }
        if self.rotate_on_complete.get() {
            row.api_key_hmac = Some("concurrent-replacement".into());
        }
        if !snapshot_matches(row, selected) {
            return Ok(false);
        }
        if let Some(reindex) = reindex {
            row.api_key_hmac = Some(reindex.api_key_hmac.clone());
            row.pepper_version = Some(reindex.pepper_version);
        }
        self.last_used_at.set(Some(now));
        Ok(true)
    }
}

#[tokio::test]
async fn current_key_authenticates_without_native_or_token_fallback() {
    let store = FakeStore::new(Some(api_record(2)));
    let keks = Keks::new();
    let principal = authenticate(&store, &api_credentials(), &keyring(), &keks, NOW)
        .await
        .unwrap();
    assert_eq!(principal.user_id(), "user-a");
    assert_eq!(principal.credential_id(), "credential-a");
    assert_eq!(principal.username(), "client_generated");
    assert_eq!(*store.lookups.borrow(), vec![2]);
    assert_eq!(store.username_lookups.get(), 0);
    assert_eq!(store.attempts.get(), 1);
    assert_eq!(store.last_used_at.get(), Some(NOW));
    assert!(keks.lookups.borrow().is_empty());
    assert!(!format!("{principal:?}").contains("client_generated"));
}

#[tokio::test]
async fn previous_key_reindexes_atomically_to_current() {
    let store = FakeStore::new(Some(api_record(1)));
    authenticate(&store, &api_credentials(), &keyring(), &Keks::new(), NOW)
        .await
        .unwrap();
    assert_eq!(*store.lookups.borrow(), vec![2, 1]);
    let stored = store.row.borrow();
    let row = stored.as_ref().unwrap();
    assert_eq!(row.pepper_version, Some(2));
    assert_eq!(row.api_key_hmac, Some(credential_hmac(KEY, &CURRENT)));
    assert_eq!(store.last_used_at.get(), Some(NOW));
}

#[tokio::test]
async fn unknown_key_is_bounded_to_two_versioned_lookups() {
    let store = FakeStore::new(None);
    assert_eq!(
        authenticate(&store, &api_credentials(), &keyring(), &Keks::new(), NOW).await,
        Err(AuthError::InvalidKey)
    );
    assert_eq!(*store.lookups.borrow(), vec![2, 1]);
    assert_eq!(store.attempts.get(), 0);
    assert_eq!(store.completions.get(), 0);
}

#[tokio::test]
async fn bad_projection_never_bypasses_digest_version_or_owner_checks() {
    for change in 0..7 {
        let mut row = api_record(2);
        match change {
            0 => row.api_key_hmac = Some("wrong".into()),
            1 => row.pepper_version = Some(9),
            2 => row.revoked = true,
            3 => row.owner_active = false,
            4 => row.scope = "library:read".into(),
            5 => row.username = "email@example.test".into(),
            _ => row.user_id.clear(),
        }
        let store = FakeStore::new(Some(row));
        store.force_projection.set(true);
        assert_eq!(
            authenticate(&store, &api_credentials(), &keyring(), &Keks::new(), NOW).await,
            Err(AuthError::InvalidKey)
        );
        assert_eq!(store.completions.get(), 0);
    }
}

#[tokio::test]
async fn password_presence_always_refuses_before_store_access() {
    let store = FakeStore::new(Some(api_record(2)));
    for password in ["", "plain", "enc:706c61696e"] {
        let mut credentials = api_credentials();
        credentials.password = Some(password.into());
        assert_eq!(
            authenticate(&store, &credentials, &keyring(), &Keks::new(), NOW).await,
            Err(AuthError::Unsupported)
        );
    }
    assert!(store.lookups.borrow().is_empty());
    assert_eq!(store.username_lookups.get(), 0);
}

#[tokio::test]
async fn mixed_key_and_token_fields_conflict_even_when_empty() {
    let store = FakeStore::new(Some(api_record(2)));
    for field in 0..3 {
        let mut credentials = api_credentials();
        match field {
            0 => credentials.username = Some(String::new()),
            1 => credentials.token = Some(String::new()),
            _ => credentials.salt = Some(String::new()),
        }
        assert_eq!(
            authenticate(&store, &credentials, &keyring(), &Keks::new(), NOW).await,
            Err(AuthError::Conflicting)
        );
    }
    assert!(store.lookups.borrow().is_empty());
}

#[tokio::test]
async fn revocation_cas_loss_and_commit_failure_return_no_identity() {
    for failure in 0..3 {
        let store = FakeStore::new(Some(api_record(1)));
        match failure {
            0 => store.revoke_on_complete.set(true),
            1 => store.rotate_on_complete.set(true),
            _ => store.fail_completion.set(true),
        }
        let expected = if failure == 2 {
            AuthError::Unavailable
        } else {
            AuthError::InvalidKey
        };
        assert_eq!(
            authenticate(&store, &api_credentials(), &keyring(), &Keks::new(), NOW).await,
            Err(expected)
        );
        assert_eq!(store.last_used_at.get(), None);
        assert_eq!(store.row.borrow().as_ref().unwrap().pepper_version, Some(1));
    }
}

#[tokio::test]
async fn token_auth_opens_exact_kek_and_accepts_equivalent_hex_case() {
    let store = FakeStore::new(Some(token_record()));
    let keks = Keks::new();
    let mut credentials = token_credentials();
    credentials.token = credentials.token.map(|t| t.to_ascii_uppercase());
    authenticate(&store, &credentials, &keyring(), &keks, NOW)
        .await
        .unwrap();
    assert_eq!(*keks.lookups.borrow(), vec![7]);
    assert!(store.lookups.borrow().is_empty());
    assert_eq!(store.username_lookups.get(), 1);
    assert_eq!(store.attempts.get(), 1);
    assert_eq!(store.last_used_at.get(), Some(NOW));
}

#[tokio::test]
async fn invalid_token_consumes_attempt_but_does_not_update_last_used() {
    let store = FakeStore::new(Some(token_record()));
    let mut credentials = token_credentials();
    credentials.token = Some("0".repeat(32));
    assert_eq!(
        authenticate(&store, &credentials, &keyring(), &Keks::new(), NOW).await,
        Err(AuthError::InvalidToken)
    );
    assert_eq!(store.attempts.get(), 1);
    assert_eq!(store.completions.get(), 0);
    assert_eq!(store.last_used_at.get(), None);
}

#[tokio::test]
async fn rate_denial_happens_before_unwrap_or_last_used() {
    let store = FakeStore::new(Some(token_record()));
    store.deny_attempt.set(true);
    let keks = Keks::new();
    assert_eq!(
        authenticate(&store, &token_credentials(), &keyring(), &keks, NOW).await,
        Err(AuthError::InvalidToken)
    );
    assert!(keks.lookups.borrow().is_empty());
    assert_eq!(store.completions.get(), 0);
    let api_store = FakeStore::new(Some(api_record(2)));
    api_store.deny_attempt.set(true);
    assert_eq!(
        authenticate(&api_store, &api_credentials(), &keyring(), &keks, NOW).await,
        Err(AuthError::InvalidKey)
    );
    assert_eq!(api_store.completions.get(), 0);
}

#[tokio::test]
async fn envelope_swaps_between_users_rows_or_fields_fail() {
    for swap in 0..3 {
        let mut row = token_record();
        let mut context = token_context(&row.user_id, &row.id);
        match swap {
            0 => context.scope = "user-b",
            1 => context.row = "credential-b",
            _ => context.field = "api_key_hmac",
        }
        let foreign_envelope =
            field_envelope::seal(&KEK, 7, &context, TOKEN_SECRET.as_bytes()).unwrap();
        row.token.as_mut().unwrap().envelope = foreign_envelope;
        let store = FakeStore::new(Some(row));
        assert_eq!(
            authenticate(&store, &token_credentials(), &keyring(), &Keks::new(), NOW).await,
            Err(AuthError::InvalidToken)
        );
        assert_eq!(store.completions.get(), 0);
    }
}

#[tokio::test]
async fn unknown_kek_never_falls_back_to_another_version() {
    let mut row = token_record();
    row.token.as_mut().unwrap().kek_version = 8;
    let store = FakeStore::new(Some(row));
    let keks = Keks::new();
    assert_eq!(
        authenticate(&store, &token_credentials(), &keyring(), &keks, NOW).await,
        Err(AuthError::Unavailable)
    );
    assert_eq!(*keks.lookups.borrow(), vec![8]);
    assert_eq!(store.completions.get(), 0);
}

#[tokio::test]
async fn malformed_or_cross_surface_rows_and_envelopes_fail_closed() {
    for change in 0..8 {
        let mut row = token_record();
        match change {
            0 => row.scope = "native".into(),
            1 => row.auth_mode = AuthMode::ApiKey,
            2 => row.api_key_hmac = Some("unexpected-extra-material".into()),
            3 => row.token.as_mut().unwrap().envelope_version = 99,
            4 => row.token.as_mut().unwrap().payload_format = 99,
            5 => row.token.as_mut().unwrap().envelope.ciphertext.clear(),
            6 => row.token.as_mut().unwrap().envelope.dek_wrapped.clear(),
            _ => row.token.as_mut().unwrap().envelope.ciphertext = vec![0; 529],
        }
        let store = FakeStore::new(Some(row));
        assert_eq!(
            authenticate(&store, &token_credentials(), &keyring(), &Keks::new(), NOW).await,
            Err(AuthError::InvalidToken)
        );
        assert_eq!(store.completions.get(), 0);
    }
}

#[tokio::test]
async fn email_missing_salt_and_unbounded_inputs_never_query() {
    let store = FakeStore::new(Some(token_record()));
    let mut email = token_credentials();
    email.username = Some("email@example.test".into());
    let mut missing_salt = token_credentials();
    missing_salt.salt = None;
    let mut long_salt = token_credentials();
    long_salt.salt = Some("s".repeat(257));
    let mut long_key = api_credentials();
    long_key.api_key = Some("k".repeat(513));
    for (credentials, error) in [
        (email, AuthError::InvalidToken),
        (missing_salt, AuthError::MissingSalt),
        (long_salt, AuthError::InvalidToken),
        (long_key, AuthError::InvalidKey),
    ] {
        assert_eq!(
            authenticate(&store, &credentials, &keyring(), &Keks::new(), NOW).await,
            Err(error)
        );
    }
    assert!(store.lookups.borrow().is_empty());
    assert_eq!(store.username_lookups.get(), 0);
}

#[tokio::test]
async fn both_mode_uses_independent_secrets_for_the_two_forms() {
    let mut row = token_record();
    row.auth_mode = AuthMode::Both;
    row.api_key_hmac = api_record(2).api_key_hmac;
    row.pepper_version = Some(2);
    let store = FakeStore::new(Some(row));
    authenticate(&store, &api_credentials(), &keyring(), &Keks::new(), NOW)
        .await
        .unwrap();
    authenticate(&store, &token_credentials(), &keyring(), &Keks::new(), NOW)
        .await
        .unwrap();
    let mut wrong = token_credentials();
    wrong.token = Some(md5_hex(format!("{KEY}salt").as_bytes()));
    assert_eq!(
        authenticate(&store, &wrong, &keyring(), &Keks::new(), NOW).await,
        Err(AuthError::InvalidToken)
    );
}

#[test]
fn keyring_and_dormant_retirement_fail_closed() {
    assert!(!keyring().can_retire_previous(1));
    assert!(keyring().can_retire_previous(0));
    for (version, bytes) in [(2, &PREVIOUS[..]), (1, &CURRENT[..]), (1, &PREVIOUS[..31])] {
        assert!(IndexKeyring::new(
            Pepper {
                version: 2,
                bytes: &CURRENT
            },
            Some(Pepper { version, bytes })
        )
        .is_err());
    }
    assert!(IndexKeyring::new(
        Pepper {
            version: 0,
            bytes: &CURRENT
        },
        None
    )
    .is_err());
}

#[test]
fn wire_error_codes_are_known_subsonic_codes() {
    for (error, code) in [
        (AuthError::Unsupported, 42),
        (AuthError::Conflicting, 43),
        (AuthError::MissingSalt, 10),
        (AuthError::InvalidKey, 44),
        (AuthError::InvalidToken, 40),
        (AuthError::Unavailable, 0),
    ] {
        assert_eq!(error.subsonic_code(), code);
    }
}
