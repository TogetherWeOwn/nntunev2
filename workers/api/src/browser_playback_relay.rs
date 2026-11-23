//! Off-route media relay. The trusted issuer supplies the committed receipt.

use super::{not_found, preference_authenticate, session_binding, unavailable};
use super::{AsyncDns, Ctx, Dialer, Mailer, Request, Response, SingleUseStore, Store};
use crate::play_grant::{self, IssuedGrant};
use crate::playback::LeaseRow;
use hmac::{Hmac, KeyInit, Mac};
use std::num::NonZeroU64;

pub(super) struct BrowserGrant<'a> {
    pub(super) user_id: &'a str,
    pub(super) session_binding: &'a str,
    pub(super) lease: &'a LeaseRow,
    pub(super) artifact_sha256: &'a str,
    pub(super) duration_ms: NonZeroU64,
    pub(super) grant: &'a IssuedGrant,
}

/// Use the configured Stream service with redirects disabled. Stream the
/// receiver's body with inbound cancellation so it can account for delivery.
pub(super) struct StreamGet<'a> {
    pub(super) path: String,
    pub(super) authorization: String,
    pub(super) range: Option<&'a str>,
}

pub(super) async fn relay<S, M, D, N, T>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    committed: Option<&BrowserGrant<'_>>,
    fetch_site: Option<&str>,
    range: Option<&str>,
    stream_get: impl AsyncFnOnce(StreamGet<'_>) -> T,
) -> Result<T, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let grant = committed.ok_or_else(not_found)?;
    let valid_range = |value: &str| {
        value.len() <= 128 && value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
    };
    if req.method != "GET" || fetch_site != Some("same-origin") || !range.is_none_or(valid_range) {
        return Err(not_found());
    }
    let (claims, _) = preference_authenticate(ctx, req, false).await?;
    let LeaseRow {
        session_id,
        track_id,
        play_id,
        epoch,
        ..
    } = grant.lease;
    if claims.user_id != grant.user_id
        || session_binding(&claims) != grant.session_binding
        || !valid_rendition_grant(ctx.cfg.play_grant_secret.as_deref(), grant, ctx.now)
        || req.path != format!("/v1/plays/media/{track_id}/{session_id}/{play_id}")
    {
        return Err(not_found());
    }

    let meta = ctx
        .store
        .track_meta(grant.user_id, track_id, true)
        .await
        .map_err(|_| unavailable())?;
    if meta.track_duration_ms != Some(grant.duration_ms.get()) {
        return Err(not_found());
    }
    let lease = ctx
        .store
        .playback_lease(grant.user_id)
        .await
        .map_err(|_| unavailable())?;
    if !lease.is_some_and(|lease| {
        lease.is_continuation(session_id, play_id, track_id, ctx.now)
            && lease.epoch == *epoch
            && grant.grant.exp_unix <= lease.continuation_deadline
    }) {
        return Err(not_found());
    }

    Ok(stream_get(StreamGet {
        path: format!("/play/{track_id}"),
        authorization: play_grant::bearer(&grant.grant.token),
        range,
    })
    .await)
}

fn valid_rendition_grant(secret: Option<&[u8]>, grant: &BrowserGrant<'_>, now: u64) -> bool {
    let exp = grant.grant.exp_unix;
    let LeaseRow {
        session_id,
        track_id,
        play_id,
        epoch,
        ..
    } = grant.lease;
    if exp <= now
        || exp - now > play_grant::MAX_GRANT_TTL
        || crate::ledger::derive_play_idempotency_key(grant.user_id, track_id, play_id).is_err()
        || !crate::ledger::is_valid_key_field(session_id, 128)
        || !nntune_core::transcode::canonical_sha256_hex(grant.artifact_sha256)
            .is_ok_and(|canonical| canonical == grant.artifact_sha256)
    {
        return false;
    }
    let Some(secret) = secret.filter(|secret| secret.len() >= play_grant::MIN_SECRET_BYTES) else {
        return false;
    };
    // Authenticate the exact receipt against the receiver's canonical v3 wire format.
    let payload = format!(
        "{}|{track_id}|{play_id}|{epoch}|{exp}|{}",
        grant.user_id, grant.artifact_sha256
    );
    let expected = format!("v3.{}", crate::b64::encode(payload.as_bytes()));
    let Some((signed, signature)) = grant.grant.token.rsplit_once('.') else {
        return false;
    };
    if signed != expected || signature.len() != 43 {
        return false;
    };
    let (Ok(signature), Ok(mut mac)) = (
        crate::b64::decode(signature),
        Hmac::<sha2::Sha256>::new_from_slice(secret),
    ) else {
        return false;
    };
    mac.update(signed.as_bytes());
    mac.verify_slice(&signature).is_ok()
}
