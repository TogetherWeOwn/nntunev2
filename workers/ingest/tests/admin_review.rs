use std::cell::{Cell, RefCell};

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
    let id = 0x00000000_0000_4000_8000_000000000000_u128.to_be_bytes();
    let key = format!("media/{0}/{0}", hex::encode(id));
    let binding = S3Binding::VersionId(version.into());
    let locator = S3Locator::new("fixture-bucket".into(), key, binding).unwrap();
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

fn job(status: ReviewStatus) -> ReviewJob {
    let published = status == ReviewStatus::Published;
    ReviewJob {
        view: JobView {
            job_id: "upload".into(),
            revision: if published { 2 } else { 1 },
            status,
            reason: (status == ReviewStatus::NeedsReview).then_some(NeedsReview::PositionMismatch),
        },
        owner_id: "owner".into(),
        object: Some(object("v1", 1)),
        retried_revision: published.then_some(1),
    }
}

#[tokio::test]
async fn reads_are_scoped_and_redacted() {
    let pending = ReviewStatus::NeedsReview;
    let get = async |_: &str, _: &str| Ok(Some(job(pending)));
    let detail = async |owner, scope, id| detail(&auth(&grant(owner, scope)), &get, id).await;
    let view = detail("owner", READ_SCOPE, "upload").await.unwrap();
    let public = json!({"job_id":"upload","revision":1,"status":"needs_review","reason":"position_mismatch"});
    assert_eq!(to_value(view).unwrap(), public);
    let list = async |owner: &str, status, limit, offset| {
        assert_eq!((owner, status), ("owner", Some(pending)));
        assert!((1..=100).contains(&limit));
        let mut row = job(pending);
        match offset {
            1 => row.owner_id = "other".into(),
            2 => row.view.job_id = "/".into(),
            _ => {}
        }
        if limit == 1 {
            return Ok(vec![row, job(pending)]);
        }
        Ok(vec![row])
    };
    let read_queue = async |limit, offset| {
        let input = json!({"limit":limit,"offset":offset,"status":"needs_review"});
        let request = serde_json::from_value(input).unwrap();
        queue(&auth(&grant("owner", READ_SCOPE)), &list, request).await
    };
    let expected = json!({"jobs":[public],"limit":100,"offset":0});
    let view = read_queue(200, 0).await.unwrap();
    assert_eq!(to_value(view).unwrap(), expected);
    for (limit, offset) in [(1, 0), (50, 1), (50, 2)] {
        assert_eq!(read_queue(limit, offset).await.unwrap_err().status, 503);
    }
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
    use ReviewStatus::{Accepted, NeedsReview, Published};
    let current = Cell::new(Accepted);
    let reads = Cell::new(0);
    let stored = Cell::new(("v1", 1));
    let returned_row = RefCell::new(None);
    // Return the owned row even for a foreign query to test the operation's fence.
    let get = async |_: &str, _: &str| Ok(Some(job(current.get())));
    let verify = async |owner: &str, _: &ObjectRef| {
        assert_eq!(owner, "owner");
        reads.set(reads.get() + 1);
        let (version, digest) = stored.get();
        Ok(object(version, digest))
    };
    let publish = async |expected: &ReviewJob| {
        assert_eq!(expected.view.revision, 1);
        if let Some(row) = returned_row.take() {
            return Ok(row);
        }
        assert_eq!(current.replace(Published), NeedsReview);
        Err(BoundaryError::Unavailable) // Lost response after commit.
    };
    let run = async |owner, scope, body| {
        let bearer = grant(owner, scope);
        retry(&auth(&bearer), &get, &verify, &publish, "upload", body).await
    };
    let authorized = async || run("owner", RETRY_SCOPE, RETRY).await;
    assert_eq!(authorized().await.unwrap_err().status, 409);
    current.set(NeedsReview);
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
    assert_eq!(reads.get(), 0);
    for mismatch in [("v2", 1), ("v1", 2)] {
        stored.set(mismatch);
        assert_eq!(authorized().await.unwrap_err().status, 409);
        assert_eq!(current.get(), NeedsReview);
    }
    stored.set(("v1", 1));
    for (owner, version, status) in [("other", "v1", 404), ("owner", "v2", 503)] {
        let mut row = job(Published);
        row.owner_id = owner.into();
        row.object = Some(object(version, 1));
        returned_row.replace(Some(row));
        assert_eq!(authorized().await.unwrap_err().status, status);
    }
    assert_eq!(authorized().await.unwrap_err().status, 503);
    let published = json!({"job_id":"upload","revision":2,"status":"published"});
    assert_eq!(to_value(authorized().await.unwrap()).unwrap(), published);
    assert_eq!(reads.get(), 5);
}
