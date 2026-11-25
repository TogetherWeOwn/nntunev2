//! Standalone provider check. The reserved object ID is only for disposable versions.

use std::time::Duration;

use serde::Deserialize;
use worker::wasm_bindgen::{self, prelude::*};

use super::{check_version, FixtureRetirementError, FixtureVersionRemover, BUCKET};

const DISPOSABLE_OBJECT: &str = "00000000000040008000000000000000";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    bucket: String,
    endpoint: String,
    region: String,
    generation: String,
    version: String,
    sibling_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    access_key: String,
    secret_key: String,
}

#[wasm_bindgen]
pub async fn qualify_disposable_version(
    endpoint: String,
    region: String,
    scope: String,
    credential: String,
) -> String {
    use FixtureRetirementError::{Refused, Unconfirmed};
    let result = async {
        if endpoint.len() > 253
            || region.len() > 63
            || scope.len() > 4096
            || credential.len() > 1024
        {
            return Err(Refused);
        }
        let scope: Scope = serde_json::from_str(&scope).map_err(|_| Refused)?;
        let credential: Credential = serde_json::from_str(&credential).map_err(|_| Refused)?;
        if scope.bucket != BUCKET || scope.endpoint != endpoint || scope.region != region {
            return Err(Refused);
        }
        let remover = FixtureVersionRemover::new(
            [0; 16],
            &endpoint,
            region,
            credential.access_key,
            credential.secret_key,
        )?;
        let key = format!("media/{DISPOSABLE_OBJECT}/{}", scope.generation);
        let version = &scope.version;
        let sibling = &scope.sibling_version;
        if version == sibling || !super::super::readback::valid_version(sibling) {
            return Err(Refused);
        }
        let until = super::super::now() + Duration::from_secs(20);
        let require_present = async |pin: &str| {
            let present = remover.send("HEAD", BUCKET, &key, pin, until).await?;
            if present.status != 200 || !present.body.is_empty() {
                return Err(Unconfirmed);
            }
            check_version(&present, pin, true)
        };
        require_present(version).await?;
        require_present(sibling).await?;
        for _ in 0..2 {
            let absent = remover
                .delete_and_read_absence(BUCKET, &key, version, until)
                .await?;
            check_version(&absent, version, false)?;
        }
        require_present(sibling).await
    }
    .await;
    match result {
        Ok(()) => "qualified",
        Err(Refused) => "refused",
        Err(Unconfirmed) => "unconfirmed",
    }
    .into()
}
