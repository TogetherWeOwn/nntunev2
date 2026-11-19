//! Manifest acceptance orchestration: identity gate, read-first winner, producer
//! only on a confirmed miss, and the freezer's winner (never the candidate) as the
//! sole result. A recording fake store stands in for the database; no I/O occurs.
use std::cell::RefCell;

use nntune_ingest::{
    audio_acceptance::{accept_manifest, AcceptanceError, ManifestAcceptanceStore},
    audio_grouping::GroupingError,
    audio_manifest::AcceptedAudioManifest,
    audio_manifest_store::FreezeError,
    audio_producer::ProducerError,
};
use serde_json::json;

const JOB: &str = "job_0123456789abcdef";
const OWNER: &str = "owner-1";

fn hash() -> String {
    "a".repeat(64)
}

fn manifest_for(
    job: &str,
    user: &str,
    digest: &str,
    title: &str,
    frozen: bool,
) -> AcceptedAudioManifest {
    let mut value = json!({
        "version": 1, "job_id": job, "user_id": user,
        "original_content_sha256": digest,
        "entries": [{"audio_ordinal": 1, "source_selector": {
            "nzb_file_ordinal": 0, "archive_member": null, "audio_selection_index": 0
        }, "artist_group_ref": 1, "release_group_ref": 1, "hints": {"title": title}}],
        "art_blobs": []
    });
    if frozen {
        value["effective_discs"] = json!([1]);
    }
    AcceptedAudioManifest::parse(&value.to_string()).unwrap()
}

fn manifest(title: &str) -> AcceptedAudioManifest {
    manifest_for(JOB, OWNER, &hash(), title, true)
}

struct FakeStore<'a> {
    calls: &'a RefCell<Vec<&'static str>>,
    load: Result<Option<AcceptedAudioManifest>, FreezeError>,
    freeze: Result<AcceptedAudioManifest, FreezeError>,
    frozen_candidates: RefCell<Vec<AcceptedAudioManifest>>,
}

impl<'a> FakeStore<'a> {
    fn new(
        calls: &'a RefCell<Vec<&'static str>>,
        load: Result<Option<AcceptedAudioManifest>, FreezeError>,
        freeze: Result<AcceptedAudioManifest, FreezeError>,
    ) -> Self {
        Self {
            calls,
            load,
            freeze,
            frozen_candidates: RefCell::new(Vec::new()),
        }
    }
}

impl ManifestAcceptanceStore for FakeStore<'_> {
    async fn load(
        &self,
        _job_id: &str,
        _user_id: &str,
        _original_hash: &str,
    ) -> Result<Option<AcceptedAudioManifest>, FreezeError> {
        self.calls.borrow_mut().push("load");
        self.load.clone()
    }

    async fn freeze(
        &self,
        candidate: &AcceptedAudioManifest,
        _job_id: &str,
        _user_id: &str,
        _original_hash: &str,
    ) -> Result<AcceptedAudioManifest, FreezeError> {
        self.calls.borrow_mut().push("freeze");
        self.frozen_candidates.borrow_mut().push(candidate.clone());
        self.freeze.clone()
    }
}

async fn run(
    store: &FakeStore<'_>,
    calls: &RefCell<Vec<&'static str>>,
    produced: Result<AcceptedAudioManifest, ProducerError>,
) -> Result<AcceptedAudioManifest, AcceptanceError> {
    accept_manifest(store, JOB, OWNER, &hash(), || async {
        calls.borrow_mut().push("produce");
        produced
    })
    .await
}

struct NoScratch;

impl nntune_ingest::audio_scratch::ScratchPrefixStore for NoScratch {
    fn read<'a>(
        &'a self,
        _expected: &'a nntune_ingest::verify::StagedFile,
        _length: usize,
    ) -> futures_util::future::LocalBoxFuture<
        'a,
        Result<nntune_ingest::audio_scratch::ScratchPrefixResponse<'a>, ()>,
    > {
        panic!("durable winner must not read scratch")
    }
}

#[tokio::test]
async fn verified_composition_recovers_without_original_payload_or_verify_progress() {
    let calls = RefCell::new(Vec::new());
    let winner = manifest("Already accepted");
    let store = FakeStore::new(&calls, Ok(Some(winner.clone())), Ok(manifest("Never")));
    let progress = nntune_ingest::verify::VerifyProgress::new(0);
    let digest = hash();
    let got = nntune_ingest::audio_producer::accept_verified_manifest(
        nntune_ingest::audio_producer::VerifiedCandidateInput {
            job_id: JOB,
            user_id: OWNER,
            original: "not an NZB; accepted payload may no longer be in KV",
            expected_hash: &digest,
            progress: &progress,
        },
        NoScratch,
        std::future::pending(),
        &store,
    )
    .await;
    assert_eq!(got, Ok(winner));
    assert_eq!(*calls.borrow(), ["load"]);
}

#[tokio::test]
async fn stored_winner_is_returned_without_producer_or_freeze() {
    let calls = RefCell::new(Vec::new());
    let winner = manifest("Stored");
    let store = FakeStore::new(&calls, Ok(Some(winner.clone())), Ok(manifest("Never")));
    let got = run(&store, &calls, Ok(manifest("Candidate"))).await;
    assert_eq!(got, Ok(winner));
    assert_eq!(*calls.borrow(), ["load"]);
}

#[tokio::test]
async fn read_error_never_invokes_producer_or_falls_back_to_freeze() {
    for error in [
        FreezeError::MissingOrFenced,
        FreezeError::InvalidStored,
        FreezeError::Permanent,
        FreezeError::Unavailable,
    ] {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Err(error), Ok(manifest("Never")));
        let got = run(&store, &calls, Ok(manifest("Candidate"))).await;
        assert_eq!(got, Err(AcceptanceError::Store(error)));
        assert_eq!(*calls.borrow(), ["load"]);
    }
}

#[tokio::test]
async fn miss_produces_then_freezes_and_returns_the_frozen_winner() {
    let calls = RefCell::new(Vec::new());
    let candidate = manifest("Candidate");
    let store = FakeStore::new(&calls, Ok(None), Ok(candidate.clone()));
    let got = run(&store, &calls, Ok(candidate.clone())).await;
    assert_eq!(got, Ok(candidate.clone()));
    assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
    assert_eq!(*store.frozen_candidates.borrow(), [candidate]);
}

#[tokio::test]
async fn concurrent_winner_that_differs_from_candidate_is_the_result() {
    let calls = RefCell::new(Vec::new());
    let candidate = manifest("Mine");
    let winner = manifest("Theirs");
    assert_ne!(candidate, winner);
    let store = FakeStore::new(&calls, Ok(None), Ok(winner.clone()));
    let got = run(&store, &calls, Ok(candidate)).await;
    assert_eq!(got, Ok(winner));
    assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
}

#[tokio::test]
async fn producer_failure_does_not_freeze() {
    let calls = RefCell::new(Vec::new());
    let error = ProducerError::Grouping(GroupingError::UnsupportedText);
    let store = FakeStore::new(&calls, Ok(None), Ok(manifest("Never")));
    let got = run(&store, &calls, Err(error)).await;
    assert_eq!(got, Err(AcceptanceError::Producer(error)));
    assert_eq!(*calls.borrow(), ["load", "produce"]);
}

#[tokio::test]
async fn candidate_identity_mismatch_is_refused_before_freeze() {
    let wrong = [
        manifest_for("other_job", OWNER, &hash(), "x", true),
        manifest_for(JOB, "other_owner", &hash(), "x", true),
        manifest_for(JOB, OWNER, &"b".repeat(64), "x", true),
    ];
    for candidate in wrong {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Ok(None), Ok(manifest("Never")));
        let got = run(&store, &calls, Ok(candidate)).await;
        assert_eq!(got, Err(AcceptanceError::Identity));
        assert_eq!(*calls.borrow(), ["load", "produce"]);
    }
}

#[tokio::test]
async fn freeze_error_is_returned_without_candidate_fallback() {
    for error in [
        FreezeError::MissingOrFenced,
        FreezeError::Permanent,
        FreezeError::Unavailable,
    ] {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Ok(None), Err(error));
        let got = run(&store, &calls, Ok(manifest("Candidate"))).await;
        assert_eq!(got, Err(AcceptanceError::Store(error)));
        assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
    }
}

#[tokio::test]
async fn winners_with_wrong_identity_from_load_or_freeze_are_invalid() {
    let wrong = [
        manifest_for("other_job", OWNER, &hash(), "x", true),
        manifest_for(JOB, "other_owner", &hash(), "x", true),
        manifest_for(JOB, OWNER, &"b".repeat(64), "x", true),
    ];
    for bad in wrong {
        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Ok(Some(bad.clone())), Ok(manifest("Never")));
        let got = run(&store, &calls, Ok(manifest("Candidate"))).await;
        assert_eq!(got, Err(AcceptanceError::InvalidWinner));
        assert_eq!(*calls.borrow(), ["load"]);

        let calls = RefCell::new(Vec::new());
        let store = FakeStore::new(&calls, Ok(None), Ok(bad));
        let got = run(&store, &calls, Ok(manifest("Candidate"))).await;
        assert_eq!(got, Err(AcceptanceError::InvalidWinner));
        assert_eq!(*calls.borrow(), ["load", "produce", "freeze"]);
    }
}

#[tokio::test]
async fn invalid_root_identity_touches_neither_store_nor_producer() {
    let long_user = "u".repeat(129);
    let bad_hash = "A".repeat(64);
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
        let store = FakeStore::new(&calls, Ok(None), Ok(manifest("Never")));
        let got = accept_manifest(&store, job, user, digest, || async {
            calls.borrow_mut().push("produce");
            Ok(manifest("Candidate"))
        })
        .await;
        assert_eq!(
            got,
            Err(AcceptanceError::Identity),
            "{job:?} {user:?} {digest:?}"
        );
        assert!(calls.borrow().is_empty());
    }
}

#[tokio::test]
async fn legacy_winner_without_effective_discs_passes_through_unchanged() {
    let legacy = manifest_for(JOB, OWNER, &hash(), "Legacy", false);
    assert_eq!(legacy.effective_discs(), None);

    let calls = RefCell::new(Vec::new());
    let store = FakeStore::new(&calls, Ok(Some(legacy.clone())), Ok(manifest("Never")));
    let got = run(&store, &calls, Ok(manifest("Candidate")))
        .await
        .unwrap();
    assert_eq!(got, legacy);
    assert_eq!(got.effective_discs(), None);
    assert_eq!(*calls.borrow(), ["load"]);

    let calls = RefCell::new(Vec::new());
    let store = FakeStore::new(&calls, Ok(None), Ok(legacy.clone()));
    let got = run(&store, &calls, Ok(manifest("Candidate")))
        .await
        .unwrap();
    assert_eq!(got, legacy);
    assert_eq!(got.effective_discs(), None);
}
