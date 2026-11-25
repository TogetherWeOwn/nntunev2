use std::cell::RefCell;

use super::{id, Client, ReadbackFixture, Transaction};
use nntune_core::s3_locator::{S3Binding, S3Locator, S3MediaMetadata};
use nntune_ingest::{
    admin_review::{
        source::{select, PublishedSource, Selection},
        Authorization, ObjectRef, ReviewJob, ReviewStatus, READ_SCOPE, RETRY_SCOPE,
    },
    api_error::ApiError,
    caller_auth::issue,
};

const OWNER: &str = "catalogue-owner";
const JOB: &str = "catalogue-old";
const KEY: &str = "synthetic-operator-source-fixture";

const OWNED_READ: (&str, &str, Option<&str>) = (OWNER, READ_SCOPE, None);

fn selection<'a>(fixture: &ReadbackFixture, track_id: &'a str) -> Selection<'a> {
    let [_, environment, library, ..] = fixture.context.identities;
    Selection {
        job_id: JOB,
        revision: 2,
        track_id,
        environment,
        library,
    }
}

async fn read<'a>(
    client: &mut Client,
    fixture: &ReadbackFixture,
    target: Selection<'a>,
    credentials: (&str, &str, Option<&str>),
) -> Result<Option<PublishedSource<'a>>, ApiError> {
    read_then(client, fixture, target, credentials, async || {}).await
}

async fn read_then<'a>(
    client: &mut Client,
    fixture: &ReadbackFixture,
    target: Selection<'a>,
    (actor, scope, version): (&str, &str, Option<&str>),
    after_read: impl AsyncFn(),
) -> Result<Option<PublishedSource<'a>>, ApiError> {
    let bearer = format!("Bearer {}", issue(actor, scope, 10, KEY.as_bytes()));
    let existing = fixture.media.metadata();
    let media = match version {
        None => existing.clone(),
        Some(version) => {
            let locator = S3Locator::new(
                existing.locator().bucket().into(),
                existing.locator().key().into(),
                S3Binding::VersionId(version.into()),
            )
            .unwrap();
            S3MediaMetadata::from_trusted_context(locator, &fixture.context).unwrap()
        }
    };
    select(
        &Authorization {
            bearer: Some(&bearer),
            secret: Some(KEY),
            now: 1,
        },
        client,
        target,
        async |tx, requested_actor, requested| {
            assert!(*requested == target);
            assert_eq!(requested_actor, actor);
            // Deliberately return the persisted row for foreign owners too.
            let Some(row) = tx
                .query_opt(
                    "SELECT r.view::text, j.user_id FROM public.test_operator_review r
                 JOIN public.ingest_jobs j ON j.id = r.job_id WHERE r.job_id = $1",
                    &[&requested.job_id],
                )
                .await
                .unwrap()
            else {
                return Ok(None);
            };
            after_read().await;
            Ok(Some(ReviewJob {
                view: serde_json::from_str(row.get(0)).unwrap(),
                owner_id: row.get(1),
                object: Some(ObjectRef {
                    media: media.clone(),
                    plaintext_sha256: *fixture.origin.plaintext_sha256(),
                }),
                retried_revision: Some(1),
            }))
        },
    )
    .await
}

pub(super) async fn exercise(
    writer: &mut Client,
    refresh: &mut Client,
    fixture: &ReadbackFixture,
    track_id: &str,
) {
    let target = selection(fixture, track_id);
    for client in [&mut *writer, &mut *refresh] {
        let selected = read(client, fixture, target, OWNED_READ)
            .await
            .unwrap()
            .expect("the committed Operator selection must survive refresh");
        assert!(selected.selection == target);
        assert!(selected.locator == *fixture.media.metadata().locator());
    }
    use ReviewStatus::{Accepted, Failed, NeedsReview, Published};
    for status in [Accepted, Failed, NeedsReview, Published] {
        writer
            .execute(
                "UPDATE public.test_operator_review
             SET view = jsonb_set(view, '{status}', $2::text::jsonb) WHERE job_id = $1",
                &[&JOB, &serde_json::to_string(&status).unwrap()],
            )
            .await
            .unwrap();
        if status != Published {
            assert!(read(refresh, fixture, target, OWNED_READ)
                .await
                .unwrap()
                .is_none());
        }
    }
    for wrong_target in [
        Selection {
            job_id: "catalogue-queued",
            ..target
        },
        Selection {
            job_id: "catalogue-new",
            ..target
        },
        Selection {
            revision: 1,
            ..target
        },
        Selection {
            track_id: super::RELEASE,
            ..target
        },
        Selection {
            environment: id(0x7f),
            ..target
        },
        Selection {
            library: id(0x81),
            ..target
        },
    ] {
        assert!(read(refresh, fixture, wrong_target, OWNED_READ)
            .await
            .unwrap()
            .is_none());
    }
    assert!(read(
        refresh,
        fixture,
        target,
        (OWNER, READ_SCOPE, Some("different-version")),
    )
    .await
    .unwrap()
    .is_none());
    for (actor, scope) in [("catalogue-other", READ_SCOPE), (OWNER, RETRY_SCOPE)] {
        assert_eq!(
            read(refresh, fixture, target, (actor, scope, None))
                .await
                .err()
                .unwrap()
                .status,
            404
        );
    }
}

pub(super) async fn refresh_during_replacement(
    refresh: &mut Client,
    replacement: Transaction<'_>,
    fixture: &ReadbackFixture,
    track_id: &str,
) {
    let target = selection(fixture, track_id);
    let replacement = RefCell::new(Some(replacement));
    let inflight = read_then(refresh, fixture, target, OWNED_READ, async || {
        // Commit on the writer after the review read and before the source read.
        let replacement = replacement.borrow_mut().take().unwrap();
        replacement.commit().await.unwrap();
    })
    .await
    .unwrap()
    .expect("the refresh must see the source from the review snapshot");
    assert!(inflight.selection == target);
    assert!(inflight.locator == *fixture.media.metadata().locator());
    assert!(read(refresh, fixture, target, OWNED_READ)
        .await
        .unwrap()
        .is_none());
}
