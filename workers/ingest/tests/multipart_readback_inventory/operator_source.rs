use super::{id, Client, ReadbackFixture};
use nntune_core::s3_locator::{S3Binding, S3Locator, S3MediaMetadata};
use nntune_ingest::{
    admin_review::{
        source::{select, PublishedSource, Selection},
        Authorization, JobView, ObjectRef, ReviewJob, ReviewStatus, READ_SCOPE, RETRY_SCOPE,
    },
    api_error::ApiError,
    caller_auth::issue,
};

const OWNER: &str = "catalogue-owner";
const JOB: &str = "catalogue-old";
const KEY: &str = "synthetic-operator-source-fixture";

const OWNED_READ: (&str, &str, ReviewStatus, Option<&str>) =
    (OWNER, READ_SCOPE, ReviewStatus::Published, None);

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
    (actor, scope, status, version): (&str, &str, ReviewStatus, Option<&str>),
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
        async |_, requested_actor, requested| {
            // Deliberately return the review row for foreign requests too.
            // The source entry point must enforce owner, job and publication.
            assert!(*requested == target);
            assert_eq!(requested_actor, actor);
            Ok(Some(ReviewJob {
                view: JobView {
                    job_id: target.job_id.into(),
                    revision: 2,
                    status,
                    reason: None,
                },
                owner_id: OWNER.into(),
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
    use ReviewStatus::{Accepted, Failed, NeedsReview, Published};
    let target = selection(fixture, track_id);
    for client in [writer, &mut *refresh] {
        let selected = read(client, fixture, target, OWNED_READ)
            .await
            .unwrap()
            .expect("the committed Operator selection must survive refresh");
        assert!(selected.selection == target);
        assert!(selected.locator == *fixture.media.metadata().locator());
    }
    for status in [Accepted, Failed, NeedsReview] {
        assert!(
            read(refresh, fixture, target, (OWNER, READ_SCOPE, status, None))
                .await
                .unwrap()
                .is_none()
        );
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
        (OWNER, READ_SCOPE, Published, Some("different-version")),
    )
    .await
    .unwrap()
    .is_none());
    for (actor, scope) in [("catalogue-other", READ_SCOPE), (OWNER, RETRY_SCOPE)] {
        assert_eq!(
            read(refresh, fixture, target, (actor, scope, Published, None))
                .await
                .err()
                .unwrap()
                .status,
            404
        );
    }
}

pub(super) async fn refuse_replaced(
    client: &mut Client,
    fixture: &ReadbackFixture,
    track_id: &str,
) {
    assert!(
        read(client, fixture, selection(fixture, track_id), OWNED_READ)
            .await
            .unwrap()
            .is_none()
    );
}
