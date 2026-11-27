//! Protected transport for a selected staging fixture.

pub const PATH: &str = "/v1/admin/private-trial/upload";
pub const FIXTURE_BYTES: usize = 177_175;
#[cfg(target_arch = "wasm32")]
const FIXTURE_BUCKET: &str = "nntunev2-private-fixtures-staging";

mod caller;
#[cfg(target_arch = "wasm32")]
pub(crate) use caller::worker;
pub mod cleanup;
#[cfg(all(target_arch = "wasm32", feature = "staging-fixture-cleanup"))]
mod cleanup_runner;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod registered;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod registration;
#[cfg(any(target_arch = "wasm32", all(test, feature = "postgres-tests")))]
pub mod sealed_fixture;
pub use caller::*;

#[cfg(test)]
#[path = "../../tests/fixtures/private_trial_fixture_boundaries.rs"]
mod fixture_boundaries;

#[cfg(any(target_arch = "wasm32", test))]
fn parse_object<T: serde::de::DeserializeOwned>(raw: &str, limit: usize) -> Result<T, TrialError> {
    if raw.len() > limit || !raw.trim_ascii_start().starts_with('{') {
        return Err(TrialError::Refused);
    }
    serde_json::from_str(raw).map_err(|_| TrialError::Refused)
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MirrorEvidence {
    release_mbid: String,
    title: String,
    exact_title_candidates: Vec<nntune_ingest::matching::MbSearchHit>,
    recordings: Vec<MirrorRecording>,
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
enum MirrorRecording {
    Row {
        release_mbid: String,
        mbid: String,
        title: String,
        track_no: Option<i16>,
        medium_no: Option<i16>,
    },
}

#[cfg(any(target_arch = "wasm32", test))]
fn validate_mirror_evidence(
    title: &str,
    selected_mbid: &str,
    candidates: nntune_ingest::matching::admin_publish_match::CompleteReleaseCandidates<'_>,
    recordings: &str,
    numbering: nntune_ingest::matching::admin_publish_match::FileNumbering<'_>,
) -> Result<(), TrialError> {
    use nntune_ingest::matching::admin_publish_match::{decide_publish_match, select_release};
    let release = select_release(title, Some(candidates)).map_err(|_| TrialError::Refused)?;
    if release.mbid() != selected_mbid {
        return Err(TrialError::Refused);
    }
    decide_publish_match(release, 1, &[numbering], 200, recordings)
        .map(|_| ())
        .map_err(|_| TrialError::Refused)
}
