use hmac::{Hmac, KeyInit, Mac};
use nntune_api::{
    b64, ingest_grant,
    ingest_session_auth::{self, Action, Grant},
    ingest_session_request::{replay, Request, RequestError},
    native_rest::{credential_hmac, CredentialError, NativeRestRecord, Pepper, Scope},
    play_grant,
};
use nntune_ingest::caller_auth::{authenticate, AuthError};
use sha2::Sha256;

const KEY: &str = "synthetic-grant-key-for-offline-tests";
const NOW: u64 = 1_800_000_000;

#[test]
fn ingest_grant_requires_a_positive_lifetime_and_expires_at_the_deadline() {
    for (now, ttl) in [(NOW, 0), (u64::MAX, 1)] {
        assert!(ingest_grant::issue_at("owner", now, ttl, Some(KEY)).is_err());
    }
    let token = ingest_grant::issue_at("owner", NOW, u64::MAX, Some(KEY)).unwrap();
    let header = ingest_grant::bearer(&token);
    let deadline = NOW + ingest_grant::MAX_GRANT_TTL;
    assert_eq!(
        authenticate(Some(&header), Some(KEY), deadline - 1)
            .unwrap()
            .user_id,
        "owner"
    );
    assert_eq!(
        authenticate(Some(&header), Some(KEY), deadline),
        Err(AuthError::Expired)
    );
}

#[test]
fn ingest_session_bearer_is_nonempty_and_valid_only_in_its_lifetime() {
    let peppers = [Pepper {
        version: 7,
        bytes: KEY.as_bytes(),
    }];
    let record = NativeRestRecord {
        id: "credential".into(),
        user_id: "owner".into(),
        key_prefix: String::new(),
        key_hmac: credential_hmac(KEY, peppers[0].bytes),
        pepper_version: 7,
        scopes: vec![Scope::IngestSubmit],
        created_at: NOW,
        expires_at: NOW + 2,
        revoked_at: None,
    };
    let grant = Grant {
        credential_id: record.id.clone(),
        environment: "test".into(),
        library_id: "library".into(),
        actions: vec![Action::Create],
    };
    let check = |record, bearer, now| {
        ingest_session_auth::authorize(
            (Some(record), Some(bearer)),
            &peppers,
            now,
            Some(&grant),
            ("test", "library"),
            Action::Create,
            None,
        )
    };
    let unauthenticated = Err(CredentialError::Unauthenticated);
    assert_eq!(check(&record, KEY, NOW - 1), unauthenticated);
    assert_eq!(check(&record, KEY, NOW), Ok(()));
    assert_eq!(check(&record, KEY, NOW + 1), Ok(()));
    assert_eq!(check(&record, KEY, NOW + 2), unauthenticated);
    let invalid = NativeRestRecord {
        key_hmac: credential_hmac("", peppers[0].bytes),
        ..record.clone()
    };
    assert_eq!(check(&invalid, "", NOW), unauthenticated);
}

#[test]
fn ingest_metadata_and_replay_identity_reject_nul() {
    let request = Request {
        file_bytes: 1,
        content_sha256: [7; 32],
        title: "Sonata",
        artist: "Musician",
        album: None,
    };
    for invalid in [
        Request {
            title: "title\0",
            ..request
        },
        Request {
            artist: "artist\0",
            ..request
        },
        Request {
            album: Some("album\0"),
            ..request
        },
    ] {
        assert!(matches!(
            invalid.fingerprint(),
            Err(RequestError::InvalidMetadata)
        ));
    }
    let fingerprint = request.fingerprint().unwrap();
    for invalid in [" ", "session\0"] {
        assert_eq!(
            replay("key", &fingerprint, Some((&fingerprint, invalid))),
            Err(RequestError::InvalidStoredIdentity)
        );
    }
    assert_eq!(
        replay("key", &fingerprint, Some((&fingerprint, "session-1"))),
        Ok(Some("session-1"))
    );
}

#[test]
fn playback_grant_requires_a_valid_epoch_and_positive_lifetime() {
    let mint = |epoch, now, ttl| {
        play_grant::issue_v2_at(
            "owner",
            "track",
            "play",
            epoch,
            now,
            ttl,
            Some(KEY.as_bytes()),
        )
    };
    for (epoch, now, ttl) in [
        (1, NOW, 0),
        (1, u64::MAX, 1),
        (0, NOW, 1),
        (u64::MAX, NOW, 1),
    ] {
        assert_eq!(
            mint(epoch, now, ttl),
            Err(play_grant::MintError::InvalidIdentity)
        );
    }
    let epoch = i64::MAX as u64;
    let grant = mint(epoch, NOW, 1).unwrap();
    assert_eq!(grant.exp_unix, NOW + 1);
    let (payload, signature) = grant
        .token
        .strip_prefix("v2.")
        .unwrap()
        .split_once('.')
        .unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    mac.verify_slice(&b64::decode(signature).unwrap()).unwrap();
    assert_eq!(
        b64::decode(payload).unwrap(),
        format!("owner|track|play|{epoch}|{}", NOW + 1).as_bytes()
    );
    assert_eq!(
        format!("{grant:?}"),
        format!("IssuedGrant {{ exp_unix: {}, .. }}", NOW + 1)
    );
}
