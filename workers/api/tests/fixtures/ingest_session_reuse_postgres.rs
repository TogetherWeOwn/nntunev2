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
    committed: &CommittedFlacUpload<'_, '_>,
) {
    let scope = prepared.completed_scope();
    let expected = committed.outcome();
    let reload =
        async |id: &str, scope: &CompletionScope<'_>| load_authority(authority, id, scope).await;
    let until = SystemTime::now() + Duration::from_secs(60);
    let no_start = async |_: &mut Client, _: &CompletionScope<'_>, _: &str, _: SystemTime| {
        panic!("retry must not initiate another multipart upload")
    };
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
    let obj = &expected.object;
    let mut cleaner = connect(DB).await;
    let cleaner_pid = backend_pid(&cleaner).await;
    let writer_pid = backend_pid(writer).await;
    {
        let held = cleaner.transaction().await.unwrap();
        held.query_one(
            "SELECT 1 FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid FOR UPDATE",
            &[obj],
        ).await.unwrap();
        let mut attempt = std::pin::pin!(open_or_reuse(
            writer, auth, prepared, until, reload, no_start
        ));
        observe_blocked(observer, cleaner_pid, writer_pid, attempt.as_mut())
            .await
            .unwrap();
        set_reuse_grant(observer, &auth.record.id, false).await;
        held.commit().await.unwrap();
        assert!(matches!(attempt.await, Err(Refused)));
        set_reuse_grant(observer, &auth.record.id, true).await;
    }
    assert!(matches!(
        open_or_reuse(
            writer,
            auth,
            prepared,
            until,
            async |_, _| Err(StoreError),
            no_start
        )
        .await,
        Err(Unconfirmed)
    ));
    // The credential initially limits the protection. Once restored, a retry
    // can extend the durable fence to its own deadline.
    let cutoff: SystemTime = observer
        .query_one(
            "SELECT date_trunc('second', clock_timestamp()) + interval '20 seconds'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let expiry = |time: SystemTime| {
        i64::try_from(time.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap()
    };
    let credential_deadline =
        "UPDATE public.native_rest_credentials SET expires_at = $2 WHERE id = $1";
    observer
        .execute(credential_deadline, &[&auth.record.id, &expiry(cutoff)])
        .await
        .unwrap();
    // Submit two normal retries on separate writer connections. The first
    // holds the session while it waits for the winner, so the second queues
    // behind it before either can return a receipt.
    let (first, second) = {
        let mut other_writer = connect(WRITER).await;
        let other_pid = backend_pid(&other_writer).await;
        let held = cleaner.transaction().await.unwrap();
        held.query_one(
            "SELECT 1 FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid FOR UPDATE",
            &[obj],
        ).await.unwrap();
        let mut first = std::pin::pin!(open_or_reuse(
            writer, auth, prepared, until, reload, no_start
        ));
        observe_blocked(observer, cleaner_pid, writer_pid, first.as_mut())
            .await
            .unwrap();
        let mut second = std::pin::pin!(open_or_reuse(
            &mut other_writer,
            auth,
            prepared,
            until,
            reload,
            no_start
        ));
        observe_blocked(observer, writer_pid, other_pid, second.as_mut())
            .await
            .unwrap();
        held.commit().await.unwrap();
        tokio::join!(first, second)
    };
    for attempt in [first, second] {
        let UploadAttempt::Reused(reused) = attempt.unwrap() else {
            panic!("a completed retry must return protected reuse");
        };
        assert_eq!(reused.usable_until(), cutoff);
        assert_pins(reused.outcome(), expected);
        assert!(committed
            .verified()
            .matches_committed_outcome(reused.outcome()));
    }
    observer
        .execute(
            credential_deadline,
            &[&auth.record.id, &(auth.record.expires_at as i64)],
        )
        .await
        .unwrap();
    let wrong_map = CandidateIntent {
        map_id: [0xff; 16],
        ..*prepared.intent
    };
    assert!(matches!(
        open_or_reuse(
            writer,
            auth,
            PreparedUpload {
                intent: &wrong_map,
                ..prepared
            },
            until,
            reload,
            no_start,
        )
        .await,
        Err(Refused)
    ));
    assert!(observer
        .query_one(
            "SELECT upload_until = $2 FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid",
            &[obj, &cutoff],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    let extended = reuse(writer, auth, &scope, job, reload).await.unwrap();
    assert_pins(extended.outcome(), expected);
    assert!(extended.usable_until() > cutoff);
    assert!(extended.usable_until() <= SystemTime::now() + Duration::from_secs(30));

    // Tighten the session and retention independently. Neither shorter retry
    // may take protection away from the caller with the extended deadline.
    let session = observer.query_one(
        "SELECT absolute_deadline, idle_deadline FROM public.ingest_session_reservations WHERE id = $1::text::uuid",
        &[&scope.session],
    ).await.unwrap();
    let move_session = "UPDATE public.ingest_session_reservations
        SET absolute_deadline = $2, idle_deadline = $3
        WHERE id = $1::text::uuid";
    observer
        .execute(move_session, &[&scope.session, &cutoff, &UNIX_EPOCH])
        .await
        .unwrap();
    assert_eq!(
        reuse(writer, auth, &scope, job, reload)
            .await
            .unwrap()
            .usable_until(),
        cutoff
    );
    observer
        .execute(
            move_session,
            &[
                &scope.session,
                &session.get::<_, SystemTime>(0),
                &session.get::<_, SystemTime>(1),
            ],
        )
        .await
        .unwrap();

    observer.execute(
        "UPDATE public.storage_validated_digest_winners SET retain_until = $2 WHERE object_id = $1::text::uuid",
        &[obj, &cutoff],
    ).await.unwrap();
    assert_eq!(
        reuse(writer, auth, &scope, job, reload)
            .await
            .unwrap()
            .usable_until(),
        cutoff
    );
    let protected = observer
        .query_one(
            "UPDATE public.storage_validated_digest_winners SET retain_until = '-infinity'
         WHERE object_id = $1::text::uuid RETURNING upload_until",
            &[obj],
        )
        .await
        .unwrap()
        .get::<_, SystemTime>(0);
    assert_eq!(protected, extended.usable_until());
    // With only the reuse fence active, the real delete guard refuses admission.
    assert!(!observer
        .query_one(DELETE, &[obj])
        .await
        .unwrap()
        .get::<_, bool>(0));

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

async fn set_reuse_grant(observer: &Client, credential_id: &str, allowed: bool) {
    assert_eq!(
        observer
            .execute(
                "UPDATE public.fixture_upload_grants SET can_reuse = $2 WHERE credential_id = $1",
                &[&credential_id, &allowed],
            )
            .await
            .unwrap(),
        1
    );
}

fn assert_pins(got: &CommittedOutcome, expected: &CommittedOutcome) {
    assert_eq!(got.map, expected.map);
    assert_eq!(got.object, expected.object);
    assert_eq!(got.generation, expected.generation);
    assert_eq!(got.configuration, expected.configuration);
    assert_eq!(got.root_version, expected.root_version);
    assert_eq!(got.media_version, expected.media_version);
    assert_eq!(got.page_versions, expected.page_versions);
}

pub(super) async fn refuse_unverified(
    observer: &Client,
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
        open_or_reuse(
            writer,
            auth,
            prepared,
            SystemTime::now() + Duration::from_secs(45),
            async |_, _| panic!("unverified inventory must not reach the authority reader"),
            async |_, _, _, _| panic!("a terminal reservation must not initiate an upload"),
        )
        .await,
        Err(Refused)
    ));
}
