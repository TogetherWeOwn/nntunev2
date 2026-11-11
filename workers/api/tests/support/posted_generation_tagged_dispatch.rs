//! Real DB producer tests; object IO is scripted, NOT real R2 acceptance.
//! The production R2 implementation compiles in the normal ingest target.
use super::*;
use nntune_ingest::flac::{FlacWalk, StreamInfo, TaggedFlac};
use nntune_ingest::posted_generations::dispatch::{
    prepare_tagged_file_dispatch,
    storage::{tagged_object_id, TaggedObjectMetadata, TaggedObjectStore},
    TaggedDispatchInput,
};

fn tagged(bytes: Vec<u8>) -> TaggedFlac {
    TaggedFlac {
        bytes,
        walk: FlacWalk {
            streaminfo: StreamInfo {
                min_block_size: 16,
                max_block_size: 16,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 0,
                md5: [0; 16],
            },
            frame_count: 0,
            total_samples: 0,
            seek_index: Vec::new(),
            has_seektable: false,
            first_frame_offset: 0,
            frame_spans: Vec::new(),
        },
        sha256_hex: "untrusted-sidecar".into(),
        art: Vec::new(),
        skipped_art: Vec::new(),
    }
}

fn input(o: &Objects) -> TaggedDispatchInput<'_> {
    TaggedDispatchInput {
        file_index: 0,
        tagged: tagged(o.file_bytes.clone()),
        plan: o.plan.clone(),
        canonical_plan: &o.plan_bytes,
    }
}

enum Drift {
    None,
    Role,
    Stopped,
    Original,
    Owner(String),
    Admission,
}

struct Store<'a> {
    db: Client,
    fx: &'a Fx,
    object: Option<(DispatchObject, Vec<u8>)>,
    drift: Drift,
    after: Option<String>,
    lost_reply: bool,
    writes: usize,
    bad_read: bool,
}

impl<'a> Store<'a> {
    async fn new(fx: &'a Fx) -> Self {
        Self {
            db: connect().await,
            fx,
            object: None,
            drift: Drift::None,
            after: None,
            lost_reply: false,
            writes: 0,
            bad_read: false,
        }
    }
}

impl TaggedObjectStore for Store<'_> {
    async fn create(
        &mut self,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        self.writes += 1;
        // These statements complete in an independent connection while the
        // producer awaits storage, proving it holds no admission/job locks.
        match std::mem::replace(&mut self.drift, Drift::None) {
            Drift::None => {}
            Drift::Role => {
                self.db
                    .execute(
                        "UPDATE users SET role = 'member' WHERE id = $1",
                        &[&self.fx.owner],
                    )
                    .await
                    .quiet("change role during storage");
            }
            Drift::Stopped => {
                self.db
                    .execute(
                        "UPDATE ingest_jobs SET status = 'done' WHERE id = $1",
                        &[&self.fx.job],
                    )
                    .await
                    .quiet("stop during storage");
            }
            Drift::Original => {
                self.db
                    .execute(
                        "DELETE FROM ingest_originals WHERE job_id = $1",
                        &[&self.fx.job],
                    )
                    .await
                    .quiet("delete original during storage");
            }
            Drift::Owner(owner) => {
                self.db
                    .execute(
                        "UPDATE ingest_jobs SET user_id = $2 WHERE id = $1",
                        &[&self.fx.job, &owner],
                    )
                    .await
                    .quiet("rebind owner during storage");
            }
            Drift::Admission => {
                let hash = digest(&self.db, "new-admission", &self.fx.uuid).await;
                let tx = self.db.transaction().await.quiet("begin admission drift");
                tx.execute("UPDATE ingest_jobs SET content_sha256 = $2, idempotency_key = $3 WHERE id = $1", &[&self.fx.job, &hash, &format!("admin:{hash}")]).await.quiet("rebind admitted hash");
                tx.execute(
                    "UPDATE ingest_originals SET content_sha256 = $2 WHERE job_id = $1",
                    &[&self.fx.job, &hash],
                )
                .await
                .quiet("rebind original hash");
                tx.commit().await.quiet("commit admission drift");
            }
        }
        self.after = Some(state(&self.db, self.fx).await);
        if self.object.is_some() {
            return Ok(None);
        }
        let object = DispatchObject::new(id, "stored-version")?;
        let size = bytes.len() as u64;
        self.object = Some((object.clone(), bytes));
        if self.lost_reply {
            return Err(StoreError::Database);
        }
        Ok(Some(TaggedObjectMetadata { object, size }))
    }

    async fn head(&mut self, id: &str) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        Ok(self
            .object
            .as_ref()
            .filter(|(object, _)| object.id() == id)
            .map(|(object, bytes)| TaggedObjectMetadata {
                object: object.clone(),
                size: bytes.len() as u64,
            }))
    }

    async fn read(
        &mut self,
        expected: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        let (object, bytes) = self.object.as_ref().ok_or(StoreError::Inconsistent)?;
        if object != expected || self.bad_read {
            return Err(StoreError::Inconsistent);
        }
        let start = offset as usize;
        Ok(bytes[start..(start + limit).min(bytes.len())].to_vec())
    }
}

#[test]
fn postgres_tagged_producer_commits_byte_derived_storage_identity_and_replays() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, LARGE);
        let mut store = Store::new(&fx).await;
        let pointer = prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store,
        )
        .await
        .expect("prepare from stored bytes")
        .expect("prepared");
        let row = db.query_one("SELECT object_id, object_version, plaintext_bytes, plaintext_sha256 FROM ingest_file_dispatches WHERE id = $1", &[&id_of(&pointer)]).await.quiet("read stored identity");
        let actual = PreparedDispatchFile::from_bytes(
            0,
            store.object.as_ref().expect("object").0.clone(),
            &o.file_bytes,
            o.plan.clone(),
            &o.plan_bytes,
        )
        .expect("byte identity");
        assert!(
            row.get::<_, String>("object_id")
                == tagged_object_id(&fx.job, "tagged-op", 0).expect("key")
        );
        assert_eq!(row.get::<_, String>("object_version"), "stored-version");
        assert_eq!(row.get::<_, i64>("plaintext_bytes"), LARGE as i64);
        let before = state(&db, &fx).await;
        // The existing byte-based preparation must see exactly the same input,
        // regardless of the producer's deliberately false public sidecar hash.
        assert!(prep(&mut db, &fx, "tagged-op", 0, &actual).await.as_ref() == Some(&pointer));
        let replay = prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store,
        )
        .await
        .expect("producer replay");
        assert!(replay.as_ref() == Some(&pointer));
        assert!(before == state(&db, &fx).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 1);
        assert_eq!(attempts(&db, &fx).await, 0);
        assert_eq!(receipts(&db, &fx).await, 0);
        assert_eq!(status(&db, &fx).await, "queued");
    });
}

#[test]
fn postgres_tagged_producer_disabled_or_unadmitted_never_reaches_storage() {
    run_test(async {
        let mut db = connect().await;
        for shape in [
            Shape::admin(),
            Shape {
                role: "member",
                ..Shape::admin()
            },
            Shape {
                original: false,
                ..Shape::admin()
            },
        ] {
            let fx = seed(&db, shape).await;
            let o = objects(&fx, SMALL);
            let mut store = Store::new(&fx).await;
            let before = state(&db, &fx).await;
            let mode = if shape.role == "admin" && shape.original {
                OFF
            } else {
                ON
            };
            assert!(matches!(
                prepare_tagged_file_dispatch(
                    &mut db,
                    mode,
                    &fx.job,
                    "tagged-op",
                    0,
                    input(&o),
                    &mut store
                )
                .await,
                Ok(None)
            ));
            assert_eq!(store.writes, 0);
            assert!(before == state(&db, &fx).await);
        }
    });
}

#[test]
fn postgres_tagged_producer_revalidates_role_stop_original_owner_and_admission_after_storage() {
    run_test(async {
        let mut db = connect().await;
        for case in 0..5 {
            let fx = admin_fx(&db).await;
            let o = objects(&fx, SMALL);
            let mut store = Store::new(&fx).await;
            store.drift = match case {
                0 => Drift::Role,
                1 => Drift::Stopped,
                2 => Drift::Original,
                3 => Drift::Owner(admin_fx(&db).await.owner),
                _ => Drift::Admission,
            };
            assert!(matches!(
                prepare_tagged_file_dispatch(
                    &mut db,
                    ON,
                    &fx.job,
                    "tagged-op",
                    0,
                    input(&o),
                    &mut store
                )
                .await,
                Ok(None)
            ));
            assert_eq!(store.writes, 1);
            assert!(store.object.is_some());
            assert!(store.after.as_ref() == Some(&state(&db, &fx).await));
            assert_eq!(dispatch_rows(&db, &fx).await, 0);
            assert_eq!(attempts(&db, &fx).await, 0);
        }
    });
}

#[test]
fn postgres_tagged_producer_ambiguous_put_and_divergent_retry_keep_one_identity() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let mut o = objects(&fx, SMALL);
        let mut store = Store::new(&fx).await;
        store.lost_reply = true;
        let first = prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store,
        )
        .await
        .expect("reconcile ambiguous PUT")
        .expect("prepared");
        let before = state(&db, &fx).await;
        o.file_bytes[0] ^= 1;
        assert!(prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store
        )
        .await
        .is_err());
        assert!(before == state(&db, &fx).await);
        o.file_bytes[0] ^= 1;
        let replay = prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store,
        )
        .await
        .expect("exact retry");
        assert!(replay.as_ref() == Some(&first));
        assert!(before == state(&db, &fx).await);
    });
}

#[test]
fn postgres_tagged_producer_readback_failure_retains_object_without_dispatch_or_attempt() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let mut store = Store::new(&fx).await;
        store.bad_read = true;
        let before = state(&db, &fx).await;
        assert!(prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            input(&o),
            &mut store
        )
        .await
        .is_err());
        assert!(store.object.is_some());
        assert!(before == state(&db, &fx).await);
        assert_eq!(dispatch_rows(&db, &fx).await, 0);
    });
}

#[test]
fn postgres_tagged_producer_refuses_plan_alias_and_invalid_file_before_storage() {
    run_test(async {
        let mut db = connect().await;
        let fx = admin_fx(&db).await;
        let o = objects(&fx, SMALL);
        let mut store = Store::new(&fx).await;
        let before = state(&db, &fx).await;
        let mut alias = input(&o);
        alias.plan = DispatchObject::new(
            &tagged_object_id(&fx.job, "tagged-op", 0).expect("key"),
            "version-a",
        )
        .expect("alias");
        assert!(prepare_tagged_file_dispatch(
            &mut db,
            ON,
            &fx.job,
            "tagged-op",
            0,
            alias,
            &mut store
        )
        .await
        .is_err());
        let mut bad = input(&o);
        bad.file_index = 99;
        assert!(matches!(
            prepare_tagged_file_dispatch(&mut db, ON, &fx.job, "tagged-op", 0, bad, &mut store)
                .await,
            Ok(None)
        ));
        assert_eq!(store.writes, 0);
        assert!(before == state(&db, &fx).await);
    });
}
