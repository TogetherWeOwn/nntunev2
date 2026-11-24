//! Internal Stream service projection of the exact committed browser receipt.
use std::time::UNIX_EPOCH;

use nntune_core::playback_custody::{self, CustodySnapshot, LEASE_PATH, RESOLVE_PATH};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use worker::Response;
use zeroize::Zeroizing;

use super::{
    private_browser_worker::{hex, now, service, Settings},
    CollectedBody, Config, Env, PgStore, StoreError, WorkerResult,
};
use crate::{
    handlers,
    private_playback::{PrivatePlayback, PrivateReceipt},
    storage_digest_read::custody::{PlaybackRoot, ResolvedPlayback},
};

fn root_secret(env: &Env) -> Option<Zeroizing<[u8; 32]>> {
    env.secret("PRIVATE_PLAYBACK_ROOT_KEY")
        .ok()
        .and_then(|value| hex(&Zeroizing::new(value.to_string())))
        .map(Zeroizing::new)
}

fn custody_secret(env: &Env) -> Option<Zeroizing<String>> {
    let secret = Zeroizing::new(
        env.secret(playback_custody::SECRET_BINDING)
            .ok()?
            .to_string(),
    );
    (secret.len() >= 32).then_some(secret)
}

pub(super) fn can_open(env: &Env) -> bool {
    super::private_browser_worker::enabled(env)
        && env.service("STREAM").is_ok()
        && custody_secret(env).is_some()
        && root_secret(env).is_some()
}

fn authentic(secret: &[u8], path: &str, bearer: &str, header: Option<&str>) -> bool {
    let signature = header
        .and_then(|header| header.strip_prefix(playback_custody::AUTH_SCHEME))
        .and_then(|header| header.strip_prefix(' '))
        .and_then(hex::<32>);
    playback_custody::signature(secret, path, bearer)
        .zip(signature)
        .is_some_and(|(expected, observed)| bool::from(expected.ct_eq(&observed)))
}

pub(super) async fn dispatch(
    env: &Env,
    store: &PgStore,
    cfg: &Config,
    req: &handlers::Request,
    body: &CollectedBody,
) -> Option<WorkerResult<Response>> {
    if !matches!(req.path.as_str(), RESOLVE_PATH | LEASE_PATH) {
        return None;
    }
    let reply = serve(env, store, cfg, req, body).await;
    Some(reply.or_else(|_| fixed_response(503)))
}

fn fixed_response(status: u16) -> WorkerResult<Response> {
    let mut response = Response::empty()?.with_status(status);
    response
        .headers_mut()
        .set("cache-control", "private, no-store")?;
    Ok(response)
}

async fn serve(
    env: &Env,
    store: &PgStore,
    cfg: &Config,
    req: &handlers::Request,
    body: &CollectedBody,
) -> WorkerResult<Response> {
    let (Some(settings), Some(playback), CollectedBody::Complete(body)) =
        (Settings::load(env), service(cfg), body)
    else {
        return fixed_response(404);
    };
    let Some(bearer) = std::str::from_utf8(body).ok().filter(|s| s.len() <= 1800) else {
        return fixed_response(404);
    };
    let secret = custody_secret(env);
    if req.method != "POST"
        || req.cookie.is_some()
        || !secret.is_some_and(|secret| {
            authentic(
                secret.as_bytes(),
                &req.path,
                bearer,
                req.authorization.as_deref(),
            )
        })
    {
        return fixed_response(404);
    }
    let scope = settings.scope();
    let reply = super::with_deadline(
        async {
            let mut db = store.connect().await?;
            let Some(receipt) = playback.load_for_stream(&db, bearer, scope, now).await? else {
                return Ok(None);
            };
            if req.path == LEASE_PATH {
                return Ok(Some(None));
            }
            let Some(root) = root_secret(env) else {
                return Ok(None);
            };
            let root = PlaybackRoot {
                version: settings.root_version,
                secret: &root,
            };
            let Some(resolved) = playback
                .resolve(&mut db, &receipt, bearer, &root, now)
                .await?
            else {
                return Ok(None);
            };
            Ok(Some(Some(project(&receipt, resolved, bearer)?)))
        },
        4_500,
        StoreError,
    )
    .await;
    match reply {
        Ok(None) => fixed_response(404),
        Ok(Some(None)) => fixed_response(204),
        Ok(Some(Some(snapshot))) => {
            let mut response = Response::from_json(&snapshot)?;
            response
                .headers_mut()
                .set("cache-control", "private, no-store")?;
            Ok(response)
        }
        Err(_) => fixed_response(503),
    }
}

pub(crate) fn project(
    receipt: &PrivateReceipt,
    resolved: ResolvedPlayback,
    bearer: &str,
) -> Result<CustodySnapshot, StoreError> {
    let pins = resolved.read.playback_snapshot()?;
    let locator = resolved.media.locator();
    Ok(CustodySnapshot {
        bearer_sha256: Sha256::digest(bearer.as_bytes()).into(),
        catalogue_reference: PrivatePlayback::catalogue_reference(receipt),
        reference_id: pins.reference_id,
        identities: resolved.context.identities,
        map_id: pins.map_id,
        plaintext_len: pins.plaintext_len,
        plaintext_sha256: pins.plaintext_sha256,
        bucket: locator.bucket().into(),
        key: locator.key().into(),
        media_version: pins.media_version.into(),
        page_versions: pins.page_versions.to_vec(),
        usable_until_unix: pins
            .usable_until
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StoreError)?
            .as_secs(),
        header: resolved.context.header.to_vec(),
        envelope_record: resolved.record.to_vec(),
        page_hashes: resolved.ordered_page_hashes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custody_authentication_binds_the_operation_and_exact_bearer() {
        let secret = [42; 32];
        let bearer = "synthetic-bearer";
        let signature = playback_custody::signature(&secret, RESOLVE_PATH, bearer).unwrap();
        let hex = signature.iter().fold(String::new(), |mut hex, byte| {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").unwrap();
            hex
        });
        let header = format!("{} {hex}", playback_custody::AUTH_SCHEME);
        assert!(authentic(&secret, RESOLVE_PATH, bearer, Some(&header)));
        assert!(!authentic(&secret, LEASE_PATH, bearer, Some(&header)));
        assert!(!authentic(
            &secret,
            RESOLVE_PATH,
            "different",
            Some(&header)
        ));
        assert!(!authentic(&secret, RESOLVE_PATH, bearer, None));
    }
}
