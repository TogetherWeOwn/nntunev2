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
    let fence_is = async |expr: &str| {
        let sql = format!("SELECT upload_until = {expr} FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid");
        observer
            .query_one(&sql, &[obj])
            .await
            .unwrap()
            .get::<_, bool>(0)
    };
    // Unauthorized and cross-owner reuse claims must fail closed before reaching
    // the authority reader or extending the winner's reuse fence.
    let upload_only = Grant {
        actions: vec![Action::Create, Action::Upload],
        ..auth.grant.clone()
    };
    let mut revoked = auth.record.clone();
    revoked.revoked_at = Some(auth.record.created_at);
    let mut stranger = auth.record.clone();
    stranger.user_id = "foreign-reuse-principal".into();
    let stranger_auth = Authorization {
        record: &stranger,
        ..auth
    };
    for denied in [
        Authorization {
            grant: &upload_only,
            ..auth
        },
        Authorization {
            record: &revoked,
            ..auth
        },
        stranger_auth,
    ] {
        let res = reuse(writer, denied, &scope, job, async |_, _| {
            panic!("pre-lock reuse denial reached reader")
        })
        .await;
        assert!(matches!(res, Err(Refused)));
    }
    // Self-consistent cross-owner grant, job and scope targeting the owner's
    // completed session must be refused by `outcome_lock.sql`'s principal fence.
    let stranger_job = Job {
        owner_id: "foreign-reuse-principal",
        ..*job
    };
    let stranger_scope = CompletionScope {
        principal: stranger_job.owner_id,
        ..scope
    };
    let res = reuse(
        writer,
        stranger_auth,
        &stranger_scope,
        &stranger_job,
        async |_, _| panic!("cross-owner reuse bypassed the SQL principal fence"),
    )
    .await;
    assert!(matches!(res, Err(Refused)));
    // A second uncompleted session with the exact same plaintext SHA-256 and
    // checkpoint coordinates in the same scope must pass `outcome_lock.sql` and
    // still be refused by `reuse_lock.sql`, never claiming the first winner by digest.
    let dup_sid = fresh_id().unwrap();
    let inserted_dup: String = writer
        .query_one(
            "SELECT pg_temp.reserve($1::text::uuid, $2::text::uuid, $3, $4, $5, $6::text::uuid)::text",
            &[&uuid_text(scope.environment), &uuid_text(scope.library), &scope.principal, &"duplicate-digest-reuse", &scope.fingerprint.as_slice(), &dup_sid],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(inserted_dup.replace('-', ""), dup_sid);
    assert_eq!(
        observer
            .execute(
                "UPDATE public.ingest_session_reservations SET checkpoint_generation = $2::text::uuid, checkpoint_revision = $3, checkpoint_offset = $4 WHERE id = $1::text::uuid",
                &[&dup_sid, &uuid_text(scope.seal), &scope.revision, &scope.offset],
            )
            .await
            .unwrap(),
        1
    );
    let dup_job = Job {
        id: &dup_sid,
        ..*job
    };
    let dup_scope = CompletionScope {
        session: &dup_sid,
        ..scope
    };
    let dup_prepared = PreparedUpload {
        scope: CompletionScope {
            session: &dup_sid,
            ..prepared.scope
        },
        job: &dup_job,
        ..prepared
    };
    // Prove `outcome_lock.sql` accepts `dup_scope` (returning `Ok(None)` while
    // uncompleted), isolating the `reuse` refusal to `reuse_lock.sql`.
    assert!(read(writer, auth, dup_prepared, reload)
        .await
        .unwrap()
        .is_none());
    let res = reuse(writer, auth, &dup_scope, &dup_job, async |_, _| {
        panic!("uncompleted duplicate-digest session claimed another session's winner")
    })
    .await;
    assert!(matches!(res, Err(Refused)));
    observer
        .batch_execute(&format!(
            "BEGIN; DELETE FROM public.ingest_session_cleanup_intents WHERE session_id = '{dup_sid}'::uuid; DELETE FROM public.ingest_session_reservations WHERE id = '{dup_sid}'::uuid; COMMIT;"
        ))
        .await
        .unwrap();
    assert!(fence_is("'-infinity'").await);

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
    assert!(fence_is("'-infinity'").await);
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
    assert!(fence_is("'-infinity'").await);
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
    // behind it before either can return a receipt. The second caller's
    // fresh-upload deadline has already passed.
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
            UNIX_EPOCH,
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

    // A commit failure after `reuse_protect.sql` updates `upload_until` must
    // roll back the fence extension, release locks, and return no pins.
    observer
        .batch_execute(&format!(
            "CREATE SEQUENCE public.fixture_reuse_commit_attempts;
             CREATE FUNCTION public.fixture_reject_reuse_commit() RETURNS trigger
             LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, public AS $$
             BEGIN
                 PERFORM nextval('public.fixture_reuse_commit_attempts');
                 RAISE EXCEPTION 'synthetic reuse commit refusal';
             END $$;
             REVOKE ALL ON FUNCTION public.fixture_reject_reuse_commit() FROM PUBLIC;
             CREATE CONSTRAINT TRIGGER fixture_reject_reuse_commit AFTER UPDATE
             ON public.storage_validated_digest_winners DEFERRABLE INITIALLY DEFERRED
             FOR EACH ROW WHEN (NEW.object_id = '{obj}'::uuid)
             EXECUTE FUNCTION public.fixture_reject_reuse_commit();"
        ))
        .await
        .unwrap();
    assert!(matches!(
        reuse(writer, auth, &scope, job, reload).await,
        Err(Unconfirmed)
    ));
    assert!(matches!(
        open_or_reuse(writer, auth, prepared, until, reload, no_start).await,
        Err(Unconfirmed)
    ));
    let attempts: i64 = observer
        .query_one(
            "SELECT last_value FROM public.fixture_reuse_commit_attempts",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(attempts, 2, "both reuse paths must reach the commit gate");
    assert!(observer
        .query_one(
            "SELECT upload_until = $2 FROM public.storage_validated_digest_winners WHERE object_id = $1::text::uuid",
            &[obj, &cutoff],
        )
        .await
        .unwrap()
        .get::<_, bool>(0));
    for (table, column, id) in [
        ("ingest_session_reservations", "id", scope.session),
        (
            "storage_validated_digest_winners",
            "object_id",
            obj.as_str(),
        ),
    ] {
        observer
            .query_one(
                &format!("SELECT 1 FROM public.{table} WHERE {column} = $1::text::uuid FOR UPDATE NOWAIT"),
                &[&id],
            )
            .await
            .expect("rolled-back reuse must release its row lock");
    }
    observer
        .batch_execute(
            "DROP TRIGGER fixture_reject_reuse_commit ON public.storage_validated_digest_winners;
             DROP FUNCTION public.fixture_reject_reuse_commit();
             DROP SEQUENCE public.fixture_reuse_commit_attempts;",
        )
        .await
        .unwrap();

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
