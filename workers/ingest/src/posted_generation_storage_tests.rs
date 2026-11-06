use super::*;
use crate::flac::{FlacWalk, StreamInfo, TaggedFlac};

// Synthetic typed tag output: storage uses bytes, never sidecar assertions.
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
        sha256_hex: "not-a-trusted-digest".into(),
        art: Vec::new(),
        skipped_art: Vec::new(),
    }
}

#[derive(Default)]
struct MemoryStore {
    stored: Option<(DispatchObject, Vec<u8>)>,
    creates: usize,
    heads: usize,
    reads: Vec<(u64, usize)>,
    lost_reply: bool,
    unavailable: bool,
    bad_read: i8,
    replace_on_read: bool,
}

impl TaggedObjectStore for MemoryStore {
    async fn create(
        &mut self,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        self.creates += 1;
        if self.unavailable {
            return Err(StoreError::Database);
        }
        if self.stored.is_some() {
            return Ok(None);
        }
        let object = DispatchObject::new(id, "version-a")?;
        let size = bytes.len() as u64;
        self.stored = Some((object.clone(), bytes));
        if self.lost_reply {
            return Err(StoreError::Database);
        }
        Ok(Some(TaggedObjectMetadata { object, size }))
    }

    async fn head(&mut self, id: &str) -> Result<Option<TaggedObjectMetadata>, StoreError> {
        self.heads += 1;
        Ok(self
            .stored
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
        self.reads.push((offset, limit));
        if self.replace_on_read {
            let (object, _) = self.stored.as_mut().ok_or(StoreError::Inconsistent)?;
            *object = DispatchObject::new(object.id(), "version-b")?;
        }
        let (object, bytes) = self.stored.as_ref().ok_or(StoreError::Inconsistent)?;
        if object != expected {
            return Err(StoreError::Inconsistent);
        }
        let start = offset as usize;
        let end = (start + limit).min(bytes.len());
        let mut out = bytes
            .get(start..end)
            .ok_or(StoreError::Inconsistent)?
            .to_vec();
        if self.bad_read < 0 {
            out.pop();
        }
        if self.bad_read > 0 {
            out.push(0);
        }
        Ok(out)
    }
}

#[test]
fn tagged_key_is_framed_and_separates_job_operation_and_file() {
    let key = tagged_object_id("job-a", "op-a", 0).expect("valid key");
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    for other in [
        tagged_object_id("job-b", "op-a", 0),
        tagged_object_id("job-a", "op-b", 0),
        tagged_object_id("job-a", "op-a", 1),
        tagged_object_id("job", "a-op-a", 0),
    ] {
        assert!(other.is_ok_and(|other| other != key));
    }
    assert!(tagged_object_id("ab", "c", 0).ok() != tagged_object_id("a", "bc", 0).ok());
    for bad in ["", "a/b", "a.b"] {
        assert!(tagged_object_id(bad, "op", 0).is_err());
        assert!(tagged_object_id("job", bad, 0).is_err());
    }
    assert!(tagged_object_id("job", "op", -1).is_err());
}

#[tokio::test]
async fn tagged_storage_hashes_actual_bytes_and_replays_without_overwrite() {
    let bytes: Vec<u8> = (0..150_001).map(|i| (i % 251) as u8).collect();
    let mut store = MemoryStore::default();
    let first = freeze_tagged_file(&mut store, "job", "op", 0, tagged(bytes.clone()))
        .await
        .expect("freeze");
    let second = freeze_tagged_file(&mut store, "job", "op", 0, tagged(bytes.clone()))
        .await
        .expect("replay");
    assert!(first.object == second.object);
    assert!(first.sha256 == hex::encode(Sha256::digest(&bytes)));
    assert_eq!(first.size, bytes.len() as i64);
    assert!(store.reads.iter().all(|(_, length)| *length <= READ_BYTES));
    assert_eq!(
        store.reads,
        vec![
            (0, 65_536),
            (65_536, 65_536),
            (131_072, 18_929),
            (0, 65_536),
            (65_536, 65_536),
            (131_072, 18_929)
        ]
    );
    assert!(store
        .stored
        .as_ref()
        .is_some_and(|(_, stored)| *stored == bytes));
    assert_eq!(format!("{first:?}"), "FrozenTaggedFile([REDACTED])");
}

#[tokio::test]
async fn tagged_storage_lost_put_reply_recovers_only_identical_stored_bytes() {
    let mut store = MemoryStore {
        lost_reply: true,
        ..Default::default()
    };
    assert!(
        freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![3; 99]))
            .await
            .is_ok()
    );
    let original = store.stored.clone();
    assert!(
        freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![4; 99]))
            .await
            .is_err()
    );
    assert!(store.stored == original);
    assert!(
        freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![3; 100]))
            .await
            .is_err()
    );
    assert!(store.stored == original);
}

#[tokio::test]
async fn tagged_storage_missing_short_oversized_or_replaced_reads_refuse() {
    for mut store in [
        MemoryStore {
            unavailable: true,
            ..Default::default()
        },
        MemoryStore {
            bad_read: -1,
            ..Default::default()
        },
        MemoryStore {
            bad_read: 1,
            ..Default::default()
        },
        MemoryStore {
            replace_on_read: true,
            ..Default::default()
        },
    ] {
        assert!(
            freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![5; 100]))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn tagged_reader_refuses_foreign_plan_versions_and_bad_ranges_before_io() {
    let mut store = MemoryStore::default();
    let frozen = freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![7; 100]))
        .await
        .expect("freeze");
    let heads = store.heads;
    let object = frozen.object;
    {
        let mut reader = TaggedFileReader::new(&mut store, object.clone(), 100).expect("reader");
        let foreign = DispatchObject::new("plan-ref", "version-a").expect("plan identity");
        let wrong_version =
            DispatchObject::new(object.id(), "version-b").expect("version identity");
        for candidate in [&foreign, &wrong_version] {
            assert!(reader.read(candidate, 0, READ_BYTES).await.is_err());
        }
        for (offset, limit) in [
            (0, 0),
            (0, READ_BYTES + 1),
            (101, READ_BYTES),
            (u64::MAX, 1),
        ] {
            assert!(reader.read(&object, offset, limit).await.is_err());
        }
    }
    assert_eq!(store.heads, heads);
    assert!(TaggedFileReader::new(&mut store, object.clone(), 0).is_err());
    assert!(TaggedFileReader::new(&mut store, object, MAX_EXACT_INTEGER + 1).is_err());
}

#[tokio::test]
async fn tagged_reader_checks_replacement_and_deletion_even_at_eof() {
    let mut store = MemoryStore::default();
    let frozen = freeze_tagged_file(&mut store, "job", "op", 0, tagged(vec![9; 100]))
        .await
        .expect("freeze");
    let object = frozen.object;
    {
        let mut reader = TaggedFileReader::new(&mut store, object.clone(), 100).expect("reader");
        assert!(reader
            .read(&object, 100, READ_BYTES)
            .await
            .is_ok_and(|bytes| bytes.is_empty()));
    }
    store.stored.as_mut().expect("stored").0 =
        DispatchObject::new(object.id(), "version-b").expect("replacement");
    {
        let mut reader = TaggedFileReader::new(&mut store, object.clone(), 100).expect("reader");
        assert!(reader.read(&object, 100, READ_BYTES).await.is_err());
    }
    store.stored = None;
    let mut reader = TaggedFileReader::new(&mut store, object.clone(), 100).expect("reader");
    assert!(reader.read(&object, 100, READ_BYTES).await.is_err());
}

#[tokio::test]
async fn tagged_storage_invalid_identity_or_empty_input_never_writes() {
    let mut store = MemoryStore::default();
    assert!(
        freeze_tagged_file(&mut store, "bad/job", "op", 0, tagged(vec![1]))
            .await
            .is_err()
    );
    assert!(
        freeze_tagged_file(&mut store, "job", "op", 0, tagged(Vec::new()))
            .await
            .is_err()
    );
    assert_eq!(store.creates, 0);
    assert_eq!(store.heads, 0);
}
