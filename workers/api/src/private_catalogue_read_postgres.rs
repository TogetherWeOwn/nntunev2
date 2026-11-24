use super::*;
use crate::{
    play_grant::IssuedGrant,
    playback::{LeaseAcquire, LeaseRow},
    storage_digest_read::protect_digest_read,
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

fn receipt(track_id: &str, digest: &str, exp: u64) -> BrowserReceipt {
    let payload = format!("{MEMBER}|{track_id}|{PLAY}|1|{exp}|{digest}");
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
            epoch: 1,
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
    let signed = receipt(track, digest, exp);
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

    let query = async |db: &mut Client, receipt: &BrowserReceipt, pins: &PublicationPins<'_>| {
        resolve(db, receipt, pins, SECRET, SystemTime::now)
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
    let wrong_digest = receipt(track, &"00".repeat(32), exp);
    assert!(query(&mut reader, &wrong_digest, &pins).await.is_none());
    let mut unsigned = signed.clone();
    unsigned.grant.token.push('A');
    assert!(query(&mut reader, &unsigned, &pins).await.is_none());

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
        query(&mut reader, &receipt(same_media_track, digest, exp), &pins)
            .await
            .is_none(),
        "shared bytes cannot supply another track's reference"
    );
    assert_eq!(acquire(&writer).await, LeaseAcquire::Acquired { epoch: 2 });
    assert!(query(&mut reader, &signed, &pins).await.is_none());
}
