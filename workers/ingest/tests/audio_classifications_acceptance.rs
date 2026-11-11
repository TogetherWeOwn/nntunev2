//! Classification acceptance orchestration: identity gate, read-first winner,
//! evidence retrieval only on a confirmed miss, classification, and the
//! freezer's winner (never the candidate) as the sole result. A recording
//! fake store stands in for the database; no I/O occurs.
use std::cell::RefCell;

use nntune_ingest::{
    audio_classifications::{classify_entries, ClassError, ClassKind, MirrorEvidence},
    audio_classifications_acceptance::{
        accept_classifications, ClassificationAcceptanceError, ClassificationAcceptanceStore,
    },
    audio_classifications_store::StoreError,
    audio_manifest::AcceptedAudioManifest,
};
use serde_json::json;

const JOB: &str = "job_0123456789abcdef";
const OWNER: &str = "owner-1";
const RECORDING: &str = "aaaaaaaa-1111-4111-8111-111111111111";
const RELEASE: &str = "cccccccc-3333-4333-8333-333333333333";
const SNAPSHOT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn hash() -> String {
    "a".repeat(64)
}

fn entry_ordinal(ordinal: u32, title: &str) -> serde_json::Value {
    json!({
        "audio_ordinal": ordinal,
        "source_selector": {
            "nzb_file_ordinal": ordinal - 1,
            "archive_member": null,
            "audio_selection_index": 0,
        },
        "artist_group_ref": 1,
        "release_group_ref": 1,
        "hints": {"title": title},
    })
}

fn manifest_for(
    job: &str,
    user: &str,
    digest: &str,
    titles: &[&str],
    frozen: bool,
) -> AcceptedAudioManifest {
    let entries: Vec<serde_json::Value> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| entry_ordinal(i as u32 + 1, title))
        .collect();
    let mut value = json!({
        "version": 1, "job_id": job, "user_id": user,
        "original_content_sha256": digest,
        "entries": entries,
        "art_blobs": []
    });
    if frozen {
        value["effective_discs"] = json!(vec![1; titles.len()]);
    }
    AcceptedAudioManifest::parse(&value.to_string()).unwrap()
}

fn manifest(titles: &[&str]) -> AcceptedAudioManifest {
    manifest_for(JOB, OWNER, &hash(), titles, true)
}

fn checkpoint(
    manifest: &AcceptedAudioManifest,
    evidence: &[Option<MirrorEvidence>],
) -> nntune_ingest::audio_classifications::ClassificationCheckpoint {
    classify_entries(manifest, evidence).unwrap()
}

fn verified(recording: &str) -> MirrorEvidence {
    MirrorEvidence {
        recording_mbid: recording.into(),
        release_mbid: RELEASE.into(),
        snapshot_sha256: SNAPSHOT.into(),
        medium_no: 2,
        track_no: 5,
    }
}

struct FakeStore<'a> {
    calls: &'a RefCell<Vec<&'static str>>,
    load:
        Result<Option<nntune_ingest::audio_classifications::ClassificationCheckpoint>, StoreError>,
    freeze: Result<nntune_ingest::audio_classifications::ClassificationCheckpoint, StoreError>,
    frozen_candidates: RefCell<Vec<nntune_ingest::audio_classifications::ClassificationCheckpoint>>,
}

impl<'a> FakeStore<'a> {
    fn new(
        calls: &'a RefCell<Vec<&'static str>>,
        load: Result<
            Option<nntune_ingest::audio_classifications::ClassificationCheckpoint>,
            StoreError,
        >,
        freeze: Result<nntune_ingest::audio_classifications::ClassificationCheckpoint, StoreError>,
    ) -> Self {
        Self {
            calls,
            load,
            freeze,
            frozen_candidates: RefCell::new(Vec::new()),
        }
    }
}

impl ClassificationAcceptanceStore for FakeStore<'_> {
    async fn load(
        &self,
        _job_id: &str,
        _user_id: &str,
        _original_hash: &str,
    ) -> Result<Option<nntune_ingest::audio_classifications::ClassificationCheckpoint>, StoreError>
    {
        self.calls.borrow_mut().push("load");
        self.load.clone()
    }

    async fn freeze(
        &self,
        candidate: &nntune_ingest::audio_classifications::ClassificationCheckpoint,
        _job_id: &str,
        _user_id: &str,
        _original_hash: &str,
    ) -> Result<nntune_ingest::audio_classifications::ClassificationCheckpoint, StoreError> {
        self.calls.borrow_mut().push("freeze");
        self.frozen_candidates.borrow_mut().push(candidate.clone());
        self.freeze.clone()
    }
}

async fn run(
    store: &FakeStore<'_>,
    manifest: &AcceptedAudioManifest,
    calls: &RefCell<Vec<&'static str>>,
    produced: Result<Vec<Option<MirrorEvidence>>, ClassError>,
) -> Result<
    nntune_ingest::audio_classifications::ClassificationCheckpoint,
    ClassificationAcceptanceError,
> {
    accept_classifications(store, manifest, JOB, OWNER, &hash(), || async {
        calls.borrow_mut().push("produce");
        produced
    })
    .await
}

#[tokio::test]
async fn stored_winner_is_returned_without_producer_or_freeze() {
    let winner = checkpoint(&manifest(&["Stored"]), &[None]);
    let calls = RefCell::new(Vec::new());
    let store = FakeStore::new(
        &calls,
        Ok(Some(winner.clone())),
        Ok(checkpoint(&manifest(&["Never"]), &[None])),
    );
    let got = run(&store, &manifest(&["Candidate"]), &calls, Ok(vec![None])).await;
    assert_eq!(got, Ok(winner));
    assert_eq!(*calls.borrow(), ["load"]);
}

#[tokio::test]
async fn stored_verified_winner_skips_evidence_retrieval() {
    let winner = checkpoint(&manifest(&["Stored"]), &[Some(verified(RECORDING))]);
    let calls = RefCell::new(Vec::new());
    let store = FakeStore::new(
        &calls,
        Ok(Some(winner.clone())),
        Ok(checkpoint(&manifest(&["Never"]), &[None])),
    );
    let got = run(&store, &manifest(&["Candidate"]), &calls, Ok(vec![None]))
        .await
        .unwrap();
    assert!(matches!(got.entries()[0].kind, ClassKind::Verified));
    assert_eq!(*calls.borrow(), ["load"]);
}

#[tokio::test]
async fn read_error_never_invokes_producer_or_falls_back_to_freeze() {
    for error in [
        StoreError::MissingOrFenced,
        StoreError::InvalidStored,
        StoreError::Permanent,
        StoreError::Unavailable,
    ] {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(
            &calls,
            Err(error),
            Ok(checkpoint(&manifest(&["Never"]), &[None])),
        );
        let got = run(&store, &manifest(&["Candidate"]), &calls, Ok(vec![None])).await;
        assert_eq!(got, Err(ClassificationAcceptanceError::Store(error)));
        assert_eq!(*calls.borrow(), ["load"]);
    }
}

#[tokio::test]
async fn miss_produces_classifies_freezes_and_returns_the_frozen_winner() {
    let calls = RefCell::new(Vec::new());
    let winner = checkpoint(&manifest(&["Theirs"]), &[Some(verified(RECORDING))]);
    let store = FakeStore::new(&calls, Ok(None), Ok(winner.clone()));
    let got = run(&store, &manifest(&["Mine"]), &calls, Ok(vec![None])).await;
    assert_eq!(got, Ok(winner.clone()));
    assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
    let frozen = store.frozen_candidates.borrow();
    assert_eq!(frozen.len(), 1);
    assert_ne!(frozen[0], winner);
    assert!(matches!(frozen[0].entries()[0].kind, ClassKind::AllLocal));
}

#[tokio::test]
async fn producer_failure_does_not_freeze() {
    let calls = RefCell::new(Vec::new());
    let store = FakeStore::new(
        &calls,
        Ok(None),
        Ok(checkpoint(&manifest(&["Never"]), &[None])),
    );
    let got = run(
        &store,
        &manifest(&["Candidate"]),
        &calls,
        Err(ClassError::Evidence),
    )
    .await;
    assert_eq!(
        got,
        Err(ClassificationAcceptanceError::Classify(
            ClassError::Evidence
        ))
    );
    assert_eq!(*calls.borrow(), ["load", "produce"]);
}

#[tokio::test]
async fn classification_refusal_does_not_freeze() {
    // Evidence length mismatch and mixed verified/all-local in one release
    // both refuse the whole checkpoint before any freeze.
    let cases = [
        (vec!["A"], vec![]),
        (vec!["A", "B"], vec![Some(verified(RECORDING)), None]),
    ];
    for (titles, evidence) in cases {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(
            &calls,
            Ok(None),
            Ok(checkpoint(&manifest(&["Never"]), &[None])),
        );
        let manifest = manifest(&titles);
        let got = run(&store, &manifest, &calls, Ok(evidence)).await;
        assert!(matches!(
            got,
            Err(ClassificationAcceptanceError::Classify(_))
        ));
        assert_eq!(*calls.borrow(), ["load", "produce"]);
    }
}

#[tokio::test]
async fn winners_with_wrong_identity_from_load_or_freeze_are_invalid() {
    let wrong = [
        checkpoint(
            &manifest_for("other_job", OWNER, &hash(), &["x"], true),
            &[None],
        ),
        checkpoint(
            &manifest_for(JOB, "other_owner", &hash(), &["x"], true),
            &[None],
        ),
        checkpoint(
            &manifest_for(JOB, OWNER, &"b".repeat(64), &["x"], true),
            &[None],
        ),
    ];
    for bad in wrong {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(
            &calls,
            Ok(Some(bad.clone())),
            Ok(checkpoint(&manifest(&["Never"]), &[None])),
        );
        let got = run(&store, &manifest(&["Candidate"]), &calls, Ok(vec![None])).await;
        assert_eq!(got, Err(ClassificationAcceptanceError::InvalidWinner));
        assert_eq!(*calls.borrow(), ["load"]);

        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Ok(None), Ok(bad));
        let got = run(&store, &manifest(&["Candidate"]), &calls, Ok(vec![None])).await;
        assert_eq!(got, Err(ClassificationAcceptanceError::InvalidWinner));
        assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
    }
}

#[tokio::test]
async fn invalid_root_identity_touches_neither_store_nor_producer() {
    let long_user = "u".repeat(129);
    let bad_hash = "A".repeat(64);
    let one = manifest(&["x"]);
    let cases: [(&str, &str, &str); 6] = [
        ("", OWNER, &hash()),
        ("bad job", OWNER, &hash()),
        (JOB, "", &hash()),
        (JOB, &long_user, &hash()),
        (JOB, OWNER, "short"),
        (JOB, OWNER, &bad_hash),
    ];
    for (job, user, digest) in cases {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(
            &calls,
            Ok(None),
            Ok(checkpoint(&manifest(&["Never"]), &[None])),
        );
        let got = accept_classifications(&store, &one, job, user, digest, || async {
            calls.borrow_mut().push("produce");
            Ok(vec![None])
        })
        .await;
        assert_eq!(
            got,
            Err(ClassificationAcceptanceError::Identity),
            "{job:?} {user:?} {digest:?}"
        );
        assert!(calls.borrow().is_empty());
    }
}

#[tokio::test]
async fn manifest_identity_mismatch_is_refused_before_store() {
    for (job, user, digest) in [
        ("other_job".to_string(), OWNER.to_string(), hash()),
        (JOB.to_string(), "other_owner".to_string(), hash()),
        (JOB.to_string(), OWNER.to_string(), "b".repeat(64)),
    ] {
        let foreign = manifest_for(&job, &user, &digest, &["x"], true);
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(
            &calls,
            Ok(None),
            Ok(checkpoint(&manifest(&["Never"]), &[None])),
        );
        let got = run(&store, &foreign, &calls, Ok(vec![None])).await;
        assert_eq!(got, Err(ClassificationAcceptanceError::Identity));
        assert!(calls.borrow().is_empty());
    }
}
