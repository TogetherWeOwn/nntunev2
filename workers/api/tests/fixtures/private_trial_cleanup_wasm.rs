//! Exercise the runner's version confirmation with synthetic committed pins.

use std::time::{Duration, UNIX_EPOCH};

use wasm_bindgen::prelude::*;

use super::{s3_confirmation, FixtureVersionRemover, RetiredFixture, S3Binding, S3Locator};

#[wasm_bindgen]
pub async fn private_trial_cleanup_test(
    bucket: String,
    media_key: String,
    media_version: String,
    page_key: String,
    page_version: String,
) -> Result<(), JsValue> {
    let remover = FixtureVersionRemover::new(
        [7; 16],
        "https://fixture.test:8443",
        "fixture-region".into(),
        "FIXTUREACCESS".into(),
        "fixture-secret".into(),
    )
    .unwrap();
    let pins = RetiredFixture {
        media: S3Locator::new(bucket, media_key, S3Binding::VersionId(media_version)).unwrap(),
        page_key,
        page_version,
    };
    let until = UNIX_EPOCH
        + Duration::from_millis(worker::Date::now().as_millis())
        + Duration::from_secs(5);
    pins.confirm_versions(s3_confirmation(&remover, until))
        .await
        .map_err(|error| JsValue::from_str(&format!("{error:?}")))
}
