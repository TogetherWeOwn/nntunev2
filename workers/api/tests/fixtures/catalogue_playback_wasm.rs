use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::s3_locator::S3Binding;
use wasm_bindgen::prelude::*;

use crate::{
    postgres::PgClient,
    private_catalogue_read::{lookup_scoped, PublicationScope},
    private_listening::PrivateListening,
    private_playback::{
        browser::{BrowserScope, BrowserStart},
        PrivatePlayback,
    },
    session::{issue_session, verify_session_dual, SessionKeys},
    storage_digest_read::custody::PlaybackRoot,
    worker_entry::private_browser_custody::project,
};

#[path = "../../../ingest/tests/fixtures/catalogue_postgres_wasm.rs"]
mod postgres;

// This is deployment scope, supplied independently of the upload receipt.
fn id(value: u8) -> [u8; 16] {
    let mut bytes = [value; 16];
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    bytes
}

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(worker::Date::now().as_millis())
}

#[wasm_bindgen]
pub async fn resolve_uploaded_playback(track: String, grant_key: String) -> String {
    let db = postgres::connect().await;
    let scope = BrowserScope {
        publication: PublicationScope {
            environment: id(1),
            library: id(2),
            provider_configuration: id(7),
            bucket: "fixture-bucket",
        },
        root_version: id(0),
    };
    let accepted_seq = db
        .query_one(
            "SELECT accepted_seq FROM public.ingest_jobs WHERE id = 'catalogue-old'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let time = now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let deadline = UNIX_EPOCH + Duration::from_secs(time + 120);
    assert!(db
        .query_one(
            include_str!("private_catalogue_attach.sql"),
            &[&track, &deadline]
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    const MEMBER: &str = "catalogue-other";
    let keys = SessionKeys::single(&[24; 32]);
    let cookie = issue_session(keys.primary, MEMBER, time, 120).unwrap();
    let claims = verify_session_dual(&keys, &cookie, time).unwrap();
    let invite: i64 = db
        .query_one(
            include_str!("private_playback_session.sql"),
            &[
                &claims.session_id,
                &MEMBER,
                &(claims.created_at as i64),
                &(claims.expires_at as i64),
            ],
        )
        .await
        .unwrap()
        .get(0);
    let policy = PrivateListening::parse(&serde_json::json!({
        "owner_user_id": "catalogue-owner", "invite_ids": [invite], "expires_at": claims.expires_at
    }).to_string()).unwrap();

    let mut db = PgClient::new(db);
    let (published, row) = lookup_scoped(&db, &track, scope.publication, None, None)
        .await
        .unwrap()
        .expect("committed catalogue source");
    assert_eq!(row.get::<_, i64>("accepted_seq"), accepted_seq);
    let S3Binding::VersionId(version) = published.locator.binding() else {
        panic!("catalogue lookup must return a version");
    };
    let start = BrowserStart {
        track_id: track,
        session_id: "catalogue-tab".into(),
        play_id: None,
        request_id: None,
    };
    let service = PrivatePlayback {
        policy: Some(&policy),
        grant_secret: grant_key.as_bytes(),
    };
    let receipt = service
        .open_browser(&mut db, &claims, &start, scope, now)
        .await
        .unwrap()
        .expect("issued private playback receipt");
    // The uploaded fixture contains 19 samples at 44.1 kHz.
    let relay = receipt.relay();
    assert_eq!(relay.duration_ms.get(), 1);
    let bearer = &relay.grant.token;
    let loaded = service
        .load_for_stream(&db, bearer, scope, now)
        .await
        .unwrap()
        .expect("committed browser receipt");
    let playback = service
        .resolve(
            &mut db,
            &loaded,
            bearer,
            &PlaybackRoot {
                version: scope.root_version,
                secret: &[17; 32],
            },
            now,
        )
        .await
        .unwrap()
        .expect("protected private playback");
    assert_eq!(playback.media.locator(), &published.locator);
    let snapshot = project(&loaded, playback, bearer).unwrap();
    assert_eq!(&snapshot.media_version, version);
    serde_json::json!({
        "bearer": bearer,
        "play": relay.lease.play_id,
        "snapshot": snapshot,
    })
    .to_string()
}
