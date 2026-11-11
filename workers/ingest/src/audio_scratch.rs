//! Bound direct-file scratch reader for manifest candidate production.
//!
//! This adapter is not mounted by Matching yet. It consumes only assembled,
//! healthy files from one verified round; spools are refused, not omitted.
//! Scratch metadata is internal verification evidence, never mirror authority.

use std::future::Future;

use futures_util::{future::LocalBoxFuture, stream::LocalBoxStream, StreamExt};

use crate::{
    audio_selection::{AudioSelectionPlan, DirectAudioSelection, PrefixReader, SelectionError},
    audio_tags::MAX_TAG_PREFIX_BYTES,
    jobs::{validate_job_id, MAX_REPAIR_ROUNDS},
    pool_deadline::race_against_deadline,
    verify::{StagedFile, VerifyProgress},
};

const MAX_ASSEMBLED_BYTES: u64 = 32 << 20;
const MAX_PREFIX_CHUNKS: usize = 4096;
pub const SCRATCH_PREFIX_DEADLINE_MS: u64 = 30_000;

/// No Debug: object keys and private source selectors stay out of diagnostics.
pub struct ScratchPrefixResponse<'a> {
    pub staged: StagedFile,
    pub object_size: u64,
    pub range_offset: u64,
    pub range_length: u64,
    pub body: LocalBoxStream<'a, Result<Vec<u8>, ()>>,
}

/// The returned body must cancel on drop. One response binds metadata and bytes
/// atomically; a separate HEAD followed by an unconditional GET is not enough.
pub trait ScratchPrefixStore {
    fn read<'a>(
        &'a self,
        expected: &'a StagedFile,
        length: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>>;
}

pub struct ScratchAudioReader<S, D> {
    plan: AudioSelectionPlan,
    staged: Vec<StagedFile>,
    store: S,
    deadline: D,
}

impl<S: ScratchPrefixStore, D: Future<Output = ()> + Unpin> ScratchAudioReader<S, D> {
    /// Recheck the original before deriving any scratch address. The caller
    /// supplies the accepted digest and the completed round's private progress,
    /// not an arbitrary key or a client-supplied URL. No I/O occurs on refusal.
    pub fn bind(
        job_id: &str,
        original: &str,
        expected_hash: &str,
        progress: &VerifyProgress,
        store: S,
        deadline: D,
    ) -> Result<Self, SelectionError> {
        let plan = AudioSelectionPlan::from_original(original, expected_hash)?;
        if !validate_job_id(job_id)
            || progress.round == 0
            || progress.round > MAX_REPAIR_ROUNDS + 1
            || progress.missing_blocks != 0
            || progress.size_refused_blocks != 0
            || !progress.judging.is_empty()
        {
            return Err(SelectionError::Identity);
        }
        if !progress.spool.is_empty() {
            return Err(SelectionError::Unsupported);
        }
        let document =
            nntune_core::nzb::parse(original.as_bytes()).map_err(|_| SelectionError::Invalid)?;
        let mut staged = Vec::with_capacity(plan.selections().len());
        for selection in plan.selections() {
            let index = selection.selector().nzb_file_ordinal;
            let file = document.files.get(index).ok_or(SelectionError::Identity)?;
            let record = progress
                .staged
                .get(&index)
                .ok_or(SelectionError::Identity)?;
            if !record.spool.is_empty() {
                return Err(SelectionError::Unsupported);
            }
            let name = file.filename.as_deref().unwrap_or(&file.subject);
            if record.key != VerifyProgress::file_key(job_id, progress.round, index, name)
                || record.size == 0
                || record.size > MAX_ASSEMBLED_BYTES
                || record.sha256.len() != 64
                || !record
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || record.missing_blocks != 0
                || record.size_refused_blocks != 0
                || record.missing
                || record.crc32 != 0
            {
                return Err(SelectionError::Identity);
            }
            staged.push(record.clone());
        }
        Ok(Self {
            plan,
            staged,
            store,
            deadline,
        })
    }

    pub fn plan(&self) -> &AudioSelectionPlan {
        &self.plan
    }
}

impl<S: ScratchPrefixStore, D: Future<Output = ()> + Unpin> PrefixReader
    for ScratchAudioReader<S, D>
{
    fn prefix<'a>(
        &'a mut self,
        selection: &'a DirectAudioSelection,
        cap: usize,
    ) -> LocalBoxStream<'a, Result<Vec<u8>, ()>> {
        futures_util::stream::once(async move {
            if cap == 0
                || cap > MAX_TAG_PREFIX_BYTES
                || selection.original_content_sha256() != self.plan.original_content_sha256()
            {
                return Err(());
            }
            let index = self
                .plan
                .selections()
                .iter()
                .position(|bound| {
                    bound.audio_ordinal() == selection.audio_ordinal()
                        && bound.selector() == selection.selector()
                })
                .ok_or(())?;
            let expected = &self.staged[index];
            let length = (cap as u64).min(expected.size) as usize;
            let read = self.store.read(expected, length);
            let response = race_against_deadline(read, &mut self.deadline)
                .await
                .ok_or(())??;
            if response.staged != *expected
                || response.object_size != expected.size
                || response.range_offset != 0
                || response.range_length != length as u64
            {
                return Err(());
            }
            let mut body = response.body;
            let mut prefix = Vec::new();
            let mut chunks = 0;
            while prefix.len() < length {
                let next = Box::pin(body.next());
                let chunk = race_against_deadline(next, &mut self.deadline)
                    .await
                    .ok_or(())?
                    .ok_or(())??;
                chunks += 1;
                if chunks > MAX_PREFIX_CHUNKS || chunk.len() > length - prefix.len() {
                    return Err(());
                }
                prefix.extend_from_slice(&chunk);
            }
            // No EOF poll after the exact range: dropping the stream cancels
            // any unread tail, including a store that ignored its range.
            Ok(prefix)
        })
        .boxed_local()
    }
}

#[cfg(test)]
#[test]
fn r2_metadata_requires_exact_verification_fields_without_legacy_defaults() {
    let expected = StagedFile {
        key: "private-scratch-key".into(),
        size: 42,
        sha256: "a".repeat(64),
        missing_blocks: 0,
        size_refused_blocks: 0,
        par2_set_id: Some("set".into()),
        spool: vec![],
        crc32: 0,
        missing: false,
    };
    let good = std::collections::HashMap::from([
        ("size".to_owned(), "42".to_owned()),
        ("sha256".to_owned(), expected.sha256.clone()),
        ("missing_blocks".to_owned(), "0".to_owned()),
        ("size_refused_blocks".to_owned(), "0".to_owned()),
        ("crc32".to_owned(), "0".to_owned()),
        ("par2_set_id".to_owned(), "set".to_owned()),
    ]);
    assert!(metadata_matches(&good, &expected));
    for key in good.keys() {
        let mut absent = good.clone();
        absent.remove(key);
        assert!(!metadata_matches(&absent, &expected), "missing {key}");
        let mut changed = good.clone();
        changed.insert(key.clone(), "unparseable".into());
        assert!(!metadata_matches(&changed, &expected), "changed {key}");
    }
    let mut missing = good.clone();
    missing.insert("missing".into(), "1".into());
    assert!(!metadata_matches(&missing, &expected));
    let mut no_set = expected.clone();
    no_set.par2_set_id = None;
    assert!(!metadata_matches(&good, &no_set));
    let mut metadata = good;
    metadata.remove("par2_set_id");
    assert!(metadata_matches(&metadata, &no_set));
}

/// R2 implementation using one ranged GET, not a buffered whole-object read.
/// Construction adds no route or network call. A future producer passes one
/// Delay for the whole extraction, never a fresh timeout for every file/chunk.
pub struct R2ScratchPrefixes {
    bucket: worker::Bucket,
}

impl R2ScratchPrefixes {
    pub fn new(bucket: worker::Bucket) -> Self {
        Self { bucket }
    }
}

fn metadata_matches(
    metadata: &std::collections::HashMap<String, String>,
    expected: &StagedFile,
) -> bool {
    metadata.get("sha256") == Some(&expected.sha256)
        && metadata.get("size") == Some(&expected.size.to_string())
        && metadata.get("missing_blocks").map(String::as_str) == Some("0")
        && metadata.get("size_refused_blocks").map(String::as_str) == Some("0")
        && metadata.get("crc32").map(String::as_str) == Some("0")
        && !metadata.contains_key("missing")
        && metadata.get("par2_set_id") == expected.par2_set_id.as_ref()
}

impl ScratchPrefixStore for R2ScratchPrefixes {
    fn read<'a>(
        &'a self,
        expected: &'a StagedFile,
        length: usize,
    ) -> LocalBoxFuture<'a, Result<ScratchPrefixResponse<'a>, ()>> {
        Box::pin(async move {
            if length == 0 || length > MAX_TAG_PREFIX_BYTES || length as u64 > expected.size {
                return Err(());
            }
            let object = self
                .bucket
                .get(expected.key.clone())
                .range(worker::Range::OffsetWithLength {
                    offset: 0,
                    length: length as u64,
                })
                .execute()
                .await
                .map_err(|_| ())?
                .ok_or(())?;
            // Acquire the cancelling reader before inspecting metadata: even
            // a rejected response must release/cancel its body, not leak it.
            let body = object.body().ok_or(())?.stream().map_err(|_| ())?;
            let metadata = object.custom_metadata().map_err(|_| ())?;
            if !metadata_matches(&metadata, expected) {
                return Err(());
            }
            let (range_offset, range_length) = match object.range().map_err(|_| ())? {
                worker::Range::OffsetWithLength { offset, length } => (offset, length),
                worker::Range::Prefix { length } => (0, length),
                _ => return Err(()),
            };
            Ok(ScratchPrefixResponse {
                staged: expected.clone(),
                object_size: object.size(),
                range_offset,
                range_length,
                body: body.map(|chunk| chunk.map_err(|_| ())).boxed_local(),
            })
        })
    }
}
