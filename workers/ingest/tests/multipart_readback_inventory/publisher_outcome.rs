use super::{
    id, publisher_fixture, BUCKET, FINAL_VERSION, INVENTORY, RETAIN_UNTIL, SCOPE, SHARED_VERSION,
    WINNERS,
};
use nntune_ingest::{
    digest_candidate_store::{
        outcome::{read_completion, stage_completion, OutcomeError::Refused},
        CandidateIntent,
    },
    multipart::{readback_inventory::VerifiedCompletedReadback, session_outcome::CompletionScope},
};
use tokio_postgres::{error::SqlState, Client, Transaction};

#[tokio::test]
#[ignore = "requires a migrated disposable PostgreSQL database"]
async fn postgres_commits_scoped_outcome_and_winner_together() {
    publisher_fixture(true).await;
}

pub(super) async fn prove(
    observer: &Client,
    writer: &mut Client,
    intent: &CandidateIntent,
    verified: &VerifiedCompletedReadback<'_, '_>,
) {
    let checkpoint = intent.plaintext_len as i64;
    let [environment, library, object, seal, configuration] = intent.identities.map(hex::encode);
    let map = hex::encode(intent.map_id);
    let sid = "99000000-0000-4000-8000-000000000001";
    let lease = "99000000-0000-4000-8000-000000000002";
    let first = "99000000-0000-4000-8000-000000000003";
    let owner = "synthetic-outcome-owner";
    let other_object = hex::encode(id(0xa0));
    let other_generation = hex::encode(id(0xa1));
    for (signature, returns, sql) in [
        (
            "reserve(uuid, uuid, text, text, bytea, uuid)",
            "uuid",
            include_str!("../../../api/src/ingest_session_reserve.sql"),
        ),
        (
            "lease(uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, uuid)",
            "uuid",
            include_str!("../../../api/src/ingest_session_acquire_write_lease.sql"),
        ),
        (
            "advance(uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, bigint, uuid)",
            "bigint",
            include_str!("../../../api/src/ingest_session_advance_checkpoint.sql"),
        ),
        (
            "terminate(uuid, uuid, text, uuid, text, bytea, uuid, bigint, bigint, uuid)",
            "text",
            include_str!("../../../api/src/ingest_session_terminate.sql"),
        ),
    ] {
        observer
            .batch_execute(&format!(
                "CREATE FUNCTION pg_temp.{signature} RETURNS SETOF {returns}
            LANGUAGE sql VOLATILE AS $fixture${sql}$fixture$;"
            ))
            .await
            .unwrap();
    }
    observer.batch_execute(&format!(
        "INSERT INTO public.users (id, email, created_at) VALUES ('{owner}', '{owner}@example.invalid', 1);
         INSERT INTO public.storage_object_intents
         SELECT '{other_object}', '{other_generation}', environment_id, library_id,
                provider_configuration_version_id, plaintext_bytes, envelope_bytes, header_tag
         FROM public.storage_object_intents WHERE object_id = '{object}';
         SELECT pg_temp.reserve('{environment}', '{library}', '{owner}', 'synthetic', decode(repeat('fa',32),'hex'), '{sid}');
         SELECT pg_temp.lease('{environment}', '{library}', '{owner}', '{sid}', decode(repeat('fa',32),'hex'), '{seal}', 0, 0, '{first}');
         SELECT pg_temp.advance('{environment}', '{library}', '{owner}', '{sid}', decode(repeat('fa',32),'hex'), '{seal}', 0, 0, {checkpoint}, '{first}');
         SELECT pg_temp.lease('{environment}', '{library}', '{owner}', '{sid}', decode(repeat('fa',32),'hex'), '{seal}', 1, {checkpoint}, '{lease}');"
    )).await.unwrap();
    let scope = CompletionScope {
        environment: intent.identities[0],
        library: intent.identities[1],
        principal: owner,
        session: sid,
        fingerprint: &[0xfa; 32],
        seal: intent.identities[3],
        revision: 1,
        offset: checkpoint,
    };
    let authorized = async |_: &Transaction<'_>, locked: &CompletionScope<'_>| {
        let result = observer.query_one(
            "SELECT 1 FROM public.ingest_session_reservations WHERE id = $1::text::uuid FOR UPDATE NOWAIT",
            &[&locked.session],
        ).await;
        assert_eq!(
            result.unwrap_err().code(),
            Some(&SqlState::LOCK_NOT_AVAILABLE)
        );
        Ok(RETAIN_UNTIL)
    };
    let stage = async |tx: &mut Transaction<'_>, scope: &CompletionScope<'_>, lease_id: &str| {
        stage_completion(tx, scope, lease_id, intent, SCOPE, verified, RETAIN_UNTIL).await
    };
    let absent = async || {
        let pins = observer.query_opt(INVENTORY, &[&map]).await.unwrap();
        let winner = observer
            .query_opt(WINNERS, &[&environment, &library])
            .await
            .unwrap();
        let active: bool = observer.query_one(
            "SELECT terminal_reason IS NULL AND completed_map_id IS NULL AND write_lease_id = $2::text::uuid
             FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
            &[&sid, &lease],
        ).await.unwrap().get(0);
        assert!(pins.is_none() && winner.is_none() && active);
    };

    let foreign = CompletionScope {
        principal: "foreign",
        ..scope
    };
    let conflict = CompletionScope {
        fingerprint: &[0xfb; 32],
        ..scope
    };
    let mut tx = writer.transaction().await.unwrap();
    for (rejected, lease_id) in [(&foreign, lease), (&conflict, lease), (&scope, first)] {
        assert_eq!(stage(&mut tx, rejected, lease_id).await, Err(Refused));
    }
    tx.rollback().await.unwrap();
    for update in [
        "idle_deadline = '-infinity'",
        "write_lease_id = NULL, write_lease_generation = NULL",
    ] {
        let mut tx = writer.transaction().await.unwrap();
        let sql = format!(
            "UPDATE public.ingest_session_reservations SET {update} WHERE id = $1::text::uuid"
        );
        tx.execute(&sql, &[&sid]).await.unwrap();
        assert_eq!(stage(&mut tx, &scope, lease).await, Err(Refused));
        tx.rollback().await.unwrap();
    }
    let mut tx = writer.transaction().await.unwrap();
    tx.execute(
        "UPDATE public.ingest_session_reservations SET multipart_object_id = $2::text::uuid,
         multipart_bucket = $3 WHERE id = $1::text::uuid",
        &[&sid, &object, &BUCKET],
    )
    .await
    .unwrap();
    assert_eq!(stage(&mut tx, &scope, lease).await, Err(Refused));
    absent().await;
    tx.commit().await.unwrap();
    for (claimed_object, claimed_bucket) in [(&object, "different-bucket"), (&other_object, BUCKET)]
    {
        let mut tx = writer.transaction().await.unwrap();
        tx.execute(
            "UPDATE public.ingest_session_reservations SET multipart_object_id = $2::text::uuid,
             multipart_bucket = $3, multipart_upload_id = 'fixture-upload'
             WHERE id = $1::text::uuid",
            &[&sid, &claimed_object, &claimed_bucket],
        )
        .await
        .unwrap();
        assert_eq!(stage(&mut tx, &scope, lease).await, Err(Refused));
        assert!(tx.query_opt(INVENTORY, &[&map]).await.unwrap().is_none());
        assert!(tx
            .query_opt(WINNERS, &[&environment, &library])
            .await
            .unwrap()
            .is_none());
        tx.rollback().await.unwrap();
    }
    observer
        .execute(
            "UPDATE public.ingest_session_reservations SET multipart_upload_id = 'fixture-upload'
         WHERE id = $1::text::uuid",
            &[&sid],
        )
        .await
        .unwrap();

    let mut tx = writer.transaction().await.unwrap();
    stage(&mut tx, &scope, lease).await.unwrap();
    absent().await;
    tx.rollback().await.unwrap();
    absent().await;
    assert!(read_completion(writer, &scope, authorized)
        .await
        .unwrap()
        .is_none());

    let mut tx = writer.transaction().await.unwrap();
    stage(&mut tx, &scope, lease).await.unwrap();
    tx.commit().await.unwrap();
    let row = observer.query_one(
        "UPDATE public.ingest_session_reservations SET idle_deadline = '-infinity' WHERE id = $1::text::uuid
         RETURNING write_lease_id IS NULL AND terminal_reason = 'completed'",
        &[&sid],
    ).await.unwrap();
    assert!(row.get::<_, bool>(0));
    for action in ["abort", "expire"] {
        let sql = format!("SELECT pg_temp.terminate('{environment}', '{library}', '{owner}', '{sid}', '{action}', NULL, NULL, NULL, NULL, NULL)");
        assert!(observer.query_opt(&sql, &[]).await.unwrap().is_none());
    }
    let outcome = read_completion(writer, &scope, authorized)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.map.replace('-', ""), map);
    assert_eq!(outcome.object.replace('-', ""), object);
    assert_eq!(outcome.generation.replace('-', ""), seal);
    assert_eq!(outcome.configuration.replace('-', ""), configuration);
    assert_eq!(outcome.root_version.replace('-', ""), hex::encode(id(0x90)));
    assert_eq!(outcome.media_version, SHARED_VERSION);
    assert_eq!(outcome.page_versions, [FINAL_VERSION]);
    for refused in [&foreign, &conflict] {
        assert!(matches!(
            read_completion(writer, refused, authorized).await,
            Err(Refused)
        ));
    }
    for authorization in [Err(Refused), Ok(0)] {
        let result = read_completion(writer, &scope, async |_, _| authorization).await;
        assert!(matches!(result, Err(Refused)));
    }
    let mut tx = writer.transaction().await.unwrap();
    assert_eq!(stage(&mut tx, &scope, lease).await, Err(Refused));
    tx.rollback().await.unwrap();

    for (retention, absolute) in [("-infinity", "infinity"), ("infinity", "-infinity")] {
        observer.batch_execute(&format!(
            "UPDATE public.storage_validated_digest_winners SET retain_until = '{retention}' WHERE object_id = '{object}';
             UPDATE public.ingest_session_reservations SET absolute_deadline = '{absolute}' WHERE id = '{sid}';"
        )).await.unwrap();
        let result = read_completion(writer, &scope, authorized).await;
        assert!(matches!(result, Err(Refused)));
    }
    observer
        .batch_execute(&format!(
            "BEGIN;
             DELETE FROM public.ingest_session_cleanup_intents WHERE session_id = '{sid}';
             DELETE FROM public.ingest_session_reservations WHERE id = '{sid}';
             COMMIT;"
        ))
        .await
        .unwrap();
}
