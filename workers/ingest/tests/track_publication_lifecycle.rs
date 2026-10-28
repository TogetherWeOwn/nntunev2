//! Acceptance at the real matching/transcode sequencers. Only external effects
//! are replaced; the production code owns derivation, ordering and checkpointing.

use std::cell::RefCell;
use std::collections::BTreeMap;

use nntune_core::nzb::File;
use nntune_core::par2::scratch::sha256_hex;
use nntune_ingest::jobs::{JobCheckpoint, JobState};
use nntune_ingest::matching::MbRecording;
use nntune_ingest::queue::{LibraryWrite, QueueMsg};
use nntune_ingest::queue_consumer::{
    run_match_write_with, run_transcode_step_with, DetailedTrackMatch, MatchWriteRuntime,
    TranscodeRuntime,
};
use nntune_ingest::retry::{
    JobError, ERR_ACCOUNT_DELETING, ERR_MATCH_FAILED, ERR_TRANSCODE_FAILED, ERR_WRITE_FAILED,
};
use nntune_ingest::track_writers::TrackRow;
use nntune_ingest::transcode::{
    ArtifactFetchError, PublishError, PublishOutcome, TranscodeCall, TranscodeInput,
    TranscodePublication, TranscodeRequest,
};
use nntune_ingest::writer_execution::{MediaPublication, PublicationContext};
use serde_json::{json, Value};

const OWNER: &str = "11000000-0000-4000-8000-000000000001";
const JOB: &str = "22000000-0000-4000-8000-000000000002";
const PROVIDER: &str = "33000000-0000-4000-8000-000000000003";
const STALE_PROVIDER: &str = "44000000-0000-4000-8000-000000000004";
const RELEASE: &str = "55000000-0000-4000-8000-000000000005";
const RECORDING_A: &str = "66000000-0000-4000-8000-000000000006";
const RECORDING_B: &str = "77000000-0000-4000-8000-000000000007";
const TRACK_A: &str = "88000000-0000-4000-8000-000000000008";
const TRACK_B: &str = "99000000-0000-4000-8000-000000000009";
const VERIFIED_AT: i64 = 1_790_000_000;

fn message() -> QueueMsg {
    serde_json::from_value(json!({
        "job_id": JOB, "user_id": OWNER,
        "idempotency_key": format!("ingest:{OWNER}:release:fixture"),
        "source": "nzb_upload", "state": "queued", "attempt": 1,
        "payload_ref": format!("nzb:sha256:{}", "c".repeat(64)),
        "provider_id": STALE_PROVIDER
    }))
    .unwrap()
}

fn input(ordinal: usize) -> TranscodeInput {
    TranscodeInput {
        file_index: ordinal,
        key: format!("ingest-scratch/{JOB}/r1/in/{ordinal:04}-{}", "a".repeat(64)),
        sha256: "b".repeat(64),
        size: 512,
        scratch_prefix: format!("ingest-scratch/{JOB}/r1/"),
    }
}

fn checkpoint(state: JobState, ordinals: &[usize], association: Value) -> JobCheckpoint {
    let msg = message();
    // JSON intentionally accepts the future field before the source adds it:
    // an ignored association is an assertion failure, never a compiler error.
    serde_json::from_value(json!({
        "job_id": msg.job_id, "user_id": msg.user_id,
        "idempotency_key": msg.idempotency_key, "source": msg.source,
        "state": state, "attempt": 1, "failure_reason": null,
        "provider_id": PROVIDER, "track_ids": association,
        "transcode_inputs": ordinals.iter().copied().map(input).collect::<Vec<_>>()
    }))
    .unwrap()
}

fn selected_ids() -> Value {
    json!({"2": TRACK_A, "7": TRACK_B})
}

fn association(cp: &JobCheckpoint) -> Value {
    serde_json::to_value(cp).unwrap()["track_ids"].clone()
}

fn file(ordinal: usize, role: &str) -> File {
    File {
        ordinal,
        subject: format!("\"misleading-name-{ordinal}.flac\" yEnc"),
        filename: Some(format!("misleading-name-{ordinal}.flac")),
        role: role.into(),
        container: "flac".into(),
        posted_at_unix: 1_700_000_000,
        segments: Vec::new(),
    }
}

fn detailed_match() -> DetailedTrackMatch {
    DetailedTrackMatch {
        release_mbid: Some(RELEASE.into()),
        // Out of order, with non-content files: cursor 0/1 is not NZB 2/7.
        files: vec![file(7, "content"), file(0, "nfo"), file(2, "content")],
        recordings: vec![
            MbRecording {
                mbid: RECORDING_A.into(),
                title: "  First recording  ".into(),
                medium_no: Some(1),
                track_no: Some(3),
            },
            MbRecording {
                mbid: RECORDING_B.into(),
                title: "Second recording".into(),
                medium_no: Some(2),
                track_no: Some(4),
            },
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedTrack {
    owner: String,
    release: String,
    title: String,
    medium: i32,
    track: i32,
    recording: String,
    ordinal: usize,
}

fn expected_tracks() -> Vec<CapturedTrack> {
    vec![
        CapturedTrack {
            owner: OWNER.into(),
            release: RELEASE.into(),
            title: "First recording".into(),
            medium: 1,
            track: 3,
            recording: RECORDING_A.into(),
            ordinal: 2,
        },
        CapturedTrack {
            owner: OWNER.into(),
            release: RELEASE.into(),
            title: "Second recording".into(),
            medium: 2,
            track: 4,
            recording: RECORDING_B.into(),
            ordinal: 7,
        },
    ]
}

struct MatchEffects {
    calls: RefCell<Vec<String>>,
    rows: RefCell<Vec<CapturedTrack>>,
    detailed: DetailedTrackMatch,
    ids: BTreeMap<usize, String>,
    resolve_error: Option<JobError>,
    materialize_error: Option<JobError>,
    fail_meter: Option<&'static str>,
}

impl MatchEffects {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            detailed: detailed_match(),
            ids: BTreeMap::from([(2, TRACK_A.into()), (7, TRACK_B.into())]),
            resolve_error: None,
            materialize_error: None,
            fail_meter: None,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl MatchWriteRuntime for MatchEffects {
    async fn resolve(&self, msg: &QueueMsg) -> Result<LibraryWrite, JobError> {
        self.calls.borrow_mut().push("obsolete:resolve".into());
        Ok(nntune_ingest::matching::library_write_for(
            msg,
            None,
            Vec::new(),
        ))
    }

    async fn upsert(&self, _: &LibraryWrite) -> Result<(), JobError> {
        self.calls
            .borrow_mut()
            .push("obsolete:library-upsert".into());
        Ok(())
    }

    async fn meter(&self, _: &QueueMsg, stage: &'static str) -> Result<(), ()> {
        self.calls.borrow_mut().push(format!("meter:{stage}"));
        if self.fail_meter == Some(stage) {
            Err(())
        } else {
            Ok(())
        }
    }

    async fn resolve_tracks(&self, _: &QueueMsg) -> Result<DetailedTrackMatch, JobError> {
        self.calls.borrow_mut().push("resolve:tracks".into());
        if let Some(err) = &self.resolve_error {
            return Err(err.clone());
        }
        Ok(self.detailed.clone())
    }

    async fn materialize_tracks(
        &self,
        owner: &str,
        rows: &[TrackRow<'_>],
    ) -> Result<BTreeMap<usize, String>, JobError> {
        self.calls.borrow_mut().push("materialize:tracks".into());
        self.rows
            .borrow_mut()
            .extend(rows.iter().map(|row| CapturedTrack {
                owner: owner.into(),
                release: row.release_id.into(),
                title: row.title.into(),
                medium: row.medium_no,
                track: row.track_no,
                recording: row.mbid.into(),
                ordinal: row.file_ordinal,
            }));
        // An error may follow a committed prefix. The orchestrator must not
        // treat that prefix as a complete durable association or spend a meter.
        if let Some(err) = &self.materialize_error {
            return Err(err.clone());
        }
        Ok(self.ids.clone())
    }
}

#[tokio::test]
async fn matching_derives_full_metadata_and_persists_db_ids_by_original_ordinal() {
    let mut cp = checkpoint(JobState::Matching, &[2, 5, 7], Value::Null);
    let runtime = MatchEffects::new();
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(*runtime.rows.borrow(), expected_tracks());
    assert_eq!(
        runtime.calls(),
        ["resolve:tracks", "materialize:tracks", "meter:match"]
    );
    assert_eq!(association(&cp), selected_ids());
    assert_eq!(cp.transcode_inputs, vec![input(2), input(7)]);
    assert_eq!(cp.state, JobState::Transcoding);
    assert!(err.transient && err.made_progress);
    assert_eq!(err.code, ERR_TRANSCODE_FAILED);
    let restored: JobCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&cp).unwrap()).unwrap();
    assert_eq!(association(&restored), selected_ids());
}

async fn assert_materializer_refusal(transient: bool) {
    let mut cp = checkpoint(JobState::Matching, &[2, 7], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.materialize_error = Some(if transient {
        JobError::transient(ERR_WRITE_FAILED, "writer transport")
    } else {
        JobError::permanent(ERR_ACCOUNT_DELETING, "writer fence closed")
    });
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ["resolve:tracks", "materialize:tracks"]);
    assert_eq!(*runtime.rows.borrow(), expected_tracks());
    assert_eq!(cp.state, JobState::Matching);
    assert!(association(&cp).is_null());
    assert_eq!(cp.transcode_inputs, vec![input(2), input(7)]);
    assert_eq!(err.transient, transient);
    assert!(!err.made_progress);
}

#[tokio::test]
async fn matching_materializer_fence_refusal_never_meters_or_saves_a_partial_association() {
    assert_materializer_refusal(false).await;
}

#[tokio::test]
async fn matching_materializer_transient_never_meters_or_saves_a_partial_association() {
    assert_materializer_refusal(true).await;
}

#[tokio::test]
async fn matching_meter_failure_retains_completed_association_before_retry() {
    let mut cp = checkpoint(JobState::Matching, &[7, 5, 2], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.fail_meter = Some("match");
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(
        association(&cp),
        selected_ids(),
        "DB IDs must precede match metering"
    );
    assert_eq!(
        cp.transcode_inputs,
        vec![input(7), input(2)],
        "filter without renumbering/reordering inputs"
    );
    assert_eq!(cp.state, JobState::Matching);
    assert_eq!(err.code, ERR_MATCH_FAILED);
    assert!(err.transient && !err.made_progress);
}

#[tokio::test]
async fn detailed_resolution_failure_has_no_materialization_meter_or_advance() {
    let mut cp = checkpoint(JobState::Matching, &[2], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.resolve_error = Some(JobError::transient(ERR_MATCH_FAILED, "metadata transport"));
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ["resolve:tracks"]);
    assert_eq!(cp.state, JobState::Matching);
    assert!(association(&cp).is_null());
    assert!(err.transient && !err.made_progress);
}

#[tokio::test]
async fn partial_materialization_replay_converges_to_the_complete_db_association() {
    let mut cp = checkpoint(JobState::Matching, &[2, 7], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.materialize_error = Some(JobError::transient(ERR_WRITE_FAILED, "second row failed"));
    let first = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(first.transient && !first.made_progress);
    assert_eq!(cp.state, JobState::Matching);
    assert!(association(&cp).is_null());
    runtime.materialize_error = None;
    runtime.calls.borrow_mut().clear();
    run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(association(&cp), selected_ids());
    assert_eq!(cp.state, JobState::Transcoding);
    assert_eq!(
        runtime.calls(),
        ["resolve:tracks", "materialize:tracks", "meter:match"]
    );
}

async fn assert_completed_empty_match(unmatched: bool) {
    let mut cp = checkpoint(JobState::Matching, &[2, 7], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.ids.clear();
    if unmatched {
        runtime.detailed.release_mbid = None;
    } else {
        runtime
            .detailed
            .recordings
            .iter_mut()
            .for_each(|recording| recording.title.clear());
    }
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(
        association(&cp),
        json!({}),
        "completed-empty must differ from legacy/null"
    );
    assert!(cp.transcode_inputs.is_empty());
    assert!(
        runtime.rows.borrow().is_empty(),
        "never fall back to filenames"
    );
    assert!(!runtime
        .calls()
        .iter()
        .any(|call| call.starts_with("obsolete:")));
    assert!(err.made_progress);
    assert_eq!(cp.state, JobState::Transcoding);
}

#[tokio::test]
async fn unmatched_release_records_completed_empty_without_fabricating_tracks() {
    assert_completed_empty_match(true).await;
}

#[tokio::test]
async fn unbacked_recordings_record_completed_empty_without_filename_fallback() {
    assert_completed_empty_match(false).await;
}

#[tokio::test]
async fn writing_uses_completed_normalized_association_without_legacy_library_resolution() {
    for ids in [selected_ids(), json!({})] {
        let mut cp = checkpoint(JobState::Writing, &[], ids.clone());
        let runtime = MatchEffects::new();
        run_match_write_with(&runtime, &message(), &mut cp)
            .await
            .unwrap();
        assert_eq!(runtime.calls(), ["meter:write"]);
        assert_eq!(cp.state, JobState::Done);
        assert_eq!(association(&cp), ids);
    }
}

#[tokio::test]
async fn writing_meter_refusal_leaves_normalized_checkpoint_resumable() {
    let mut cp = checkpoint(JobState::Writing, &[], selected_ids());
    let mut runtime = MatchEffects::new();
    runtime.fail_meter = Some("write");
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ["meter:write"]);
    assert_eq!(cp.state, JobState::Writing);
    assert_eq!(association(&cp), selected_ids());
    assert!(err.transient && !err.made_progress);
    assert_eq!(err.code, ERR_WRITE_FAILED);
}

#[tokio::test]
async fn legacy_writing_must_not_turn_missing_track_derivation_into_successful_empty() {
    let mut cp = checkpoint(JobState::Writing, &[], Value::Null);
    let runtime = MatchEffects::new();
    run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .expect_err("legacy writing must request recovery or fail, never silently ack");
    assert_ne!(
        cp.state,
        JobState::Done,
        "unmaterialized is not completed-empty"
    );
    assert!(!runtime
        .calls()
        .iter()
        .any(|call| call.starts_with("obsolete:")));
}

#[test]
fn checkpoint_json_preserves_empty_sparse_and_legacy_association_distinctions() {
    let complete = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    let empty = checkpoint(JobState::Transcoding, &[], json!({}));
    let legacy = checkpoint(JobState::Transcoding, &[], Value::Null);
    assert_eq!(association(&complete), selected_ids());
    assert_eq!(association(&empty), json!({}));
    assert!(association(&legacy).is_null());
    let mut old = serde_json::to_value(&legacy).unwrap();
    old.as_object_mut().unwrap().remove("track_ids");
    let restored: JobCheckpoint = serde_json::from_value(old).unwrap();
    assert!(association(&restored).is_null());
}

#[test]
fn metadata_fixture_has_real_derivable_rows_with_no_filename_or_mbid_as_track_id() {
    let metadata = detailed_match();
    let derived = nntune_ingest::track_writers::derive_track_rows(
        metadata.release_mbid.as_deref(),
        &metadata.files,
        &metadata.recordings,
    )
    .unwrap();
    assert_eq!(
        derived
            .rows
            .iter()
            .map(|row| (row.file_ordinal, row.title, row.medium_no, row.track_no))
            .collect::<Vec<_>>(),
        [(2, "First recording", 1, 3), (7, "Second recording", 2, 4)]
    );
    assert_ne!(TRACK_A, RECORDING_A);
    assert_ne!(TRACK_B, RECORDING_B);
}

#[tokio::test]
async fn a_skipped_recording_never_shifts_the_next_original_file_association() {
    let mut cp = checkpoint(JobState::Matching, &[2, 7], Value::Null);
    let mut runtime = MatchEffects::new();
    runtime.detailed.recordings[0].title.clear();
    runtime.ids.remove(&2);
    let err = run_match_write_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(*runtime.rows.borrow(), vec![expected_tracks().remove(1)]);
    assert_eq!(association(&cp), json!({"7": TRACK_B}));
    assert_eq!(cp.transcode_inputs, vec![input(7)]);
    assert_eq!(cp.state, JobState::Transcoding);
    assert!(err.made_progress);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    ContextTransient,
    ContextPermanent,
    FrontRetry,
    FrontPermanent,
    ArtifactMissing,
    ArtifactTransport,
    ArtifactOversize,
    ArtifactHash,
    ArtifactSize,
    Bucket,
    Put,
    BlobTransient,
    BlobPermanent,
    BlobFence,
    MediaTransient,
    MediaPermanent,
    Meter,
}

struct ArtifactEffects {
    calls: RefCell<Vec<&'static str>>,
    contexts: RefCell<Vec<(String, String, usize, String)>>,
    requests: RefCell<Vec<TranscodeRequest>>,
    puts: RefCell<Vec<(String, Vec<u8>)>>,
    blobs: RefCell<Vec<TranscodePublication>>,
    media: RefCell<Vec<MediaPublication>>,
    refusal: Option<Refusal>,
    duplicate_blob: bool,
}

impl ArtifactEffects {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            contexts: RefCell::new(Vec::new()),
            requests: RefCell::new(Vec::new()),
            puts: RefCell::new(Vec::new()),
            blobs: RefCell::new(Vec::new()),
            media: RefCell::new(Vec::new()),
            refusal: None,
            duplicate_blob: false,
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
    fn bytes(&self) -> Vec<u8> {
        b"verified opus artifact".to_vec()
    }
    fn hash(&self) -> String {
        sha256_hex(&self.bytes())
    }
}

impl TranscodeRuntime for ArtifactEffects {
    async fn validate_context(
        &self,
        msg: &QueueMsg,
        cp: &JobCheckpoint,
        item: &TranscodeInput,
    ) -> Result<PublicationContext, JobError> {
        self.calls.borrow_mut().push("context");
        self.contexts.borrow_mut().push((
            msg.user_id.clone(),
            cp.provider_id.clone().unwrap_or_default(),
            item.file_index,
            item.key.clone(),
        ));
        if self.refusal == Some(Refusal::ContextTransient) {
            return Err(JobError::transient(ERR_WRITE_FAILED, "context transport"));
        }
        if self.refusal == Some(Refusal::ContextPermanent) {
            return Err(JobError::permanent(
                ERR_ACCOUNT_DELETING,
                "context fence closed",
            ));
        }
        // This boundary only supplies the DB answer. The separate native
        // publication-context bank tests actual local and live-DB validation.
        let ids = association(cp);
        let id = ids
            .get(item.file_index.to_string())
            .and_then(Value::as_str)
            .ok_or_else(|| JobError::permanent(ERR_WRITE_FAILED, "missing association"))?;
        Ok(PublicationContext {
            track_id: id.into(),
            provider_id: cp.provider_id.clone().unwrap_or_default(),
        })
    }

    async fn call_front(&self, req: &TranscodeRequest) -> TranscodeCall {
        self.calls.borrow_mut().push("front");
        self.requests.borrow_mut().push(req.clone());
        match self.refusal {
            Some(Refusal::FrontRetry) => TranscodeCall::Retry,
            Some(Refusal::FrontPermanent) => TranscodeCall::Untranscodable,
            Some(Refusal::ArtifactHash) => TranscodeCall::Completed {
                artifact_sha256: "d".repeat(64),
                byte_size: self.bytes().len() as u64,
            },
            Some(Refusal::ArtifactSize) => TranscodeCall::Completed {
                artifact_sha256: self.hash(),
                byte_size: self.bytes().len() as u64 + 1,
            },
            _ => TranscodeCall::Completed {
                artifact_sha256: self.hash(),
                byte_size: self.bytes().len() as u64,
            },
        }
    }

    async fn fetch_artifact(&self, _: &str) -> Result<Vec<u8>, ArtifactFetchError> {
        self.calls.borrow_mut().push("fetch");
        match self.refusal {
            Some(Refusal::ArtifactMissing) => Err(ArtifactFetchError::Missing),
            Some(Refusal::ArtifactTransport) => Err(ArtifactFetchError::Transport),
            Some(Refusal::ArtifactOversize) => Err(ArtifactFetchError::Oversize),
            _ => Ok(self.bytes()),
        }
    }

    async fn put_audio(&self, key: &str, bytes: Vec<u8>) -> Result<(), ()> {
        self.calls.borrow_mut().push("put");
        self.puts.borrow_mut().push((key.into(), bytes));
        if self.refusal == Some(Refusal::Put) {
            Err(())
        } else {
            Ok(())
        }
    }

    async fn publish_cache_object(
        &self,
        publication: &TranscodePublication,
    ) -> Result<PublishOutcome, PublishError> {
        self.calls.borrow_mut().push("blob");
        self.blobs.borrow_mut().push(publication.clone());
        match self.refusal {
            Some(Refusal::BlobTransient) => Err(PublishError::Transient),
            Some(Refusal::BlobPermanent) => Err(PublishError::Deterministic),
            Some(Refusal::BlobFence) => Ok(PublishOutcome::AccountDeleting),
            _ if self.duplicate_blob => Ok(PublishOutcome::Duplicate),
            _ => Ok(PublishOutcome::Published),
        }
    }

    async fn publish_media(&self, publication: &MediaPublication) -> Result<(), JobError> {
        self.calls.borrow_mut().push("media+grant");
        self.media.borrow_mut().push(publication.clone());
        match self.refusal {
            Some(Refusal::MediaTransient) => {
                Err(JobError::transient(ERR_WRITE_FAILED, "grant transport"))
            }
            Some(Refusal::MediaPermanent) => Err(JobError::permanent(
                ERR_ACCOUNT_DELETING,
                "grant fence closed",
            )),
            _ => Ok(()),
        }
    }

    async fn meter_transcode(&self, _: &QueueMsg) -> Result<(), ()> {
        self.calls.borrow_mut().push("meter");
        if self.refusal == Some(Refusal::Meter) {
            Err(())
        } else {
            Ok(())
        }
    }

    fn audio_bucket(&self) -> Result<String, ()> {
        if self.refusal == Some(Refusal::Bucket) {
            Err(())
        } else {
            Ok("fixture-audio".into())
        }
    }

    fn now_unix_secs(&self) -> i64 {
        VERIFIED_AT
    }
}

const ALL_EFFECTS: [&str; 7] = [
    "context",
    "front",
    "fetch",
    "put",
    "blob",
    "media+grant",
    "meter",
];

fn assert_media(runtime: &ArtifactEffects, index: usize, track: &str) {
    assert_eq!(
        runtime.media.borrow()[index],
        MediaPublication {
            user_id: OWNER.into(),
            track_id: track.into(),
            source_job_id: JOB.into(),
            content_hash: runtime.hash(),
            provider_id: PROVIDER.into(),
            verified_at: VERIFIED_AT,
        }
    );
    let blobs = runtime.blobs.borrow();
    assert_eq!(blobs[index].content_hash, runtime.hash());
    assert_eq!(blobs[index].verified_at, VERIFIED_AT);
    assert_eq!(blobs[index].byte_size, runtime.bytes().len() as u64);
    assert_eq!(blobs[index].r2_bucket, "fixture-audio");
    let puts = runtime.puts.borrow();
    assert_eq!(puts[index].0, blobs[index].r2_key);
    assert_eq!(puts[index].1, runtime.bytes());
}

#[tokio::test]
async fn sparse_inputs_publish_context_bytes_blob_media_grant_meter_then_cursor() {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    let runtime = ArtifactEffects::new();
    let first = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert_media(&runtime, 0, TRACK_A);
    assert_eq!(runtime.requests.borrow()[0].input_key, input(2).key);
    assert_eq!(
        runtime.contexts.borrow()[0],
        (OWNER.into(), PROVIDER.into(), 2, input(2).key)
    );
    assert_eq!(cp.transcode_next, 1);
    assert_eq!(cp.state, JobState::Transcoding);
    assert!(first.transient && first.made_progress);
    assert_eq!(first.code, ERR_TRANSCODE_FAILED);
    runtime.calls.borrow_mut().clear();
    // A real persisted resume, not just two calls on an in-memory cursor.
    let mut restored: JobCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&cp).unwrap()).unwrap();
    let second = run_transcode_step_with(&runtime, &message(), &mut restored)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert_media(&runtime, 1, TRACK_B);
    assert_eq!(runtime.requests.borrow()[1].input_key, input(7).key);
    assert_eq!(runtime.contexts.borrow()[1].2, 7);
    assert_eq!(restored.transcode_next, 2);
    assert_eq!(restored.state, JobState::Writing);
    assert!(second.transient && second.made_progress);
    assert_eq!(second.code, ERR_WRITE_FAILED);
}

async fn assert_refusal(refusal: Refusal, expected: &[&str], transient: bool) {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    let original = cp.clone();
    let mut runtime = ArtifactEffects::new();
    runtime.refusal = Some(refusal);
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), expected, "effects after {refusal:?}");
    assert_eq!(
        cp, original,
        "a refused input must not advance after {refusal:?}"
    );
    assert_eq!(err.transient, transient, "classification after {refusal:?}");
    assert!(!err.made_progress, "refusal is not a progress continuation");
}

// Separate tests ensure a RED in one boundary does not conceal the other
// refusal cases. Each executes the real sequencer, not a test-only driver.
macro_rules! refusal_case {
    ($name:ident, $refusal:ident, $prefix:expr, $transient:expr) => {
        #[tokio::test]
        async fn $name() {
            assert_refusal(Refusal::$refusal, &ALL_EFFECTS[..$prefix], $transient).await;
        }
    };
}

refusal_case!(
    context_transient_blocks_front_and_artifacts,
    ContextTransient,
    1,
    true
);
refusal_case!(
    context_fence_blocks_front_and_artifacts,
    ContextPermanent,
    1,
    false
);
refusal_case!(front_retry_blocks_artifact_fetch, FrontRetry, 2, true);
refusal_case!(
    front_permanent_blocks_artifact_fetch,
    FrontPermanent,
    2,
    false
);
refusal_case!(
    missing_artifact_blocks_r2_and_rows,
    ArtifactMissing,
    3,
    true
);
refusal_case!(
    artifact_transport_blocks_r2_and_rows,
    ArtifactTransport,
    3,
    true
);
refusal_case!(
    oversize_artifact_blocks_r2_and_rows,
    ArtifactOversize,
    3,
    false
);
refusal_case!(
    artifact_hash_mismatch_blocks_r2_and_rows,
    ArtifactHash,
    3,
    false
);
refusal_case!(
    artifact_size_mismatch_blocks_r2_and_rows,
    ArtifactSize,
    3,
    false
);
refusal_case!(missing_bucket_blocks_r2_and_rows, Bucket, 3, true);
refusal_case!(r2_put_failure_blocks_all_rows, Put, 4, true);
refusal_case!(
    blob_transient_blocks_media_grant_and_meter,
    BlobTransient,
    5,
    true
);
refusal_case!(
    blob_permanent_blocks_media_grant_and_meter,
    BlobPermanent,
    5,
    false
);
refusal_case!(blob_fence_blocks_media_grant_and_meter, BlobFence, 5, false);
refusal_case!(
    media_grant_transient_blocks_meter_and_cursor,
    MediaTransient,
    6,
    true
);
refusal_case!(
    media_grant_fence_blocks_meter_and_cursor,
    MediaPermanent,
    6,
    false
);

#[tokio::test]
async fn meter_refusal_blocks_cursor_and_replays_identical_publication() {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    let mut runtime = ArtifactEffects::new();
    runtime.refusal = Some(Refusal::Meter);
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert!(err.transient && !err.made_progress);
    assert_eq!(cp.transcode_next, 0);
    assert_eq!(cp.state, JobState::Transcoding);
    let publication = runtime.media.borrow()[0].clone();
    runtime.refusal = None;
    runtime.duplicate_blob = true;
    runtime.calls.borrow_mut().clear();
    run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert_eq!(runtime.media.borrow()[1], publication);
    assert_eq!(runtime.requests.borrow()[0], runtime.requests.borrow()[1]);
    assert_eq!(cp.transcode_next, 1);
}

#[tokio::test]
async fn a_published_prefix_survives_second_input_grant_failure_and_replay() {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    let mut runtime = ArtifactEffects::new();
    run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(cp.transcode_next, 1);
    runtime.calls.borrow_mut().clear();
    runtime.refusal = Some(Refusal::MediaTransient);
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS[..6]);
    assert_eq!(cp.transcode_next, 1);
    assert_eq!(cp.state, JobState::Transcoding);
    assert!(err.transient && !err.made_progress);
    assert_media(&runtime, 0, TRACK_A);
    assert_media(&runtime, 1, TRACK_B);
    runtime.refusal = None;
    runtime.duplicate_blob = true;
    runtime.calls.borrow_mut().clear();
    run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert_media(&runtime, 2, TRACK_B);
    assert_eq!(cp.transcode_next, 2);
    assert_eq!(cp.state, JobState::Writing);
}

#[tokio::test]
async fn selected_db_id_is_not_the_cursor_or_recording_id() {
    let mut cp = checkpoint(JobState::Transcoding, &[7, 2], selected_ids());
    cp.transcode_next = 1;
    let runtime = ArtifactEffects::new();
    run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(runtime.calls(), ALL_EFFECTS);
    assert_media(&runtime, 0, TRACK_A);
    assert_eq!(runtime.contexts.borrow()[0].2, 2);
    assert_eq!(cp.transcode_next, 2);
}

#[tokio::test]
async fn remaining_input_without_its_association_cannot_become_empty_success() {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], json!({"2": TRACK_A}));
    cp.transcode_next = 1;
    let runtime = ArtifactEffects::new();
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(!err.transient && !err.made_progress);
    assert!(runtime.calls().iter().all(|call| *call == "context"));
    assert_eq!(cp.transcode_next, 1);
    assert_eq!(cp.state, JobState::Transcoding);
}

#[tokio::test]
async fn explicit_empty_with_remaining_input_is_not_a_successful_skip() {
    let mut cp = checkpoint(JobState::Transcoding, &[7], json!({}));
    let runtime = ArtifactEffects::new();
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(!err.transient && !err.made_progress);
    assert_eq!(cp.state, JobState::Transcoding);
    assert_eq!(cp.transcode_next, 0);
    assert!(runtime.calls().iter().all(|call| *call == "context"));
}

#[tokio::test]
async fn legacy_association_with_a_remaining_input_cannot_publish() {
    let mut cp = checkpoint(JobState::Transcoding, &[7], Value::Null);
    let runtime = ArtifactEffects::new();
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(!err.transient && !err.made_progress);
    assert_eq!(cp.state, JobState::Transcoding);
    assert_eq!(cp.transcode_next, 0);
    assert!(runtime.calls().iter().all(|call| *call == "context"));
}

#[tokio::test]
async fn completed_empty_skips_artifacts_and_reaches_terminal_without_a_grant() {
    let mut cp = checkpoint(JobState::Transcoding, &[], json!({}));
    let artifacts = ArtifactEffects::new();
    let progress = run_transcode_step_with(&artifacts, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(progress.transient && progress.made_progress);
    assert_eq!(cp.state, JobState::Writing);
    assert!(artifacts.calls().is_empty());
    let writing = MatchEffects::new();
    run_match_write_with(&writing, &message(), &mut cp)
        .await
        .unwrap();
    assert_eq!(cp.state, JobState::Done);
    assert_eq!(writing.calls(), ["meter:write"]);
    assert!(artifacts.media.borrow().is_empty());
}

#[tokio::test]
async fn legacy_empty_is_refused_not_reclassified_as_completed_empty() {
    let mut cp = checkpoint(JobState::Transcoding, &[], Value::Null);
    let runtime = ArtifactEffects::new();
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert!(!err.transient && !err.made_progress);
    assert_eq!(cp.state, JobState::Transcoding);
    assert!(runtime.calls().is_empty());
}

#[tokio::test]
async fn finished_cursor_replays_transition_without_republishing_artifacts() {
    let mut cp = checkpoint(JobState::Transcoding, &[2, 7], selected_ids());
    cp.transcode_next = 2;
    let runtime = ArtifactEffects::new();
    let err = run_transcode_step_with(&runtime, &message(), &mut cp)
        .await
        .unwrap_err();
    assert_eq!(cp.state, JobState::Writing);
    assert!(err.made_progress);
    assert!(runtime.calls().is_empty());
}
