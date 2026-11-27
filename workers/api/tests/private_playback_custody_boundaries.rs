#![cfg(not(target_arch = "wasm32"))]

use std::{
    cell::Cell,
    time::{Duration, UNIX_EPOCH},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use nntune_api::{
    handlers::browser_playback_relay::BrowserReceipt,
    play_grant::IssuedGrant,
    playback::LeaseRow,
    postgres::PgClient,
    private_catalogue_read::{self, PublicationPins},
    storage_digest_read::{protect_digest_read, DigestReadTarget},
};
use sha2::Sha256;

#[path = "support/private_playback_offline_client.rs"]
mod support;
use support::offline_client;

#[tokio::test]
async fn invalid_digest_geometry_never_requests_authority_or_storage() {
    let mut db = offline_client().await;
    let now = UNIX_EPOCH + Duration::from_secs(100);
    let until = now + Duration::from_secs(30);
    for (plaintext_bytes, valid) in [(-1, false), (0, false), ((1 << 33) + 1, false), (1, true)] {
        let target = DigestReadTarget {
            environment: [1; 16],
            library: [2; 16],
            reference: [3; 16],
            object: [4; 16],
            generation: [5; 16],
            provider_configuration: [6; 16],
            root_version: [7; 16],
            plaintext_sha256: [8; 32],
            plaintext_bytes,
        };
        let authorized = Cell::new(false);
        let result = protect_digest_read(
            &mut db,
            target,
            until,
            async |_| {
                authorized.set(true);
                Some(until)
            },
            || now,
        )
        .await;
        assert_eq!(
            authorized.get(),
            valid,
            "authorization for length {plaintext_bytes}"
        );
        if valid {
            assert!(
                result.is_err(),
                "valid length must reach the disconnected store"
            );
        } else {
            assert!(
                matches!(result, Ok(None)),
                "invalid media length reached storage"
            );
        }
    }
}

#[tokio::test]
async fn signed_grant_cannot_outlive_its_receipt_lease() {
    const SECRET: &[u8] = &[42; 32];
    let mut db = offline_client().await;
    let user = "member";
    let track = "22222222-2222-2222-2222-222222222222";
    let play = "private_1234567890abcdefghijkl";
    let digest = "a".repeat(64);
    let payload = format!("{user}|{track}|{play}|1|120|{digest}");
    let signed = format!("v4.{}", URL_SAFE_NO_PAD.encode(payload));
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(signed.as_bytes());
    let mut receipt = BrowserReceipt {
        user_id: user.into(),
        session_binding: "synthetic".into(),
        lease: LeaseRow {
            session_id: "tab".into(),
            track_id: track.into(),
            play_id: play.into(),
            epoch: 1,
            expires_at: 120,
            continuation_deadline: 120,
        },
        artifact_sha256: digest,
        duration_ms: 1.try_into().unwrap(),
        grant: IssuedGrant {
            token: format!(
                "{signed}.{}",
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            ),
            exp_unix: 120,
        },
    };
    let pins = PublicationPins {
        environment: [1; 16],
        library: [2; 16],
        accepted_seq: 1,
        provider_configuration: [3; 16],
        bucket: "test-bucket",
        version: "media-version",
    };
    let now = || UNIX_EPOCH + Duration::from_secs(100);
    let resolve = async |db: &mut PgClient, receipt: &BrowserReceipt| {
        private_catalogue_read::resolve(db, receipt, &pins, SECRET, now).await
    };
    // The consistent receipt reaches the deliberately disconnected store.
    assert!(resolve(&mut db, &receipt).await.is_err());
    receipt.lease.expires_at -= 1;
    assert!(matches!(resolve(&mut db, &receipt).await, Ok(None)));
    receipt.lease.expires_at += 1;
    receipt.lease.continuation_deadline -= 1;
    assert!(matches!(resolve(&mut db, &receipt).await, Ok(None)));
}
