use super::{
    id, ReadbackFixture, BUCKET, CONFIGURATION, FINAL_VERSION, FLAC, SCOPE, SHARED_VERSION,
};
use nntune_ingest::{
    digest_candidate_store::{
        stage_completed_candidate, CandidateIntent,
        InventoryAppendError::{Identity, MissingOrFenced},
    },
    multipart::readback_inventory::ReadbackScope,
};
use std::time::Duration;
use tokio_postgres::{Client, NoTls};

const RETAIN_UNTIL: i64 = 2_000_000_000;
const INVENTORY: &str = "SELECT media_version, page_versions
    FROM public.storage_digest_inventory_intents WHERE map_id = $1::text::uuid";
const WINNERS: &str = "SELECT object_id::text FROM public.storage_validated_digest_winners
    WHERE environment_id = $1::text::uuid AND library_id = $2::text::uuid";

async fn connect() -> Client {
    const SCRATCH: &str = "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci";
    assert!(
        std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted")
            && std::env::var("NNTUNE_TEST_DATABASE_URL").as_deref() == Ok(SCRATCH),
        "disposable GitHub-hosted migration database required"
    );
    let (client, connection) = tokio_postgres::connect(SCRATCH, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '10s'; SET lock_timeout = '5s'")
        .await
        .unwrap();
    client
}

#[tokio::test]
#[ignore = "requires a migrated disposable PostgreSQL database"]
async fn postgres_stages_completed_candidate_atomically() {
    let observer = connect().await;
    let mut writer = connect().await;
    let mut contender = connect().await;
    let identities = [0x70, 0x71, 0x72, 0x73, 0x74, 5].map(id);
    let [root, environment, library, object, generation, provider] = identities.map(hex::encode);
    let map_id = id(0x75);
    let map = hex::encode(map_id);
    let key = format!("media/{object}/{generation}");
    let page_key = format!("media-digests/{object}/{generation}/{map}/00000000");
    let fixture = ReadbackFixture::in_scope(FLAC, identities, map_id, &key);
    let receipts = [fixture.sidecar_at(0, &page_key, FINAL_VERSION)];
    let inventory = fixture.inventory(&receipts);
    let verified = inventory
        .verify(SCOPE, fixture.envelope(), async |bucket, pin| {
            assert_eq!(bucket, BUCKET);
            Ok(fixture.response_with_keys(pin, &key, &page_key))
        })
        .await
        .unwrap();
    let intent = CandidateIntent {
        identities: identities[1..].try_into().unwrap(),
        plaintext_len: fixture.context.plaintext_len,
        map_id,
        header: fixture.context.header,
        reservation_generation: 1,
    };
    let length = intent.plaintext_len;
    let sealed = inventory.geometry().sealed_len();
    let header = hex::encode(intent.header);
    let digest = hex::encode(fixture.origin.plaintext_sha256());
    let hashes: Vec<_> = fixture
        .origin
        .candidate_page_hashes(&intent.identities, length, &map_id, &intent.header)
        .unwrap()
        .iter()
        .map(|hash| hash.to_vec())
        .collect();
    observer.batch_execute(&format!(
        "INSERT INTO public.storage_environments VALUES ('{environment}');
         INSERT INTO public.storage_library_namespaces VALUES ('{library}', '{environment}', NULL);
         INSERT INTO public.storage_object_intents VALUES ('{object}', '{generation}', '{environment}',
             '{library}', '{provider}', {length}, {sealed}, decode('{header}', 'hex'));
         INSERT INTO public.storage_plaintext_digest_claim_intents VALUES ('{object}', '{environment}',
             '{library}', {length}, decode('{digest}', 'hex'));
         SELECT public.reserve_storage_digest('{object}', 3600);"
    )).await.unwrap();
    observer
        .query_one(
            "SELECT public.store_storage_digest_candidate($1::text::uuid, $2::text::uuid,
            $3::text::uuid, $4::text::uuid, 1, $5)",
            &[&environment, &library, &object, &map, &hashes],
        )
        .await
        .unwrap();

    let stage = async |tx: &mut tokio_postgres::Transaction<'_>| {
        stage_completed_candidate(tx, &intent, SCOPE, &verified, RETAIN_UNTIL).await
    };
    let mut tx = writer.transaction().await.unwrap();
    // Append succeeds internally, but winner staging lacks its exact envelope.
    assert_eq!(stage(&mut tx).await, Err(MissingOrFenced));
    assert!(tx.query_opt(INVENTORY, &[&map]).await.unwrap().is_none());
    tx.commit().await.unwrap();

    observer.execute(
        "INSERT INTO public.storage_object_envelope_intents VALUES ($1::text::uuid, $2::text::uuid, $3)",
        &[&object, &root, &fixture.record.as_slice()],
    ).await.unwrap();
    let scope = ReadbackScope {
        bucket: "foreign-bucket",
        configuration: CONFIGURATION,
    };
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(
        stage_completed_candidate(&mut tx, &intent, scope, &verified, RETAIN_UNTIL).await,
        Err(Identity)
    );
    tx.execute(
        "UPDATE public.storage_digest_reservations SET expires_at = '-infinity'
        WHERE object_id = $1::text::uuid",
        &[&object],
    )
    .await
    .unwrap();
    assert_eq!(stage(&mut tx).await, Err(MissingOrFenced));
    assert!(tx.query_opt(INVENTORY, &[&map]).await.unwrap().is_none());
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    // Count an actual winner insert even when the staging savepoint rolls back.
    tx.batch_execute(
        "CREATE TEMP SEQUENCE retention_insert_attempt;
         CREATE FUNCTION pg_temp.wait_for_retention_expiry() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('pg_temp.retention_insert_attempt');
             PERFORM pg_catalog.pg_sleep(LEAST(4, GREATEST(0, EXTRACT(EPOCH FROM
                 NEW.retain_until - pg_catalog.clock_timestamp())))::double precision);
             RETURN NEW;
         END $$;
         CREATE TRIGGER wait_for_retention_expiry AFTER INSERT ON public.storage_validated_digest_winners
         FOR EACH ROW EXECUTE FUNCTION pg_temp.wait_for_retention_expiry();",
    ).await.unwrap();
    let deadline: i64 = tx
        .query_one(
            "SELECT floor(EXTRACT(EPOCH FROM pg_catalog.clock_timestamp()))::bigint + 3",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    for (deadline, attempted_insert) in [(0, false), (deadline, true)] {
        assert_eq!(
            stage_completed_candidate(&mut tx, &intent, SCOPE, &verified, deadline).await,
            Err(MissingOrFenced)
        );
        let row = tx
            .query_one(
                "SELECT is_called FROM pg_temp.retention_insert_attempt",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, bool>(0), attempted_insert);
    }
    assert!(tx.query_opt(INVENTORY, &[&map]).await.unwrap().is_none());
    assert!(tx
        .query_opt(WINNERS, &[&environment, &library])
        .await
        .unwrap()
        .is_none());
    tx.rollback().await.unwrap();

    // The underlying guard must reject mismatched pre-existing pins and preserve them.
    let mut tx = writer.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO public.storage_digest_inventory_intents VALUES ($1::text::uuid, 'other', $2)",
        &[&map, &&[FINAL_VERSION][..]],
    )
    .await
    .unwrap();
    assert_eq!(stage(&mut tx).await, Err(MissingOrFenced));
    let row = tx.query_one(INVENTORY, &[&map]).await.unwrap();
    assert_eq!(row.get::<_, &str>(0), "other");
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO public.storage_digest_inventory_intents VALUES ($1::text::uuid, $2, $3)",
        &[&map, &SHARED_VERSION, &&[FINAL_VERSION][..]],
    )
    .await
    .unwrap();
    stage(&mut tx).await.unwrap();
    assert!(tx.query_opt(INVENTORY, &[&map]).await.unwrap().is_some());
    assert!(tx
        .query_opt(WINNERS, &[&environment, &library])
        .await
        .unwrap()
        .is_some());
    tx.rollback().await.unwrap();
    assert!(observer
        .query_opt(INVENTORY, &[&map])
        .await
        .unwrap()
        .is_none());
    assert!(observer
        .query_opt(WINNERS, &[&environment, &library])
        .await
        .unwrap()
        .is_none());

    let mut tx = writer.transaction().await.unwrap();
    stage(&mut tx).await.unwrap();
    let mut competing = contender.transaction().await.unwrap();
    let pid: i32 = competing
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(stage(&mut competing), async {
            loop {
                let blocked: bool = observer
                    .query_one(
                        "SELECT cardinality(pg_catalog.pg_blocking_pids($1)) > 0",
                        &[&pid],
                    )
                    .await
                    .unwrap()
                    .get(0);
                if blocked {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(observer
                .query_opt(INVENTORY, &[&map])
                .await
                .unwrap()
                .is_none());
            assert!(observer
                .query_opt(WINNERS, &[&environment, &library])
                .await
                .unwrap()
                .is_none());
            tx.commit().await.unwrap();
        })
    })
    .await
    .expect("competing caller must wait for the digest lock");
    assert_eq!(result, Err(MissingOrFenced));
    competing.rollback().await.unwrap();

    let row = observer.query_one(INVENTORY, &[&map]).await.unwrap();
    assert_eq!(row.get::<_, &str>(0), SHARED_VERSION);
    assert_eq!(row.get::<_, Vec<String>>(1), [FINAL_VERSION]);
    let row = observer
        .query_one(WINNERS, &[&environment, &library])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0).replace('-', ""), object);
    assert!(observer
        .query_one(
            "SELECT retain_until = pg_catalog.to_timestamp($2) AND deleting_at IS NULL
         FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid",
            &[&object, &(RETAIN_UNTIL as f64)],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    let admitted: bool = observer.query_one(
        "SELECT public.admit_storage_digest_delete($1::text::uuid, $2::text::uuid, decode($3, 'hex'),
            $4, $5::text::uuid, $6::text::uuid, $7::text::uuid)",
        &[&environment, &library, &digest, &(length as i64), &object, &generation, &root],
    ).await.unwrap().get(0);
    assert!(!admitted);
}
