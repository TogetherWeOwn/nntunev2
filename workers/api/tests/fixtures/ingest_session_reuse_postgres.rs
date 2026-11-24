use super::*;

const DELETE: &str = "SELECT public.admit_storage_digest_delete(
    w.environment_id, w.library_id, w.plaintext_sha256, w.plaintext_bytes,
    w.object_id, i.generation_id, w.root_version_id)
FROM public.storage_validated_digest_winners w
JOIN public.storage_object_intents i USING (object_id) WHERE w.object_id = $1::text::uuid";

pub(super) async fn protect_completed(
    observer: &Client,
    authority: &Client,
    writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    expected: &CommittedOutcome,
) {
    let scope = prepared.completed_scope();
    let reload =
        async |id: &str, scope: &CompletionScope<'_>| load_authority(authority, id, scope).await;
    let job = prepared.job;
    for foreign_scope in [
        CompletionScope {
            environment: [0xff; 16],
            ..scope
        },
        CompletionScope {
            library: [0xff; 16],
            ..scope
        },
    ] {
        let env = uuid_text(foreign_scope.environment);
        let lib = uuid_text(foreign_scope.library);
        let foreign_job = Job {
            environment: &env,
            library_id: &lib,
            ..*job
        };
        let foreign_grant = Grant {
            environment: env.clone(),
            library_id: lib.clone(),
            ..auth.grant.clone()
        };
        assert!(matches!(
            reuse(
                writer,
                Authorization {
                    grant: &foreign_grant,
                    ..auth
                },
                &foreign_scope,
                &foreign_job,
                async |_, _| panic!("scope check must precede the reload")
            )
            .await,
            Err(Refused)
        ));
    }
    observer
        .execute(
            "UPDATE public.fixture_upload_grants SET can_reuse = false WHERE credential_id = $1",
            &[&auth.record.id],
        )
        .await
        .unwrap();
    assert!(matches!(
        reuse(writer, auth, &scope, job, reload).await,
        Err(Refused)
    ));
    observer
        .execute(
            "UPDATE public.fixture_upload_grants SET can_reuse = true WHERE credential_id = $1",
            &[&auth.record.id],
        )
        .await
        .unwrap();

    let first = reuse(writer, auth, &scope, job, reload).await.unwrap();
    let retry = reuse(writer, auth, &scope, job, reload).await.unwrap();
    for reuse in [&first, &retry] {
        let got = reuse.outcome();
        assert_eq!(got.map, expected.map);
        assert_eq!(got.object, expected.object);
        assert_eq!(got.generation, expected.generation);
        assert_eq!(got.configuration, expected.configuration);
        assert_eq!(got.root_version, expected.root_version);
        assert_eq!(got.media_version, expected.media_version);
        assert_eq!(got.page_versions, expected.page_versions);
        assert!(reuse.usable_until() > SystemTime::now());
        assert!(reuse.usable_until() <= SystemTime::now() + Duration::from_secs(30));
    }
    let obj = &expected.object;
    let protected = observer
        .query_one(
            "UPDATE public.storage_validated_digest_winners SET retain_until = '-infinity'
         WHERE object_id = $1::text::uuid RETURNING upload_until",
            &[obj],
        )
        .await
        .unwrap()
        .get::<_, SystemTime>(0);
    assert_eq!(protected, retry.usable_until());
    assert!(protected >= first.usable_until());
    // With only the reuse fence active, the real delete guard refuses admission.
    assert!(!observer
        .query_one(DELETE, &[obj])
        .await
        .unwrap()
        .get::<_, bool>(0));

    let mut cleaner = connect(DB).await;
    let cleaner_pid = backend_pid(&cleaner).await;
    let writer_pid = backend_pid(writer).await;
    let cleanup = cleaner.transaction().await.unwrap();
    // Move the fence to the past inside the transaction to model elapsed time
    // without waiting. The reuse query sees a committed candidate, then blocks.
    cleanup.execute(
        "UPDATE public.storage_validated_digest_winners SET upload_until = '-infinity' WHERE object_id = $1::text::uuid",
        &[obj],
    ).await.unwrap();
    assert!(cleanup
        .query_one(DELETE, &[obj])
        .await
        .unwrap()
        .get::<_, bool>(0));
    let mut attempt = std::pin::pin!(reuse(writer, auth, &scope, job, async |_, _| {
        panic!("a tombstoned winner must not survive the lock wait")
    }));
    observe_blocked(observer, cleaner_pid, writer_pid, attempt.as_mut())
        .await
        .unwrap();
    cleanup.commit().await.unwrap();
    assert!(matches!(attempt.await, Err(Refused)));
    assert!(observer
        .query_one(
            "SELECT deleting_at IS NOT NULL AND upload_until = '-infinity'
         FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid",
            &[obj],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
}

pub(super) async fn refuse_unverified(
    observer: &Client,
    authority: &Client,
    writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    map: &str,
) {
    let scope = prepared.completed_scope();
    // Simulate a recorded inventory without a verified winner. The object and
    // final session coordinates match, so neither is sufficient for reuse.
    observer
        .execute(
            "INSERT INTO public.storage_digest_inventory_intents VALUES ($1::text::uuid, $2, $3)",
            &[&map, &MEDIA, &&[PAGE][..]],
        )
        .await
        .unwrap();
    assert_eq!(observer.execute(
        "UPDATE public.ingest_session_reservations SET terminal_reason = 'completed',
             terminal_at = clock_timestamp(), completed_map_id = $2::text::uuid,
             checkpoint_generation = $3::text::uuid, checkpoint_revision = $4, checkpoint_offset = $5,
             write_lease_id = NULL, write_lease_generation = NULL
         WHERE id = $1::text::uuid",
        &[&scope.session, &map, &uuid_text(scope.seal), &scope.revision, &scope.offset],
    ).await.unwrap(), 1);
    assert!(matches!(
        reuse(writer, auth, &scope, prepared.job, async |id, locked| {
            load_authority(authority, id, locked).await
        })
        .await,
        Err(Refused)
    ));
}
