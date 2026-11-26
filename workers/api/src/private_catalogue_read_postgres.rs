use super::*;
use crate::{
    play_grant::IssuedGrant,
    playback::{LeaseAcquire, LeaseRow},
    storage_digest_read::{custody, protect_digest_read},
    worker_entry::acquire_playback_lease_on,
};
use hmac::{Hmac, KeyInit, Mac};
use tokio_postgres::{Client, NoTls};

const DB: &str = "postgresql://postgres:ci-throwaway@127.0.0.1:5432/nntune_catalogue_ci";
const MEMBER: &str = "catalogue-other";
const PLAY: &str = "private_1234567890abcdefghijkl";
const SECRET: &[u8] = b"synthetic catalogue playback test key";

async fn connect() -> Client {
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(DB),
        "disposable GitHub-hosted catalogue fixture database required"
    );
    let (client, connection) = tokio_postgres::connect(DB, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '10s'")
        .await
        .unwrap();
    client
}

fn receipt(track_id: &str, digest: &str, epoch: u64, exp: u64) -> BrowserReceipt {
    let payload = format!("{MEMBER}|{track_id}|{PLAY}|{epoch}|{exp}|{digest}");
    let signed = format!("v4.{}", crate::b64::encode(payload.as_bytes()));
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(signed.as_bytes());
    let token = format!(
        "{signed}.{}",
        crate::b64::encode(&mac.finalize().into_bytes())
    );
    BrowserReceipt {
        user_id: MEMBER.into(),
        session_binding: "synthetic-session".into(),
        lease: LeaseRow {
            session_id: "catalogue-tab".into(),
            track_id: track_id.into(),
            play_id: PLAY.into(),
            epoch,
            expires_at: exp,
            continuation_deadline: exp,
        },
        artifact_sha256: digest.into(),
        duration_ms: 60_000.try_into().unwrap(),
        grant: IssuedGrant {
            token,
            exp_unix: exp,
        },
    }
}

#[tokio::test]
async fn resolves_only_the_committed_scoped_publication() {
    let mut reader = connect().await;
    let mut writer = connect().await;
    // The preceding ingest regression commits two canonical tracks sharing the
    // winning object and revision. Neither has a playback reference yet.
    let rows = writer
        .query(
            include_str!("../tests/fixtures/private_catalogue_read.sql"),
            &[],
        )
        .await
        .unwrap();
    let [first, second] = rows.as_slice() else {
        panic!("expected the committed producer fixture")
    };
    let id = |column| first.get::<_, ReadUuid>(column).0;
    let track: &str = first.get("track");
    let same_media_track: &str = second.get("track");
    assert_eq!(id("object_id"), second.get::<_, ReadUuid>("object_id").0);
    let digest: &str = first.get("digest");
    let pins = PublicationPins {
        environment: id("environment_id"),
        library: id("library_id"),
        provider_configuration: id("provider_configuration_version_id"),
        accepted_seq: first.get("accepted_seq"),
        bucket: "test-bucket",
        version: "new-media-pin",
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let exp = now.as_secs() + crate::playback::LEASE_TTL_SECS;
    let deadline = UNIX_EPOCH + Duration::from_secs(exp);
    let signed = receipt(track, digest, 1, exp);
    let acquire = async |db: &Client| {
        acquire_playback_lease_on(
            db,
            MEMBER,
            &signed.lease.session_id,
            track,
            PLAY,
            now.as_secs(),
            exp,
            false,
        )
        .await
        .unwrap()
    };
    assert_eq!(acquire(&writer).await, LeaseAcquire::Acquired { epoch: 1 });

    // Keep grant time fixed so expiry cannot mask an authority check.
    let query = async |db: &mut Client, receipt: &BrowserReceipt, pins: &PublicationPins<'_>| {
        resolve(db, receipt, pins, SECRET, || UNIX_EPOCH + now)
            .await
            .unwrap()
    };
    assert!(query(&mut reader, &signed, &pins).await.is_none());
    let publication = writer.transaction().await.unwrap();
    assert!(publication
        .query_one(
            include_str!("../tests/fixtures/private_catalogue_attach.sql"),
            &[&track, &deadline],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    assert!(query(&mut reader, &signed, &pins).await.is_none());
    publication.commit().await.unwrap();

    assert!(pins.accepted_seq > 1, "need a positive stale revision");
    let mismatches: [fn(&mut PublicationPins<'_>); 6] = [
        |pins| pins.environment = [0xff; 16],
        |pins| pins.library = [0xff; 16],
        |pins| pins.accepted_seq -= 1,
        |pins| pins.provider_configuration = [0xff; 16],
        |pins| pins.bucket = "other-bucket",
        |pins| pins.version = "other-version",
    ];
    for change in mismatches {
        let mut refused = PublicationPins { ..pins };
        change(&mut refused);
        assert!(query(&mut reader, &signed, &refused).await.is_none());
    }
    let wrong_digest = receipt(track, &"00".repeat(32), 1, exp);
    assert!(query(&mut reader, &wrong_digest, &pins).await.is_none());
    let mut unsigned = signed.clone();
    unsigned.grant.token.push('A');
    assert!(query(&mut reader, &unsigned, &pins).await.is_none());

    let (current, row) = lookup_scoped(&reader, track, (&pins).into(), None, None)
        .await
        .unwrap()
        .expect("current committed publication");
    assert_eq!(row.get::<_, i64>("accepted_seq"), pins.accepted_seq);
    assert_eq!(current.target.reference, id("track_id"));
    assert!(matches!(current.locator.binding(), S3Binding::VersionId(v) if v == pins.version));

    let resolved = query(&mut reader, &signed, &pins)
        .await
        .expect("committed publication");
    assert_eq!(resolved.target.reference, id("track_id"));
    assert_eq!(resolved.target.object, id("object_id"));
    assert!(matches!(resolved.locator.binding(), S3Binding::VersionId(v) if v == pins.version));
    let read = protect_digest_read(
        &mut reader,
        resolved.target,
        deadline,
        async |_| Some(resolved.grant_deadline),
        SystemTime::now,
    )
    .await
    .unwrap()
    .expect("resolved target supports committed read protection");
    assert_eq!(read.pins().media_version, pins.version);

    writer
        .execute(
            "UPDATE public.playback_leases SET track_id = $2 WHERE user_id = $1",
            &[&MEMBER, &same_media_track],
        )
        .await
        .unwrap();
    assert!(query(&mut reader, &signed, &pins).await.is_none());
    assert!(
        query(
            &mut reader,
            &receipt(same_media_track, digest, 1, exp),
            &pins
        )
        .await
        .is_none(),
        "shared bytes cannot supply another track's reference"
    );
    assert_eq!(acquire(&writer).await, LeaseAcquire::Acquired { epoch: 2 });
    assert!(query(&mut reader, &signed, &pins).await.is_none());
    let renewed = receipt(track, digest, 2, exp);
    assert!(query(&mut reader, &renewed, &pins).await.is_some());

    // Exercise custody with the original authenticated publisher fixture.
    let original_row = writer
        .query_one(
            include_str!("../tests/fixtures/private_catalogue_custody.sql"),
            &[&track],
        )
        .await
        .unwrap();
    let original = receipt(track, original_row.get(1), 2, exp);
    let original_pins = PublicationPins {
        accepted_seq: original_row.get(0),
        version: "Opaque+%2F/=",
        ..pins
    };
    assert!(writer
        .query_one(
            include_str!("../tests/fixtures/private_catalogue_attach.sql"),
            &[&track, &deadline]
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    let root = custody::PlaybackRoot {
        version: id("root_version_id"),
        secret: &[17; 32],
    };
    let load = async |db: &mut Client, bucket: &str, secret: &[u8; 32]| {
        let resolved = query(db, &original, &original_pins).await.unwrap();
        let locator = S3Locator::new(
            bucket.into(),
            resolved.locator.key().into(),
            S3Binding::VersionId(original_pins.version.into()),
        )
        .unwrap();
        let read = protect_digest_read(
            db,
            resolved.target,
            deadline,
            async |_| Some(deadline),
            SystemTime::now,
        )
        .await
        .unwrap()
        .unwrap();
        custody::resolve_custody(
            db,
            read,
            &locator,
            &custody::PlaybackRoot { secret, ..root },
            SystemTime::now,
        )
        .await
        .unwrap()
    };
    assert!(load(&mut reader, "other-bucket", root.secret)
        .await
        .is_none());
    assert!(load(&mut reader, original_pins.bucket, &[18; 32])
        .await
        .is_none());
    let resolved = load(&mut reader, original_pins.bucket, root.secret)
        .await
        .unwrap();
    assert_eq!(resolved.read.pins().media_version, original_pins.version);
    let manifest = nntune_core::digest_enrollment::PlaybackManifest::Stored(&resolved.manifest);
    assert!(manifest.matches(
        &resolved.read.playback_snapshot().unwrap(),
        &resolved.context
    ));
    use crate::{
        private_listening::PrivateListening, private_playback::PrivatePlayback, session::*,
    };

    let keys = SessionKeys::single(&[24; 32]);
    let issued_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = issue_session(keys.primary, MEMBER, issued_at, 600).unwrap();
    let claims = verify_session_dual(&keys, &cookie, issued_at).unwrap();
    let invite: i64 = writer
        .query_one(
            include_str!("../tests/fixtures/private_playback_session.sql"),
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
    writer
        .batch_execute(include_str!(
            "../tests/fixtures/private_playback_transitions.sql"
        ))
        .await
        .unwrap();
    let policy = PrivateListening::parse(&serde_json::json!({
        "owner_user_id": "catalogue-owner", "invite_ids": [invite], "expires_at": claims.expires_at
    }).to_string()).unwrap();
    let service = |policy| PrivatePlayback {
        policy,
        grant_secret: SECRET,
    };
    let issue = async |db: &mut Client, admission, cookie, pins: &PublicationPins<'_>| {
        service(admission)
            .issue(
                db,
                &keys,
                cookie,
                track,
                &signed.lease.session_id,
                pins,
                SystemTime::now,
            )
            .await
            .unwrap()
    };
    let configured = Some(&policy);
    let session = Some(cookie.as_str());
    for (admission, cookie, requested) in [
        (configured, None, &original_pins),
        (configured, session, &pins),
        (configured, session, &original_pins),
    ] {
        assert!(issue(&mut reader, admission, cookie, requested)
            .await
            .is_none());
    }
    let duration = include_str!("../tests/fixtures/private_playback_duration.sql");
    writer.query_one(duration, &[&track]).await.unwrap();
    assert!(issue(&mut reader, None, session, &original_pins)
        .await
        .is_none());
    let issued = issue(&mut reader, configured, session, &original_pins)
        .await
        .unwrap();
    let token = &issued.relay().grant.token;
    let resolve = async |db: &mut Client, admission, token: &str, time| {
        service(admission)
            .resolve(db, &issued, token, &root, || time)
            .await
            .unwrap()
    };
    let current = SystemTime::now();
    let expiry = UNIX_EPOCH + Duration::from_secs(issued.relay().grant.exp_unix);
    let failed_transaction = reader.batch_execute("BEGIN; SELECT 1 / 0");
    assert!(failed_transaction.await.is_err());
    for (admission, bearer, time) in [
        (configured, "invalid-bearer", current),
        (None, token.as_str(), current),
        (configured, token.as_str(), expiry),
    ] {
        assert!(resolve(&mut reader, admission, bearer, time)
            .await
            .is_none());
    }
    reader.batch_execute("ROLLBACK").await.unwrap();
    let resolved = resolve(&mut reader, configured, token, current)
        .await
        .unwrap();
    assert_eq!(resolved.read.pins().media_version, original_pins.version);
    browser_cases::receipt_retries(
        &mut reader,
        &writer,
        &service(configured),
        &claims,
        &issued,
        &original_pins,
        root.version,
    )
    .await;
    for state in ["invite", "session", "rotation", "revision", "race", "lease"] {
        writer
            .query_one(
                "SELECT pg_temp.private_playback_state($1, $2, $3)",
                &[&state, &track, &original_pins.accepted_seq],
            )
            .await
            .unwrap();
        if matches!(state, "invite" | "session" | "rotation" | "race") {
            assert!(issue(&mut reader, configured, session, &original_pins)
                .await
                .is_none());
        }
        let playback = resolve(&mut reader, configured, token, current).await;
        assert_eq!(playback.is_some(), state == "race", "{state}");
    }
}

#[path = "../tests/fixtures/private_browser_receipt_postgres.rs"]
mod browser_cases;
