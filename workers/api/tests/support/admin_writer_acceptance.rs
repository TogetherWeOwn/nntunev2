//! Real statement execution in disposable PostgreSQL. The checkpoint runtime
//! is fake; these tests do not establish live dispatch, durable checkpoint CAS,
//! production schema acceptance or atomic Done/publication integration.
#[path = "admin_checkpoint_acceptance.rs"]
mod checkpoint_acceptance;
#[path = "admin_upload_acceptance.rs"]
mod upload_acceptance;
use super::{client, OTHER_RELEASE, RECORDING, RELEASE};
use nntune_api::{
    admin_ingest as intake,
    admin_original::{
        read_original, OriginalAuthorization, OriginalBody, OriginalError,
        PostgresOriginalRepository,
    },
    original_source::{
        seal_original_source, SourceMetadata, SourceReadAuthorization, SourceRepository,
        SourceServiceError,
    },
    rotation::KekSource,
};
use nntune_core::{nzb::Document, original_source::OriginalSource};
use nntune_ingest::{
    admin_execution::{run_stage, StageError, StageRuntime},
    admin_writers::{
        write_album, AlbumWrite, FileGeometry, SourceEnvelope, SourceSealer, WriteError,
    },
    jobs::{JobCheckpoint, JobSource},
    matching::MbRecording,
    probe::{ProbeOutcome, ProbeRequest},
};

struct Keys;
impl KekSource for Keys {
    fn kek(&self, version: u32) -> Option<&[u8; 32]> {
        (version == 7).then_some(&[9; 32])
    }
}
struct Authority;
impl OriginalAuthorization for Authority {
    fn authorize(&self, _: &str) -> Result<(), OriginalError> {
        Ok(())
    }
}
impl SourceReadAuthorization for Authority {
    fn authorize(&self, _: &str) -> Result<(), SourceServiceError> {
        Ok(())
    }
}

struct Runtime(JobCheckpoint);
impl StageRuntime for Runtime {
    async fn match_release(&mut self, _: &Document) -> Result<Option<String>, StageError> {
        Ok(Some(RELEASE.into()))
    }
    async fn probe(&mut self, req: &ProbeRequest) -> Result<ProbeOutcome, StageError> {
        Ok(ProbeOutcome {
            batch_index: req.batch_index,
            present: req.segment_ids.clone(),
            missing: vec![],
        })
    }
    async fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> Result<bool, StageError> {
        if expected != &self.0 {
            return Ok(false);
        }
        self.0 = next.clone();
        Ok(true)
    }
}

#[derive(Default)]
struct Sealer {
    calls: usize,
    fail: bool,
    pending: bool,
}
impl SourceSealer for Sealer {
    async fn seal(&mut self, source: &OriginalSource) -> Result<SourceEnvelope, WriteError> {
        self.calls += 1;
        if self.pending {
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(WriteError::Seal);
        }
        let sealed = seal_original_source(&Keys, 7, source).map_err(|_| WriteError::Seal)?;
        Ok(SourceEnvelope {
            ciphertext: sealed.envelope.ciphertext,
            kek_version: sealed.metadata.kek_version,
            dek_wrapped_nonce: sealed.envelope.dek_wrapped_nonce,
            dek_wrapped: sealed.envelope.dek_wrapped,
            payload_nonce: sealed.envelope.payload_nonce,
        })
    }
}

async fn fixture() -> (
    tokio_postgres::Client,
    OriginalBody,
    JobCheckpoint,
    Vec<MbRecording>,
    Vec<FileGeometry>,
) {
    let app = client(false).await;
    // Isolated tables test the real adapters/SQL, not migration acceptance.
    app.batch_execute(
        "CREATE TEMP TABLE ingest_jobs (id TEXT PRIMARY KEY, source TEXT, status TEXT, content_sha256 TEXT, idempotency_key TEXT, nzb_bytes BIGINT);
         CREATE TEMP TABLE ingest_originals (job_id TEXT PRIMARY KEY REFERENCES ingest_jobs(id), content_sha256 TEXT, nzb_bytes BIGINT, nzb_ref_envelope_version SMALLINT, nzb_ref_payload_format SMALLINT, nzb_ref_kek_version INTEGER, nzb_ref_encrypted BYTEA, nzb_ref_dek_wrapped_nonce BYTEA, nzb_ref_dek_wrapped BYTEA, nzb_ref_payload_nonce BYTEA);
         CREATE TEMP TABLE tracks (id UUID PRIMARY KEY DEFAULT gen_random_uuid(), mbid UUID, release_id UUID NOT NULL, title TEXT NOT NULL, medium_no INTEGER NOT NULL, track_no INTEGER NOT NULL, UNIQUE(release_id,medium_no,track_no));
         CREATE TEMP TABLE track_sources (track_id UUID PRIMARY KEY REFERENCES tracks(id), source_job_id TEXT REFERENCES ingest_jobs(id), indexer_id UUID, nzb_ref_encrypted BYTEA, nzb_ref_envelope_version SMALLINT, nzb_ref_kek_version INTEGER, nzb_ref_dek_wrapped_nonce BYTEA, nzb_ref_dek_wrapped BYTEA, nzb_ref_payload_nonce BYTEA, nzb_ref_payload_format SMALLINT)"
    ).await.unwrap();
    // Reuse only the existing synthetic corpus, never add or log payloads.
    let body = nntune_ingest::fixtures::SMALL_NZB.as_bytes();
    let job = intake::derive_job_id(body);
    let hash = intake::content_sha256_hex(body);
    let bytes = body.len() as i64;
    let sealed = intake::encrypt_ingest_original(&[9; 32], 7, &hash, &job, body).unwrap();
    app.execute(
        "INSERT INTO ingest_jobs VALUES ($1,'admin','queued',$2,'admin:' || $2,$3)",
        &[&job, &hash, &bytes],
    )
    .await
    .unwrap();
    app.execute(
        "INSERT INTO ingest_originals VALUES ($1,$2,$3,2,1,7,$4,$5,$6,$7)",
        &[
            &job,
            &hash,
            &bytes,
            &sealed.ciphertext,
            &&sealed.dek_wrapped_nonce[..],
            &sealed.dek_wrapped,
            &&sealed.payload_nonce[..],
        ],
    )
    .await
    .unwrap();
    let original = read_original(
        &Authority,
        &mut PostgresOriginalRepository { client: &app },
        &Keys,
        &job,
    )
    .await
    .unwrap();
    let msg = nntune_ingest::dispatch::build_upload_dispatch(
        "stage_user",
        nntune_ingest::fixtures::SMALL_NZB,
    )
    .unwrap();
    let mut cp = nntune_ingest::checkpoint::initial(&msg);
    cp.source = JobSource::AdminUpload;
    cp.user_id.clear();
    cp.job_id = original.job_id().into();
    cp.idempotency_key = original.idempotency_key().into();
    let mut snapshot = nntune_ingest::admin_checkpoint::Snapshot::initial(
        cp.clone(),
        &nntune_ingest::admin_checkpoint::Identity {
            job_id: original.job_id(),
            user_id: "",
            idempotency_key: original.idempotency_key(),
        },
    )
    .unwrap();
    // This writer-primitive fixture deliberately fakes checkpoint persistence.
    // The original reader no longer changes job status; only this test seeds it.
    app.batch_execute("UPDATE ingest_jobs SET status = 'running'")
        .await
        .unwrap();
    let mut runtime = Runtime(cp.clone());
    for _ in 0..4 {
        run_stage(
            &mut runtime,
            &mut cp,
            original.job_id(),
            original.idempotency_key(),
            original.parsed(),
        )
        .await
        .unwrap();
        let identity = nntune_ingest::admin_checkpoint::Identity {
            job_id: original.job_id(),
            user_id: "",
            idempotency_key: original.idempotency_key(),
        };
        snapshot = snapshot.successor(cp.clone(), false, &identity).unwrap();
        let restored = nntune_ingest::admin_checkpoint::Snapshot::decode(
            &snapshot.encode().unwrap(),
            snapshot.revision(),
            &identity,
        )
        .unwrap();
        assert!(restored == snapshot);
    }
    assert_eq!((snapshot.revision(), snapshot.generation()), (5, 1));
    let recordings = vec![MbRecording {
        mbid: RECORDING.into(),
        title: "Catalogue title".into(),
        track_no: Some(1),
        medium_no: Some(1),
    }];
    // Fake trusted decoded-file inspection; never infer this from STAT bytes.
    let geometry = vec![FileGeometry {
        file_ordinal: 0,
        decoded_bytes: 64,
        range_hint: None,
    }];
    (app, original, cp, recordings, geometry)
}

fn write<'a>(
    original: &'a OriginalBody,
    cp: &'a JobCheckpoint,
    recordings: &'a [MbRecording],
    geometry: &'a [FileGeometry],
) -> AlbumWrite<'a> {
    AlbumWrite {
        checkpoint: cp,
        original_job: original.job_id(),
        original_key: original.idempotency_key(),
        parsed: original.parsed(),
        recordings,
        geometry,
    }
}

async fn counts(app: &tokio_postgres::Client) -> (i64, i64, i64) {
    let row = app.query_one("SELECT (SELECT count(*) FROM tracks), (SELECT count(*) FROM track_sources), (SELECT count(*) FROM library)", &[]).await.unwrap();
    (row.get(0), row.get(1), row.get(2))
}

struct StoredSource {
    metadata: SourceMetadata,
    envelope: nntune_api::original_source::FieldEnvelope,
}
impl SourceRepository for StoredSource {
    fn metadata(&mut self, _: &str) -> Result<[Option<SourceMetadata>; 2], SourceServiceError> {
        Ok([Some(self.metadata.clone()), None])
    }
    fn envelope(
        &mut self,
        _: &SourceMetadata,
    ) -> Result<Option<nntune_api::original_source::FieldEnvelope>, SourceServiceError> {
        Ok(Some(self.envelope.clone()))
    }
}

#[tokio::test]
async fn real_writers_use_job_fence_winning_track_and_real_keyring_seal_without_settling() {
    let (mut app, original, cp, recordings, geometry) = fixture().await;
    // A pre-existing natural-key winner, not a proposed random track id.
    app.execute("INSERT INTO tracks (id,mbid,release_id,title,medium_no,track_no) VALUES ($1::text::uuid,$2::text::uuid,$3::text::uuid,'First title',1,1)", &[&OTHER_RELEASE,&RECORDING,&RELEASE]).await.unwrap();
    let mut sealer = Sealer::default();
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Ok(1)
    );
    tx.commit().await.unwrap();
    assert_eq!(counts(&app).await, (1, 1, 1));
    let row = app
        .query_one("SELECT user_id, job_id FROM library", &[])
        .await
        .unwrap();
    assert!(row.get::<_, Option<String>>(0).is_none());
    assert_eq!(row.get::<_, String>(1), original.job_id());
    assert_eq!(
        app.query_one("SELECT status FROM ingest_jobs", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "running"
    );
    let r = app
        .query_one(
            nntune_api::original_source::source_metadata_sql(),
            &[&OTHER_RELEASE],
        )
        .await
        .unwrap();
    let metadata = SourceMetadata {
        track_id: r.get("track_id"),
        source_job_id: r.get("source_job_id"),
        envelope_version: r.get("nzb_ref_envelope_version"),
        payload_format: r.get("nzb_ref_payload_format"),
        kek_version: r.get("nzb_ref_kek_version"),
        ciphertext_len: r.get("ciphertext_len"),
        wrap_nonce_len: r.get("wrap_nonce_len"),
        wrapped_dek_len: r.get("wrapped_dek_len"),
        payload_nonce_len: r.get("payload_nonce_len"),
    };
    let r = app
        .query_one(
            nntune_api::original_source::source_envelope_sql(),
            &[
                &OTHER_RELEASE,
                &original.job_id(),
                &7i32,
                &metadata.ciphertext_len,
            ],
        )
        .await
        .unwrap();
    let nonce = |name| -> [u8; 12] { r.get::<_, Vec<u8>>(name).try_into().unwrap() };
    let envelope = nntune_api::original_source::FieldEnvelope {
        dek_wrapped_nonce: nonce("nzb_ref_dek_wrapped_nonce"),
        dek_wrapped: r.get("nzb_ref_dek_wrapped"),
        payload_nonce: nonce("nzb_ref_payload_nonce"),
        ciphertext: r.get("nzb_ref_encrypted"),
    };
    let before = envelope.ciphertext.clone();
    let opened = nntune_api::original_source::read_original_source(
        &Authority,
        &mut StoredSource { metadata, envelope },
        &Keys,
        OTHER_RELEASE,
    )
    .unwrap();
    assert_eq!(opened.track_id(), OTHER_RELEASE);
    assert_eq!(opened.source_job_id(), original.job_id());
    assert_eq!(opened.file_ordinal(), 0);
    assert_eq!(opened.file_size(), 64);
    // Replays retain the complete first source and do not ask for another seal.
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Ok(1)
    );
    tx.commit().await.unwrap();
    assert_eq!(sealer.calls, 1);
    let after: Vec<u8> = app
        .query_one("SELECT nzb_ref_encrypted FROM track_sources", &[])
        .await
        .unwrap()
        .get(0);
    assert!(before == after);
    assert_eq!(counts(&app).await, (1, 1, 1));
}

#[tokio::test]
async fn writer_savepoint_rolls_back_seal_failure_private_collision_and_database_error() {
    for mode in 0..3 {
        let (mut app, original, cp, recordings, geometry) = fixture().await;
        if mode == 1 {
            app.execute("INSERT INTO library (user_id,job_id,idempotency_key,recording_mbids) VALUES ('u1','private',$1,ARRAY[]::text[])", &[&original.idempotency_key()]).await.unwrap();
        }
        if mode == 2 {
            app.batch_execute("ALTER TABLE library ADD CHECK (release_mbid IS NULL)")
                .await
                .unwrap();
        }
        let mut sealer = Sealer {
            fail: mode == 0,
            ..Default::default()
        };
        let mut tx = app.transaction().await.unwrap();
        let result = write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry),
        )
        .await;
        assert_eq!(
            result,
            Err([WriteError::Seal, WriteError::Conflict, WriteError::Store][mode])
        );
        // Even an erroneous caller commit cannot retain a partially written album.
        tx.commit().await.unwrap();
        assert_eq!(counts(&app).await, (0, 0, if mode == 1 { 1 } else { 0 }));
    }
}

#[tokio::test]
async fn writer_refuses_wrong_job_state_identity_incomplete_evidence_and_mirror_conflicts() {
    let (mut app, original, cp, recordings, geometry) = fixture().await;
    let mut sealer = Sealer::default();
    for status in [
        "queued",
        "needs_review",
        "needs_archive_read",
        "rejected",
        "done",
        "failed",
    ] {
        app.execute("UPDATE ingest_jobs SET status = $1", &[&status])
            .await
            .unwrap();
        let mut tx = app.transaction().await.unwrap();
        assert_eq!(
            write_album(
                &mut tx,
                &mut sealer,
                &write(&original, &cp, &recordings, &geometry)
            )
            .await,
            Err(WriteError::Fence)
        );
        tx.commit().await.unwrap();
    }
    app.execute(
        "UPDATE ingest_jobs SET status = 'running', source = 'pipeline'",
        &[],
    )
    .await
    .unwrap();
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Err(WriteError::Fence)
    );
    tx.commit().await.unwrap();
    app.execute(
        "UPDATE ingest_jobs SET source = 'admin', content_sha256 = $1",
        &[&"b".repeat(64)],
    )
    .await
    .unwrap();
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Err(WriteError::Fence)
    );
    tx.commit().await.unwrap();
    app.execute(
        "UPDATE ingest_jobs SET content_sha256 = substr(idempotency_key,7)",
        &[],
    )
    .await
    .unwrap();
    for mode in 0..5 {
        let mut bad = cp.clone();
        let mut bad_geometry = vec![FileGeometry {
            file_ordinal: 0,
            decoded_bytes: 64,
            range_hint: None,
        }];
        let mut bad_recordings = recordings.clone();
        match mode {
            0 => bad.probe_tally = None,
            1 => bad.probe_tally.as_mut().unwrap().total_batches += 1,
            2 => bad.admin_release_mbid = None,
            3 => bad_geometry[0].decoded_bytes = 0,
            _ => bad_recordings.clear(),
        }
        let mut tx = app.transaction().await.unwrap();
        assert_eq!(
            write_album(
                &mut tx,
                &mut sealer,
                &write(&original, &bad, &bad_recordings, &bad_geometry)
            )
            .await,
            Err(WriteError::Invalid)
        );
        tx.commit().await.unwrap();
    }
    app.execute("INSERT INTO tracks (mbid,release_id,title,medium_no,track_no) VALUES ($1::text::uuid,$2::text::uuid,'First title',1,1)", &[&OTHER_RELEASE,&RELEASE]).await.unwrap();
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Err(WriteError::Conflict)
    );
    tx.commit().await.unwrap();
    assert_eq!(sealer.calls, 0);
    assert_eq!(counts(&app).await, (1, 0, 0));
}

#[tokio::test]
async fn cancelled_writer_rolls_back_its_savepoint_and_outer_rollback_discards_success() {
    let (mut app, original, cp, recordings, geometry) = fixture().await;
    let mut sealer = Sealer {
        pending: true,
        ..Default::default()
    };
    let mut tx = app.transaction().await.unwrap();
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(3),
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
    )
    .await
    .is_err());
    assert_eq!(sealer.calls, 1);
    tx.commit().await.unwrap();
    assert_eq!(counts(&app).await, (0, 0, 0));
    sealer.pending = false;
    let mut tx = app.transaction().await.unwrap();
    assert_eq!(
        write_album(
            &mut tx,
            &mut sealer,
            &write(&original, &cp, &recordings, &geometry)
        )
        .await,
        Ok(1)
    );
    tx.rollback().await.unwrap();
    assert_eq!(counts(&app).await, (0, 0, 0));
}
