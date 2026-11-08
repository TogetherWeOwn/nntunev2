//! Fresh admission through canonical SQL stages and publication, with fake mirror,
//! STAT, sender and decoded geometry. No seeded checkpoint or fake stage store.
//! Not live routing, broker crash recovery or ordered-schema acceptance.
use super::*;
use nntune_ingest::{
    admin_checkpoint_store::{ControlActor, Delivery, DeliveryStageOutcome, StageServices},
    admin_execution::{Control, StageOutcome},
};
use std::collections::VecDeque;

#[derive(Default)]
struct Queue {
    pending: VecDeque<String>,
    sends: usize,
}

impl checkpoints::DeliverySender for Queue {
    type Error = ();

    async fn send(&mut self, encoded: String, delay_secs: u32) -> Result<(), Self::Error> {
        assert_eq!(delay_secs, 0);
        self.sends += 1;
        self.pending.push_back(encoded);
        Ok(())
    }
}

#[derive(Default)]
struct Services {
    events: Vec<&'static str>,
    no_match: bool,
    missing: bool,
}

impl StageServices for Services {
    async fn match_release(
        &mut self,
        _: &Document,
    ) -> Result<Option<String>, checkpoints::ServiceError> {
        self.events.push("match");
        Ok((!self.no_match).then(|| RELEASE.into()))
    }

    async fn probe(
        &mut self,
        req: &ProbeRequest,
    ) -> Result<ProbeOutcome, checkpoints::ServiceError> {
        self.events.push("probe");
        Ok(ProbeOutcome {
            batch_index: req.batch_index,
            present: if self.missing {
                vec![]
            } else {
                req.segment_ids.clone()
            },
            missing: if self.missing {
                req.segment_ids.clone()
            } else {
                vec![]
            },
        })
    }
}

fn input(original: &OriginalBody) -> StageOriginal<'_> {
    StageOriginal {
        job_id: original.job_id(),
        idempotency_key: original.idempotency_key(),
        nzb_bytes: original.as_bytes().len(),
        parsed: original.parsed(),
    }
}

async fn admitted() -> (tokio_postgres::Client, OriginalBody, Queue) {
    let mut app = database().await;
    publication_tables(&app).await;
    let attempt = attempt();
    assert!(matches!(
        upload(&mut app, &attempt, true).await.unwrap(),
        AdminIngestUploadOutcome::Stored {
            deduplicated: false,
            ..
        }
    ));
    let original = read_original(
        &Authority,
        &mut PostgresOriginalRepository { client: &app },
        &Keys,
        &attempt.job_id,
    )
    .await
    .unwrap();
    let initial = checkpoints::load(&app, &input(&original), 1).await.unwrap();
    assert_eq!(
        (initial.revision(), initial.generation(), initial.status()),
        (1, 1, "queued")
    );
    let mut queue = Queue::default();
    assert_eq!(
        checkpoints::enqueue_delivery(&app, &input(&original), initial.generation(), &mut queue)
            .await,
        Ok(true)
    );
    (app, original, queue)
}

async fn drain(
    app: &mut tokio_postgres::Client,
    original: &OriginalBody,
    services: &mut Services,
    queue: &mut Queue,
) -> Snapshot {
    // Four stages for this fixture. An accidental continuation loop fails rather
    // than hanging, and every received body is decoded and reloaded from SQL.
    for _ in 0..8 {
        let Some(encoded) = queue.pending.pop_front() else {
            return checkpoints::load(app, &input(original), 1).await.unwrap();
        };
        let delivery = Delivery::decode(&encoded).unwrap();
        assert_eq!(delivery.generation(), 1);
        let completion = checkpoints::run_delivery_and_enqueue(
            app,
            &input(original),
            &delivery,
            services,
            queue,
        )
        .await
        .unwrap();
        assert!(completion.continuation.is_ok());
        assert!(matches!(
            completion.outcome,
            DeliveryStageOutcome::Advanced(_)
        ));
        let saved = checkpoints::load(app, &input(original), 1).await.unwrap();
        assert_ne!(saved.checkpoint().state, JobState::Searching);
        assert!(saved.checkpoint().user_id.is_empty());
        assert_eq!(super::super::counts(app).await, (0, 0, 0));
    }
    panic!("continuation failed to stop within the fixture stage bound")
}

async fn publish(
    app: &mut tokio_postgres::Client,
    original: &OriginalBody,
    writing: &Snapshot,
    sealer: &mut Sealer,
) -> Result<Snapshot, checkpoints::PublicationError> {
    // Same fake mirror/inspection contract as the existing writer fixture. These
    // values do not establish trusted inspection of a real decoded provider file.
    checkpoints::publish(
        app,
        &input(original),
        writing,
        &[MbRecording {
            mbid: RECORDING.into(),
            title: "Catalogue title".into(),
            track_no: Some(1),
            medium_no: Some(1),
        }],
        &[FileGeometry {
            file_ordinal: 0,
            decoded_bytes: 64,
            range_hint: None,
        }],
        sealer,
    )
    .await
}

#[tokio::test]
async fn fresh_upload_delivery_pipeline_commits_album_and_done_without_seeded_progress() {
    let (mut app, original, mut queue) = admitted().await;
    let mut services = Services::default();
    let writing = drain(&mut app, &original, &mut services, &mut queue).await;
    assert_eq!(writing.checkpoint().state, JobState::Writing);
    assert_eq!((writing.revision(), writing.generation()), (5, 1));
    assert_eq!(services.events, ["match", "probe"]);
    assert_eq!(queue.sends, 4);
    let mut sealer = Sealer::default();
    let done = publish(&mut app, &original, &writing, &mut sealer)
        .await
        .unwrap();
    assert_eq!(
        (done.checkpoint().state, done.status(), done.revision()),
        (JobState::Done, "done", 6)
    );
    assert_eq!(sealer.calls, 1);
    assert_eq!(super::super::counts(&app).await, (1, 1, 1));
    let row = app.query_one(
        "SELECT l.user_id, l.job_id, l.idempotency_key, s.source_job_id, s.nzb_ref_envelope_version, s.nzb_ref_kek_version FROM library l JOIN track_sources s ON s.source_job_id = l.job_id",
        &[],
    ).await.unwrap();
    assert_eq!(row.get::<_, Option<String>>(0), None);
    assert_eq!(row.get::<_, String>(1), original.job_id());
    assert_eq!(row.get::<_, String>(2), original.idempotency_key());
    assert_eq!(row.get::<_, String>(3), original.job_id());
    assert_eq!(row.get::<_, i16>(4), 2);
    assert_eq!(row.get::<_, i32>(5), 7);
    let before = authority(&app).await;
    assert!(publish(&mut app, &original, &writing, &mut sealer)
        .await
        .is_err());
    assert_eq!(sealer.calls, 1);
    assert_eq!(
        checkpoints::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Ok(false)
    );
    assert!(matches!(
        upload(&mut app, &attempt(), true).await.unwrap(),
        AdminIngestUploadOutcome::Stored {
            deduplicated: true,
            ..
        }
    ));
    assert_eq!(authority(&app).await, before);
    assert_eq!(super::super::counts(&app).await, (1, 1, 1));
    assert_eq!(counts(&app).await, (1, 1, 2));
}

#[tokio::test]
async fn fresh_upload_without_match_parks_then_manual_approval_requires_probe_before_publication() {
    let (mut app, original, mut queue) = admitted().await;
    let mut services = Services {
        no_match: true,
        ..Default::default()
    };
    let review = drain(&mut app, &original, &mut services, &mut queue).await;
    assert_eq!(review.checkpoint().state, JobState::NeedsReview);
    assert_eq!(services.events, ["match"]);
    let mut sealer = Sealer::default();
    assert!(publish(&mut app, &original, &review, &mut sealer)
        .await
        .is_err());
    assert_eq!(sealer.calls, 0);
    let approved = checkpoints::apply_control_and_enqueue(
        &mut app,
        ControlActor {
            user_id: "stage_user",
            source_ip: None,
            now: 2,
        },
        &input(&original),
        &review,
        Control::ApproveReview { release: RELEASE },
        &mut queue,
    )
    .await
    .unwrap();
    assert_eq!(approved.continuation, Ok(true));
    assert_eq!(approved.snapshot.checkpoint().state, JobState::Probing);
    assert!(approved.snapshot.generation() > 1);
    assert!(
        publish(&mut app, &original, &approved.snapshot, &mut sealer)
            .await
            .is_err()
    );
    let delivery = Delivery::decode(&queue.pending.pop_front().unwrap()).unwrap();
    assert_eq!(delivery.generation(), approved.snapshot.generation());
    let result = checkpoints::run_delivery_and_enqueue(
        &mut app,
        &input(&original),
        &delivery,
        &mut services,
        &mut queue,
    )
    .await
    .unwrap();
    assert_eq!(
        result.outcome,
        DeliveryStageOutcome::Advanced(StageOutcome::ReadyToPublish)
    );
    assert_eq!(result.continuation, Ok(false));
    assert_eq!(services.events, ["match", "probe"]);
    let writing = checkpoints::load(&app, &input(&original), delivery.generation())
        .await
        .unwrap();
    publish(&mut app, &original, &writing, &mut sealer)
        .await
        .unwrap();
    assert_eq!(sealer.calls, 1);
    assert_eq!(super::super::counts(&app).await, (1, 1, 1));
    assert_eq!(counts(&app).await, (1, 1, 2));
    let operation: String = app
        .query_one(
            "SELECT details->>'operation' FROM audit_log WHERE action = 'ingest.controlled'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(operation, "approve");
}

#[tokio::test]
async fn fresh_upload_missing_probe_segments_never_seals_or_publishes() {
    let (mut app, original, mut queue) = admitted().await;
    let mut services = Services {
        missing: true,
        ..Default::default()
    };
    let review = drain(&mut app, &original, &mut services, &mut queue).await;
    assert_eq!(review.checkpoint().state, JobState::NeedsReview);
    assert_eq!(services.events, ["match", "probe"]);
    assert!(queue.pending.is_empty());
    let before = authority(&app).await;
    let mut sealer = Sealer::default();
    assert!(publish(&mut app, &original, &review, &mut sealer)
        .await
        .is_err());
    assert_eq!(sealer.calls, 0);
    assert_eq!(
        checkpoints::enqueue_delivery(&app, &input(&original), 1, &mut queue).await,
        Ok(false)
    );
    assert_eq!(authority(&app).await, before);
    assert_eq!(super::super::counts(&app).await, (0, 0, 0));
    assert_eq!(counts(&app).await, (1, 1, 1));
}
