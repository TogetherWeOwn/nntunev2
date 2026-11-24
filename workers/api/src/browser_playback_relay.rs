//! Private media relay. The trusted issuer supplies the committed receipt.

use super::{not_found, preference_authenticate, session_binding, unavailable};
use super::{AsyncDns, Ctx, Dialer, Mailer, Request, Response, SingleUseStore, Store};
use crate::playback::LeaseRow;
use crate::{
    play_grant::{self, IssuedGrant},
    session::SessionClaims,
};
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

/// A committed receipt for the member's authorized catalogue track and lease.
#[derive(Clone)]
pub struct BrowserReceipt {
    pub user_id: String,
    pub session_binding: String,
    pub lease: LeaseRow,
    pub artifact_sha256: String,
    pub duration_ms: NonZeroU64,
    pub grant: IssuedGrant,
}

pub(crate) const MEDIA_PREFIX: &str = "/v1/plays/media/";

/// Use the configured Stream service with redirects disabled. Stream the
/// receiver's body with inbound cancellation so it can account for delivery.
pub(crate) struct StreamGet<'a> {
    pub(crate) path: String,
    pub(crate) authorization: String,
    pub(crate) range: Option<&'a str>,
}

pub(crate) async fn private_media<S, M, D, N, T>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
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
    if ctx.cfg.private_listening.is_none() || !admitted(req, fetch_site, range) {
        return Err(not_found());
    }
    let path = req.path.strip_prefix(MEDIA_PREFIX).ok_or_else(not_found)?;
    let mut parts = path.split('/');
    let (Some(track), Some(tab), Some(play), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(not_found());
    };
    if !crate::ledger::is_valid_key_field(track, 512)
        || !crate::ledger::is_valid_key_field(tab, 128)
        || !super::is_private_play_id(play)
    {
        return Err(not_found());
    }
    let (claims, _) = preference_authenticate(ctx, req, false).await?;
    let deadline = ctx
        .private_listening_deadline(&claims.user_id)
        .await?
        .ok_or_else(not_found)?;
    let receipt = ctx
        .store
        .browser_playback_receipt(&claims.user_id, track, tab, play)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(not_found)?;
    let grant = BrowserGrant {
        user_id: &receipt.user_id,
        session_binding: &receipt.session_binding,
        lease: &receipt.lease,
        artifact_sha256: &receipt.artifact_sha256,
        duration_ms: receipt.duration_ms,
        grant: &receipt.grant,
    };
    let lease = authorize(ctx, req, &claims, &grant, "v4").await?;
    if receipt.grant.exp_unix > lease.expires_at.min(deadline).min(claims.expires_at) {
        return Err(not_found());
    }
    Ok(stream_get(StreamGet {
        path: format!("/play/{track}"),
        authorization: play_grant::bearer(&receipt.grant.token),
        range,
    })
    .await)
}

fn admitted(req: &Request, fetch_site: Option<&str>, range: Option<&str>) -> bool {
    req.method == "GET"
        && fetch_site == Some("same-origin")
        && range.is_none_or(|value| {
            value.len() <= 128 && value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
        })
}

async fn authorize<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    req: &Request,
    claims: &SessionClaims,
    grant: &BrowserGrant<'_>,
    version: &str,
) -> Result<LeaseRow, Response>
where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    let LeaseRow {
        session_id,
        track_id,
        play_id,
        epoch,
        ..
    } = grant.lease;
    let secret = ctx.cfg.play_grant_secret.as_deref();
    if claims.user_id != grant.user_id
        || session_binding(claims) != grant.session_binding
        || !valid_grant(secret, grant, ctx.now, version)
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
    lease
        .filter(|lease| {
            lease.is_continuation(session_id, play_id, track_id, ctx.now)
                && lease.epoch == *epoch
                && grant.grant.exp_unix <= lease.continuation_deadline
        })
        .ok_or_else(not_found)
}

#[cfg(test)]
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
    if !admitted(req, fetch_site, range) {
        return Err(not_found());
    }
    let (claims, _) = preference_authenticate(ctx, req, false).await?;
    authorize(ctx, req, &claims, grant, "v3").await?;
    Ok(stream_get(StreamGet {
        path: format!("/play/{}", grant.lease.track_id),
        authorization: play_grant::bearer(&grant.grant.token),
        range,
    })
    .await)
}

fn valid_grant(secret: Option<&[u8]>, grant: &BrowserGrant<'_>, now: u64, version: &str) -> bool {
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
    // Authenticate the exact receipt against the receiver's canonical signed wire format.
    let payload = format!(
        "{}|{track_id}|{play_id}|{epoch}|{exp}|{}",
        grant.user_id, grant.artifact_sha256
    );
    let expected = format!("{version}.{}", crate::b64::encode(payload.as_bytes()));
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
