//! Real private admission/API protocol/crypto/dispatch/receipt composition.
//! Object and Queue IO remain scripted: NOT platform/workerd/R2 qualification.
use super::*;
use crate::ingest_plan_service::process;
use nntune_core::preparation_plan::{PlanBytes, PlanReference, PreparationPlan, FREEZE};
use nntune_ingest::posted_generations::dispatch::{
    self,
    preparation::{self, CompositeReader, PlanService, PreparationInput},
    storage::{TaggedObjectMetadata, TaggedObjectStore},
    DispatchMode, DispatchObject, DispatchPointer, DispatchQueue, DispatchSend,
};
use nntune_ingest::posted_generations::{AttemptOutcome, StoreError};

struct Plans {
    db: Client,
    keys: Keys,
    role_drift: Option<String>,
    freezes: usize,
}
impl PlanService for Plans {
    async fn freeze(&mut self, canonical: &[u8]) -> Result<PlanReference, StoreError> {
        self.freezes += 1;
        let mut frame = PlanBytes::new(vec![FREEZE]);
        frame.extend_from_slice(canonical);
        let reply = process(&mut self.db, MODE, &self.keys, 1, &frame)
            .await
            .map_err(|_| StoreError::Inconsistent)?;
        PlanReference::decode(&reply).map_err(|_| StoreError::Inconsistent)
    }
    async fn open(
        &mut self,
        pointer: &DispatchPointer,
        reference: &PlanReference,
    ) -> Result<PlanBytes, StoreError> {
        let value = serde_json::to_value(pointer).expect("opaque pointer serialization");
        let id = value["id"].as_str().expect("pointer id");
        let frame = reference.open_request(id).expect("open frame");
        let bytes = process(&mut self.db, MODE, &self.keys, 1, &frame)
            .await
            .map_err(|_| StoreError::Inconsistent)?;
        if let Some(owner) = self.role_drift.take() {
            assert_eq!(
                self.db
                    .execute("UPDATE users SET role = 'member' WHERE id = $1", &[&owner])
                    .await
                    .quiet("role drift after open"),
                1
            );
        }
        Ok(bytes)
    }
}
#[derive(Default)]
struct Objects {
    object: Option<DispatchObject>,
    bytes: Vec<u8>,
    puts: usize,
}
impl TaggedObjectStore for Objects {
    async fn create(
        &mut self,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        self.puts += 1;
        if self.object.is_none() {
            self.object = Some(DispatchObject::new(id, "version-one")?);
            self.bytes = bytes;
        }
        self.head(id).await
    }
    async fn head(&mut self, id: &str) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        Ok(self
            .object
            .as_ref()
            .filter(|o| o.id() == id)
            .map(|o| TaggedObjectMetadata {
                object: o.clone(),
                size: self.bytes.len() as u64,
            }))
    }
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        if self.object.as_ref() != Some(object) {
            return Err(StoreError::Inconsistent);
        }
        let start = offset as usize;
        Ok(self
            .bytes
            .get(start..self.bytes.len().min(start + limit))
            .ok_or(StoreError::Inconsistent)?
            .to_vec())
    }
}
#[derive(Default)]
struct Queue(usize);
impl DispatchQueue for Queue {
    async fn send(&mut self, _: DispatchPointer) -> Result<(), StoreError> {
        self.0 += 1;
        Ok(())
    }
}
fn tagged(bytes: Vec<u8>) -> nntune_ingest::flac::TaggedFlac {
    use nntune_ingest::flac::{FlacWalk, StreamInfo, TaggedFlac};
    TaggedFlac {
        bytes,
        walk: FlacWalk {
            streaminfo: StreamInfo {
                min_block_size: 16,
                max_block_size: 16,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 44100,
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
fn input<'a>(admitted: &'a AdminIngestAttempt, bytes: &[u8]) -> PreparationInput<'a> {
    PreparationInput {
        job: &admitted.job_id,
        operation: "canonical-file",
        predecessor: 0,
        file_index: 0,
        blocks_per_article: 11,
        tagged: tagged(bytes.to_vec()),
    }
}
async fn bundle(db: &Client, job: &str) -> (PlanReference, String) {
    let row = db
        .query_one(
            "SELECT * FROM ingest_file_dispatches WHERE job_id = $1",
            &[&job],
        )
        .await
        .quiet("read dispatch");
    let id: String = row.get("id");
    let plan_id: String = row.get("plan_id");
    let version: String = row.get("plan_version");
    let digest: String = row.get("plan_sha256");
    let reference = PlanReference {
        id: plan_id,
        version,
        digest: decode_hex(&digest),
        bytes: row.get::<_, i64>("plan_bytes") as u32,
    };
    (reference, id)
}
fn decode_hex(raw: &str) -> [u8; 32] {
    let mut out = [0; 32];
    for (n, pair) in raw.as_bytes().chunks_exact(2).enumerate() {
        out[n] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    out
}
use nntune_ingest::posted_generations::dispatch::PreparedDispatchFile;
async fn real_file(
    plans: &mut Plans,
    pointer: &DispatchPointer,
    reference: &PlanReference,
    bytes: &[u8],
) -> PreparedDispatchFile {
    let canonical = plans
        .open(pointer, reference)
        .await
        .unwrap_or_else(|_| panic!("private open refused"));
    let p =
        PreparationPlan::decode(&canonical).unwrap_or_else(|_| panic!("canonical plan invalid"));
    PreparedDispatchFile::from_bytes(
        p.file_index as i32,
        DispatchObject::new(&p.object, &p.object_version).unwrap(),
        bytes,
        DispatchObject::new(&reference.id, &reference.version).unwrap(),
        &canonical,
    )
    .unwrap()
}

#[test]
fn postgres_private_preparation_composes_actual_sealed_plan_startup_and_readonly_recovery() {
    run(async {
        let (mut db, admitted, keys) = fixture().await;
        let mut plans = Plans {
            db: connect().await,
            keys,
            role_drift: None,
            freezes: 0,
        };
        let bytes = payload();
        let mut objects = Objects::default();
        let pointer =
            preparation::prepare(&mut db, input(&admitted, &bytes), &mut objects, &mut plans)
                .await
                .unwrap_or_else(|_| panic!("composition refused"))
                .expect("prepared pointer");
        assert_eq!(objects.puts, 1);
        assert_eq!(plans.freezes, 1);
        let (reference, _) = bundle(&db, &admitted.job_id).await;
        let file = real_file(&mut plans, &pointer, &reference, &bytes).await;
        let before = snapshot(&db, &admitted.job_id).await;
        let mut reader =
            CompositeReader::new(&mut objects, &mut plans, pointer.clone(), file.clone())
                .unwrap_or_else(|_| panic!("reader refused"));
        assert!(matches!(
            dispatch::recover_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &pointer,
                &mut reader
            )
            .await,
            Ok(AttemptOutcome::StaleOrMissing)
        ));
        assert!(before == snapshot(&db, &admitted.job_id).await);
        let mut queue = Queue::default();
        assert_eq!(
            dispatch::send_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &pointer,
                &mut reader,
                &mut queue
            )
            .await
            .unwrap(),
            DispatchSend::Acknowledged
        );
        assert_eq!(queue.0, 1);
        drop(reader);
        drop(db);
        let mut db = connect().await;
        let before = snapshot(&db, &admitted.job_id).await;
        let mut reader = CompositeReader::new(&mut objects, &mut plans, pointer.clone(), file)
            .unwrap_or_else(|_| panic!("reader refused"));
        assert!(matches!(
            dispatch::recover_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &pointer,
                &mut reader
            )
            .await,
            Ok(AttemptOutcome::Acquired(_))
        ));
        assert!(before == snapshot(&db, &admitted.job_id).await);
    });
}

#[test]
fn postgres_private_preparation_exact_replay_keeps_envelope_and_divergence_refuses() {
    run(async {
        let (mut db, admitted, keys) = fixture().await;
        let mut plans = Plans {
            db: connect().await,
            keys,
            role_drift: None,
            freezes: 0,
        };
        let bytes = payload();
        let mut objects = Objects::default();
        let first =
            preparation::prepare(&mut db, input(&admitted, &bytes), &mut objects, &mut plans)
                .await
                .unwrap()
                .unwrap();
        let before = snapshot(&db, &admitted.job_id).await;
        let replay =
            preparation::prepare(&mut db, input(&admitted, &bytes), &mut objects, &mut plans)
                .await
                .unwrap()
                .unwrap();
        assert!(first == replay);
        assert!(before == snapshot(&db, &admitted.job_id).await);
        let mut changed = input(&admitted, &bytes);
        changed.blocks_per_article = 12;
        assert!(
            preparation::prepare(&mut db, changed, &mut objects, &mut plans)
                .await
                .is_err()
        );
        assert!(before == snapshot(&db, &admitted.job_id).await);
    });
}

#[test]
fn postgres_private_preparation_malformed_and_foreign_provenance_write_nothing() {
    run(async {
        let (mut db, admitted, keys) = fixture().await;
        let before = snapshot(&db, &admitted.job_id).await;
        for frame in [vec![], vec![FREEZE], vec![99], vec![FREEZE; 1_048_578]] {
            assert!(process(&mut db, MODE, &keys, 1, &frame).await.is_err());
            assert!(before == snapshot(&db, &admitted.job_id).await);
        }
        let canonical = PreparationPlan {
            owner: "foreign-owner".into(),
            job: admitted.job_id.clone(),
            operation: "canonical-file".into(),
            predecessor: 0,
            original_digest: decode_hex(&admitted.content_sha256_hex),
            original_bytes: admitted.nzb_bytes,
            file_count: admitted.file_count.unwrap() as u32,
            file_index: 0,
            object: "object".into(),
            object_version: "version".into(),
            plaintext_bytes: 1,
            plaintext_digest: [0; 32],
            blocks_per_article: 1,
        }
        .encode()
        .unwrap();
        let mut frame = vec![FREEZE];
        frame.extend(canonical);
        assert!(process(&mut db, MODE, &keys, 1, &frame).await.is_err());
        assert!(before == snapshot(&db, &admitted.job_id).await);
    });
}

#[test]
fn postgres_private_preparation_open_requires_exact_dispatch_plan_version_and_digest() {
    run(async {
        let (mut db, admitted, keys) = fixture().await;
        let mut plans = Plans {
            db: connect().await,
            keys,
            role_drift: None,
            freezes: 0,
        };
        let bytes = payload();
        let mut objects = Objects::default();
        let pointer =
            preparation::prepare(&mut db, input(&admitted, &bytes), &mut objects, &mut plans)
                .await
                .unwrap()
                .unwrap();
        let (reference, id) = bundle(&db, &admitted.job_id).await;
        let before = snapshot(&db, &admitted.job_id).await;
        for n in 0..4 {
            let mut wrong = reference.clone();
            match n {
                0 => wrong.id = "foreign-plan".into(),
                1 => wrong.version = "foreign-version".into(),
                2 => wrong.digest[0] ^= 1,
                _ => wrong.bytes += 1,
            }
            let frame = wrong.open_request(&id).unwrap();
            assert!(process(&mut plans.db, MODE, &plans.keys, 1, &frame)
                .await
                .is_err());
        }
        let frame = reference.open_request("foreign-dispatch").unwrap();
        assert!(process(&mut plans.db, MODE, &plans.keys, 1, &frame)
            .await
            .is_err());
        assert!(before == snapshot(&db, &admitted.job_id).await);
        assert!(plans.open(&pointer, &reference).await.is_ok());
    });
}

#[test]
fn postgres_private_preparation_rechecks_role_after_unlocked_private_open() {
    run(async {
        let (mut db, admitted, keys) = fixture().await;
        let mut plans = Plans {
            db: connect().await,
            keys,
            role_drift: None,
            freezes: 0,
        };
        let bytes = payload();
        let mut objects = Objects::default();
        let pointer =
            preparation::prepare(&mut db, input(&admitted, &bytes), &mut objects, &mut plans)
                .await
                .unwrap()
                .unwrap();
        let (reference, _) = bundle(&db, &admitted.job_id).await;
        let file = real_file(&mut plans, &pointer, &reference, &bytes).await;
        let mut queue = Queue::default();
        let mut reader =
            CompositeReader::new(&mut objects, &mut plans, pointer.clone(), file.clone())
                .unwrap_or_else(|_| panic!("reader refused"));
        assert_eq!(
            dispatch::send_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &pointer,
                &mut reader,
                &mut queue
            )
            .await
            .unwrap(),
            DispatchSend::Acknowledged
        );
        drop(reader);
        plans.role_drift = Some(admitted.actor_user_id.clone());
        let mut reader = CompositeReader::new(&mut objects, &mut plans, pointer.clone(), file)
            .unwrap_or_else(|_| panic!("reader refused"));
        assert!(matches!(
            dispatch::recover_file_dispatch(
                &mut db,
                DispatchMode::PreparationOnly,
                &pointer,
                &mut reader
            )
            .await,
            Ok(AttemptOutcome::StaleOrMissing)
        ));
        assert_eq!(queue.0, 1);
    });
}
