//! Exercise the runner's version confirmation with synthetic committed pins.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasm_bindgen::prelude::*;

use super::{
    cleanup_with_s3, s3_confirmation, Fixture, FixtureVersionRemover, RetiredFixture, S3Binding,
    S3Locator, TrialError,
};

#[path = "../../../ingest/tests/fixtures/catalogue_postgres_wasm.rs"]
mod postgres;

fn remover() -> FixtureVersionRemover {
    FixtureVersionRemover::new(
        [7; 16],
        "https://fixture.test:8443",
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .unwrap()
}

fn deadline() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(worker::Date::now().as_millis()) + Duration::from_secs(5)
}

fn error_name(error: TrialError) -> JsValue {
    JsValue::from_str(&format!("{error:?}"))
}

#[wasm_bindgen]
pub async fn private_trial_cleanup_test(
    bucket: String,
    media_key: String,
    media_version: String,
    page_key: String,
    page_version: String,
) -> Result<(), JsValue> {
    let remover = remover();
    let pins = RetiredFixture {
        media: S3Locator::new(bucket, media_key, S3Binding::VersionId(media_version)).unwrap(),
        page_key,
        page_version,
    };
    pins.confirm_versions(s3_confirmation(&remover, deadline()))
        .await
        .map_err(error_name)
}

#[wasm_bindgen]
pub async fn private_trial_cleanup_foreign_bucket_test() -> Result<bool, JsValue> {
    let mut db = postgres::connect().await;
    // A retirement query must fail on this connection even if the SQL schema
    // has already been loaded. The real entry point must return Refused first.
    db.batch_execute("SET default_transaction_read_only = on")
        .await
        .unwrap();
    let fixture = Fixture {
        environment: [1; 16],
        library: [2; 16],
        object: [3; 16],
        track: "00000000-0000-4000-8000-000000000004",
        session: "00000000-0000-4000-8000-000000000005",
        job: "synthetic-trial",
        issuer: "synthetic-issuer",
        invite: 1,
        bucket: "fixture-bucket",
        cutoff: 0,
    };
    let remover = remover();
    let supported_bucket = Fixture {
        bucket: "nntunev2-private-fixtures-staging",
        ..fixture
    };
    assert_eq!(
        cleanup_with_s3(&mut db, &supported_bucket, &remover, deadline()).await,
        Err(TrialError::Unconfirmed),
        "the read-only fixture must reject the SQL retirement query"
    );
    cleanup_with_s3(&mut db, &fixture, &remover, deadline())
        .await
        .map_err(error_name)
}
