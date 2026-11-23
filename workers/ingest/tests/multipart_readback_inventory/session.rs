//! Qualifies the off-route seam against the pinned, unmounted session SQL.
//! The lock below is fixture-only. The draft has no completion outcome writer.

use super::{
    id,
    publisher::{connect, seed_candidate, INVENTORY, RETAIN_UNTIL, WINNERS},
    ReadbackFixture, BUCKET, CONFIGURATION, FINAL_VERSION, FLAC, SCOPE, SHARED_VERSION,
};
use nntune_core::s3_locator::{S3Binding, S3Locator};
use nntune_ingest::{
    digest_candidate_store::stage_completed_candidate, multipart::head_receipt::HeadReceipt,
};
use std::{cell::OnceCell, fs, path::PathBuf};
use tokio_postgres::{types::ToSql, Client, GenericClient, Transaction};

const OWNER: &str = "completion-fixture-owner";
const FINGERPRINT: &[u8] = &[0x80; 32];
const REQUEST: &str = "completion-fixture-request";

const LOCK_COMPLETION: &str = "WITH locked AS MATERIALIZED (
    SELECT s.id, s.idle_deadline, s.absolute_deadline
    FROM public.ingest_session_reservations s
    JOIN public.storage_library_namespaces l ON l.id = s.library_id
    WHERE l.environment_id = $1::text::uuid AND s.library_id = $2::text::uuid
      AND s.principal_id = $3 AND s.id = $4::text::uuid AND s.fingerprint = $5
      AND s.checkpoint_generation = $6::text::uuid AND s.checkpoint_revision = $7
      AND s.checkpoint_offset = $8 AND s.write_lease_id = $9::text::uuid
      AND s.write_lease_generation = $6::text::uuid AND s.terminal_reason IS NULL
    FOR UPDATE OF s
)
SELECT id::text FROM locked
WHERE pg_catalog.clock_timestamp() < LEAST(idle_deadline, absolute_deadline)";

async fn lock_completion(tx: &Transaction<'_>, params: &[&(dyn ToSql + Sync)]) -> Option<String> {
    tx.query_opt(LOCK_COMPLETION, params)
        .await
        .unwrap()
        .map(|row| row.get(0))
}

async fn install_draft_sql(db: &Client) {
    let root = PathBuf::from(std::env::var_os("NNTUNE_TEST_SESSION_SQL_DIR").unwrap());
    let sql = |name| fs::read_to_string(root.join(format!("ingest_session_{name}.sql"))).unwrap();
    db.batch_execute(&sql("schema")).await.unwrap();
    for (file, signature, returns) in [
        (
            "reserve",
            "uuid, uuid, text, text, bytea, uuid",
            "SETOF uuid",
        ),
        (
            "acquire_write_lease",
            "uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, uuid",
            "SETOF uuid",
        ),
        (
            "advance_checkpoint",
            "uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, bigint, uuid",
            "SETOF bigint",
        ),
        (
            "status",
            "uuid, uuid, text, uuid",
            "TABLE (state text, expires_at timestamptz)",
        ),
    ] {
        let body = sql(file);
        db.batch_execute(&format!(
            "CREATE FUNCTION pg_temp.{file}({signature}) RETURNS {returns}
             LANGUAGE sql VOLATILE AS $fixture${body}$fixture$;"
        ))
        .await
        .unwrap();
    }
}

async fn assert_publication(
    db: &impl GenericClient,
    map: &str,
    scope: &[&(dyn ToSql + Sync)],
    object: Option<&str>,
) {
    let pins = db.query_opt(INVENTORY, &[&map]).await.unwrap();
    let winner = db.query_opt(WINNERS, scope).await.unwrap();
    if let Some(object) = object {
        let pins = pins.unwrap();
        assert_eq!(pins.get::<_, &str>(0), SHARED_VERSION);
        assert_eq!(pins.get::<_, Vec<String>>(1), [FINAL_VERSION]);
        let winner = winner.unwrap();
        assert_eq!(winner.get::<_, &str>(0).replace('-', ""), object);
    } else {
        assert!(pins.is_none());
        assert!(winner.is_none());
    }
}

#[tokio::test]
#[ignore = "requires a migrated disposable PostgreSQL database and pinned draft SQL"]
async fn postgres_scoped_completion_exposes_missing_retry_outcome() {
    let observer = connect().await;
    let mut writer = connect().await;
    install_draft_sql(&observer).await;
    let identities = [0x80, 0x81, 0x82, 0x83, 0x84, 5].map(id);
    let [root, environment, library, object, generation, _] = identities.map(hex::encode);
    let map_id = id(0x85);
    let map = hex::encode(map_id);
    let session = hex::encode(id(0x86));
    let lease = hex::encode(id(0x87));
    let first_lease = hex::encode(id(0x88));
    let proposed_retry = hex::encode(id(0x89));
    let scope = [&environment as &(dyn ToSql + Sync), &library];
    let scoped_session = [scope[0], scope[1], &OWNER, &session];
    let length = FLAC.len() as i64;
    let complete = [
        scope[0],
        scope[1],
        &OWNER,
        &session,
        &FINGERPRINT,
        &generation,
        &1_i64,
        &length,
        &lease,
    ];
    observer.batch_execute(&format!(
        "INSERT INTO public.users (id, email, created_at) VALUES ('{OWNER}', '{OWNER}@example.invalid', 1);
         INSERT INTO public.storage_environments VALUES ('{environment}');
         INSERT INTO public.storage_library_namespaces VALUES ('{library}', '{environment}', '{OWNER}');"
    )).await.unwrap();
    let reserve = async |proposed: &str| {
        let row = observer.query_one(
            "SELECT pg_temp.reserve($1::text::uuid, $2::text::uuid, $3, $4, $5, $6::text::uuid)::text",
            &[&environment, &library, &OWNER, &REQUEST, &FINGERPRINT, &proposed],
        ).await.unwrap();
        assert_eq!(row.get::<_, &str>(0).replace('-', ""), session);
    };
    let acquire = async |revision, offset, lease_id: &str| {
        let row = observer.query_one(
            "SELECT pg_temp.acquire_write_lease($1::text::uuid, $2::text::uuid, $3, $4::text::uuid,
                $5, $6::text::uuid, $7, $8, $9::text::uuid)::text",
            &[scope[0], scope[1], &OWNER, &session, &FINGERPRINT, &generation, &revision, &offset, &lease_id],
        ).await.unwrap();
        assert_eq!(row.get::<_, &str>(0).replace('-', ""), lease_id);
    };
    reserve(&session).await;
    acquire(0_i64, 0_i64, &first_lease).await;
    let row = observer
        .query_one(
            "SELECT pg_temp.advance_checkpoint($1::text::uuid, $2::text::uuid, $3, $4::text::uuid,
            $5, $6::text::uuid, 0, 0, $7, $8::text::uuid)",
            &[
                scope[0],
                scope[1],
                &OWNER,
                &session,
                &FINGERPRINT,
                &generation,
                &length,
                &first_lease,
            ],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    acquire(1_i64, length, &lease).await;

    let key = format!("media/{object}/{generation}");
    let page_key = format!("media-digests/{object}/{generation}/{map}/00000000");
    let sealed = OnceCell::new();
    let stage = async |tx: &mut Transaction<'_>, params: &[&(dyn ToSql + Sync)]| {
        let Some(bound_session) = lock_completion(tx, params).await else {
            return false;
        };
        assert_eq!(bound_session.replace('-', ""), session);
        // Replay uses the same ciphertext, envelope and media receipt.
        let fixture = sealed.get_or_init(|| {
            ReadbackFixture::with_session(FLAC, identities, map_id, &key, Some(&bound_session))
        });
        let observed = S3Locator::new(
            BUCKET.into(),
            key.clone(),
            S3Binding::VersionId(SHARED_VERSION.into()),
        )
        .unwrap();
        fixture
            .media
            .verify_head(
                &fixture.context,
                Some(HeadReceipt {
                    configuration: Some(CONFIGURATION),
                    locator: Some(&observed),
                    content_length: Some(fixture.media_bytes.len() as u64),
                }),
            )
            .unwrap();
        let sidecars = [fixture.sidecar_at(0, &page_key, FINAL_VERSION)];
        let inventory = fixture.inventory(&sidecars);
        let verified = inventory
            .verify(SCOPE, fixture.envelope(), async |bucket, pin| {
                assert_eq!(bucket, BUCKET);
                Ok(fixture.response_with_keys(pin, &key, &page_key))
            })
            .await
            .unwrap();
        let intent = seed_candidate(tx, fixture).await;
        tx.execute(
            "INSERT INTO public.storage_object_envelope_intents VALUES ($1::text::uuid, $2::text::uuid, $3)",
            &[&object, &root, &fixture.record.as_slice()],
        ).await.unwrap();
        stage_completed_candidate(tx, &intent, SCOPE, &verified, RETAIN_UNTIL)
            .await
            .unwrap();
        assert_eq!(
            lock_completion(tx, params).await.as_deref(),
            Some(bound_session.as_str())
        );
        assert_publication(tx, &map, &scope, Some(&object)).await;
        assert_publication(&observer, &map, &scope, None).await;
        true
    };
    let status = async || {
        let row = observer.query_one(
            "SELECT state, expires_at::text FROM pg_temp.status($1::text::uuid, $2::text::uuid, $3, $4::text::uuid)",
            &scoped_session,
        ).await.unwrap();
        assert_eq!(row.get::<_, &str>(0), "active");
        row.get::<_, String>(1)
    };

    let mut tx = writer.transaction().await.unwrap();
    for (index, wrong) in [
        (0, &library as &(dyn ToSql + Sync)),
        (1, &environment),
        (2, &"foreign-owner"),
        (3, &proposed_retry),
        (4, &&[0xff_u8; 32][..]),
        (5, &object),
        (6, &0_i64),
        (7, &0_i64),
        (8, &first_lease),
    ] {
        let mut mismatch = complete;
        mismatch[index] = wrong;
        assert!(!stage(&mut tx, &mismatch).await);
    }
    tx.execute(
        "UPDATE public.ingest_session_reservations SET idle_deadline = '-infinity' WHERE id = $1::text::uuid",
        &[&session],
    ).await.unwrap();
    assert!(!stage(&mut tx, &complete).await);
    assert!(sealed.get().is_none());
    assert_publication(&tx, &map, &scope, None).await;
    tx.rollback().await.unwrap();

    let mut tx = writer.transaction().await.unwrap();
    assert!(stage(&mut tx, &complete).await);
    tx.rollback().await.unwrap();
    assert_publication(&observer, &map, &scope, None).await;
    reserve(&proposed_retry).await;
    let rolled_back_status = status().await;

    let mut tx = writer.transaction().await.unwrap();
    assert!(stage(&mut tx, &complete).await);
    // Deliberately commit the storage seam in the sandbox to model a lost
    // acknowledgement. The real caller would need a scoped outcome in this tx.
    tx.commit().await.unwrap();
    assert_publication(&observer, &map, &scope, Some(&object)).await;
    reserve(&proposed_retry).await;
    assert_eq!(status().await, rolled_back_status);
    let tx = writer.transaction().await.unwrap();
    assert!(lock_completion(&tx, &complete).await.is_some());
    tx.rollback().await.unwrap();
}
