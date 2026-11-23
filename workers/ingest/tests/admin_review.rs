use std::cell::Cell;

use nntune_core::{
    media_envelope::EnvelopeContext,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};
use nntune_ingest::{
    admin_review::*,
    caller_auth::{issue, INGEST_WRITE_SCOPE},
    matching::admin_publish_match::NeedsReview,
};
use serde_json::{json, to_value};

const KEY: &str = "synthetic-review-only-fixture-key";
const RETRY: &[u8] = br#"{"revision":1}"#;

fn grant(owner: &str, scope: &str) -> String {
    format!("Bearer {}", issue(owner, scope, 10, KEY.as_bytes()))
}

fn auth(bearer: &str) -> Authorization<'_> {
    Authorization {
        bearer: Some(bearer),
        secret: Some(KEY),
        now: 1,
    }
}

fn object(version: &str, digest: u8) -> ObjectRef {
    let mut id = [0; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    let key = format!("media/{0}/{0}", hex::encode(id));
    let locator = S3Locator::new(
        "fixture-bucket".into(),
        key,
        S3Binding::VersionId(version.into()),
    )
    .unwrap();
    let context = EnvelopeContext {
        identities: [id; 6],
        plaintext_len: 20,
        header: [0; 48],
    };
    ObjectRef {
        media: S3MediaMetadata::from_trusted_context(locator, &context).unwrap(),
        plaintext_sha256: [digest; 32],
    }
}

#[derive(Default)]
struct Fixture {
    committed: Cell<bool>,
    reads: Cell<u32>,
    stored: Cell<Option<(&'static str, u8)>>,
}

impl Fixture {
    fn job(&self) -> ReviewJob {
        let committed = self.committed.get();
        let status = if committed {
            ReviewStatus::Published
        } else {
            ReviewStatus::NeedsReview
        };
        ReviewJob {
            view: JobView {
                job_id: "upload".into(),
                revision: if committed { 2 } else { 1 },
                status,
                reason: (!committed).then_some(NeedsReview::PositionMismatch),
            },
            owner_id: "owner".into(),
            object: Some(object("v1", 1)),
            retried_revision: committed.then_some(1),
        }
    }
}

#[tokio::test]
async fn reads_are_scoped_and_redacted() {
    let fixture = Fixture::default();
    let get = async |_: &str, _: &str| Ok(Some(fixture.job()));
    let detail = async |owner, scope, id| detail(&auth(&grant(owner, scope)), &get, id).await;
    let view = detail("owner", READ_SCOPE, "upload").await.unwrap();
    let public = json!({"job_id":"upload","revision":1,"status":"needs_review","reason":"position_mismatch"});
    assert_eq!(to_value(view).unwrap(), public);
    let request = serde_json::from_value(json!({"limit":200,"status":"needs_review"})).unwrap();
    let list = async |owner: &str, status, limit, offset| {
        assert_eq!(
            (owner, status, limit, offset),
            ("owner", Some(ReviewStatus::NeedsReview), 100, 0)
        );
        Ok(vec![fixture.job()])
    };
    let view = queue(&auth(&grant("owner", READ_SCOPE)), list, request)
        .await
        .unwrap();
    assert_eq!(
        to_value(view).unwrap(),
        json!({"jobs":[public],"limit":100,"offset":0})
    );
    for (owner, scope, id) in [
        ("other", READ_SCOPE, "upload"),
        ("owner", INGEST_WRITE_SCOPE, "upload"),
        ("owner", READ_SCOPE, "missing"),
    ] {
        assert_eq!(detail(owner, scope, id).await.unwrap_err().status, 404);
    }
}

#[tokio::test]
async fn retry_fences_side_effects_and_recovers_a_lost_response() {
    let fixture = Fixture::default();
    // Return the owned row even for a foreign query to test the operation's fence.
    let get = async |_: &str, _: &str| Ok(Some(fixture.job()));
    let verify = async |owner: &str, _: &ObjectRef| {
        assert_eq!(owner, "owner");
        fixture.reads.set(fixture.reads.get() + 1);
        let (version, digest) = fixture.stored.get().unwrap_or(("v1", 1));
        Ok(object(version, digest))
    };
    let publish = async |expected: &ReviewJob| {
        assert_eq!(expected.view.revision, 1);
        assert!(!fixture.committed.replace(true), "retry committed twice");
        Err(BoundaryError::Unavailable) // Lost response after commit.
    };
    let run = async |owner, scope, body| {
        let bearer = grant(owner, scope);
        retry(&auth(&bearer), &get, &verify, &publish, "upload", body).await
    };
    for (owner, scope, body, status) in [
        ("owner", READ_SCOPE, RETRY, 404),
        ("other", RETRY_SCOPE, RETRY, 404),
        ("owner", RETRY_SCOPE, br#"{"revision":0}"#, 409),
        ("owner", RETRY_SCOPE, b"[1]", 400),
        ("owner", RETRY_SCOPE, br#"{"revision":1,"revision":0}"#, 400),
        ("owner", RETRY_SCOPE, &[b' '; RETRY_BODY_LIMIT + 1], 413),
    ] {
        assert_eq!(run(owner, scope, body).await.unwrap_err().status, status);
    }
    assert_eq!(fixture.reads.get(), 0);
    let authorized = async || run("owner", RETRY_SCOPE, RETRY).await;
    for mismatch in [("v2", 1), ("v1", 2)] {
        fixture.stored.set(Some(mismatch));
        assert_eq!(authorized().await.unwrap_err().status, 409);
        assert!(!fixture.committed.get());
    }
    fixture.stored.set(None);
    assert_eq!(authorized().await.unwrap_err().status, 503);
    let view = authorized().await.unwrap();
    assert_eq!(
        to_value(view).unwrap(),
        json!({"job_id":"upload","revision":2,"status":"published"})
    );
    assert_eq!(fixture.reads.get(), 3);
}
