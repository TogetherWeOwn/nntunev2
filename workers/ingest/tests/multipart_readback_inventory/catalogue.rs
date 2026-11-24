use super::{id, Client, ReadbackFixture, FINAL_VERSION};
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    catalogue_publication::{
        stage,
        PublicationError::{Indeterminate, Refused},
        PublishedObject, VerifiedOccurrence,
    },
    digest_candidate_store::CandidateIntent,
    track_writers::TrackRow,
};
use tokio_postgres::Transaction;

const RELEASE: &str = "ab310000-0000-4000-8000-000000000000";
const ROWS: &str = "SELECT t.track_no, s.source_job_id, s.object_id::text
    FROM public.tracks t LEFT JOIN public.catalogue_s3_sources s ON s.track_id = t.id
    WHERE t.release_id = $1::text::uuid ORDER BY t.track_no";

pub(super) async fn exercise(
    db: &Client,
    writer: &mut Client,
    contender: &mut Client,
    first: &ReadbackFixture,
    old_intent: &CandidateIntent,
) {
    let schema = concat!(
        include_str!("../../../api/tests/fixtures/storage_s3_locator_schema.sql"),
        include_str!("../fixtures/catalogue_source_schema.sql"),
        include_str!("../fixtures/catalogue_publication.sql")
    );
    db.batch_execute(schema).await.unwrap();

    let [root, environment, library, _, _, configuration] = first.context.identities;
    let old = PublishedObject {
        intent: old_intent,
        locator: first.media.metadata().locator(),
        plaintext_sha256: first.origin.plaintext_sha256(),
        root_version_id: root,
        page_versions: &[FINAL_VERSION],
    };
    let injected = [
        (environment, library, 0x76),
        (id(0x7f), id(0x80), 0x79),
        (environment, id(0x81), 0x7c),
    ]
    .map(|(environment, library, seed)| {
        let [object, generation, map_id] = [seed, seed + 1, seed + 2].map(id);
        let key = format!("media/{}/{}", hex::encode(object), hex::encode(generation));
        let locator = S3Locator::new(
            old.locator.bucket().into(),
            key,
            S3Binding::VersionId("new-media-pin".into()),
        )
        .unwrap();
        let intent = CandidateIntent {
            identities: [environment, library, object, generation, configuration],
            map_id,
            ..*old_intent
        };
        (intent, locator)
    });
    let [new, foreign, private] = injected
        .each_ref()
        .map(|(intent, locator)| PublishedObject {
            intent,
            locator,
            plaintext_sha256: &[0xfd; 32],
            page_versions: &["new-page-pin"],
            ..old
        });

    let occurrence = |job_id, object_id, track_no| VerifiedOccurrence {
        job_id,
        owner_id: "catalogue-owner",
        object_id,
        track: TrackRow {
            release_id: RELEASE,
            title: "Synthetic track",
            medium_no: 1,
            track_no,
            mbid: RELEASE,
            file_ordinal: 0,
        },
    };
    let old_object = old_intent.identities[2];
    let first_match = occurrence("catalogue-old", old_object, 1);
    let object = new.intent.identities[2];
    let next_match = occurrence("catalogue-new", object, 1);

    let mut tx = writer.transaction().await.unwrap();
    let bad_pin = PublishedObject {
        page_versions: &["stale-page-version"],
        ..old
    };
    let wrong_locator = PublishedObject {
        locator: new.locator,
        ..old
    };
    let mut foreign_owner = occurrence("catalogue-old", old_object, 1);
    foreign_owner.owner_id = "catalogue-other";
    for (matched, receipt) in [
        (&first_match, &bad_pin),
        (&first_match, &wrong_locator),
        (&first_match, &new),
        (
            &occurrence("catalogue-new", private.intent.identities[2], 1),
            &private,
        ),
        (&foreign_owner, &old),
        (&occurrence("catalogue-queued", old_object, 1), &old),
    ] {
        refuse(&mut tx, matched, receipt).await;
    }
    tx.batch_execute(
        "UPDATE public.users SET deleting_at = clock_timestamp() WHERE id = 'catalogue-owner'",
    )
    .await
    .unwrap();
    refuse(&mut tx, &first_match, &old).await;
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    tx.batch_execute("ALTER TABLE public.catalogue_s3_sources ADD CHECK (accepted_seq < 0)")
        .await
        .unwrap();
    assert_eq!(stage(&mut tx, &first_match, &old).await, Err(Indeterminate));
    assert!(tx.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    stage(&mut tx, &first_match, &old).await.unwrap();
    assert!(db.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
    assert!(db.query(ROWS, &[&RELEASE]).await.unwrap().is_empty());

    let mut tx = writer.transaction().await.unwrap();
    let track_id = stage(&mut tx, &first_match, &old).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &first_match, &old).await.unwrap(), track_id);
    let conflicting = occurrence("catalogue-old", object, 1);
    refuse(&mut tx, &conflicting, &new).await;
    let cross_scope = occurrence("catalogue-new", foreign.intent.identities[2], 1);
    refuse(&mut tx, &cross_scope, &foreign).await;
    assert_eq!(stage(&mut tx, &next_match, &new).await.unwrap(), track_id);
    refuse(&mut tx, &first_match, &old).await;
    tx.commit().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    let second_match = occurrence("catalogue-new", object, 2);
    assert_ne!(stage(&mut tx, &second_match, &new).await.unwrap(), track_id);
    let mut competing = contender.transaction().await.unwrap();
    let pid: i32 = competing
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let late = occurrence("catalogue-old", old_object, 2);
    let (outcome, ()) = tokio::join!(stage(&mut competing, &late, &old), async {
        db.execute("SELECT pg_temp.wait_for_catalogue_lock($1)", &[&pid])
            .await
            .unwrap();
        assert_eq!(db.query(ROWS, &[&RELEASE]).await.unwrap().len(), 1);
        tx.commit().await.unwrap();
    });
    assert_eq!(outcome, Err(Refused));
    competing.commit().await.unwrap();
    let rows = db.query(ROWS, &[&RELEASE]).await.unwrap();
    assert_eq!(rows.len(), 2);
    let object = hex::encode(object);
    for row in rows {
        assert_eq!(row.get::<_, &str>(1), "catalogue-new");
        assert_eq!(row.get::<_, String>(2).replace('-', ""), object);
    }

    for edit in [
        "quarantined = true",
        "deleting_at = now()",
        "retain_until = '-infinity'",
        "delete_until = 'infinity'",
    ] {
        let mut tx = writer.transaction().await.unwrap();
        let sql = format!("UPDATE public.storage_validated_digest_winners SET {edit} WHERE object_id = $1::text::uuid");
        tx.execute(&sql, &[&object]).await.unwrap();
        refuse(&mut tx, &next_match, &new).await;
        tx.rollback().await.unwrap();
    }
}

async fn refuse(
    tx: &mut Transaction<'_>,
    matched: &VerifiedOccurrence<'_>,
    receipt: &PublishedObject<'_>,
) {
    assert_eq!(stage(tx, matched, receipt).await, Err(Refused));
}
