//! Private media relay. The trusted issuer supplies the committed receipt.

use super::{not_found, preference_authenticate, session_binding, unavailable};
use super::{AsyncDns, Ctx, Dialer, Mailer, Request, Response, SingleUseStore, Store};
use crate::play_grant::{self, IssuedGrant};
use crate::playback::LeaseRow;
use std::num::NonZeroU64;

pub(crate) struct BrowserGrant<'a> {
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

impl BrowserReceipt {
    fn as_grant(&self) -> BrowserGrant<'_> {
        BrowserGrant {
            user_id: &self.user_id,
            session_binding: &self.session_binding,
            lease: &self.lease,
            artifact_sha256: &self.artifact_sha256,
            duration_ms: self.duration_ms,
            grant: &self.grant,
        }
    }

    pub(crate) fn valid_private_grant(&self, secret: &[u8], now: u64) -> bool {
        super::is_private_play_id(&self.lease.play_id)
            && valid_grant(Some(secret), &self.as_grant(), now, "v4")
    }
}

pub(crate) const MEDIA_PREFIX: &str = "/v1/plays/media/";

/// Use the configured Stream service with redirects disabled. Stream the
/// receiver's body with inbound cancellation so it can account for delivery.
pub(crate) struct StreamGet<'a> {
    pub(crate) path: String,
    pub(crate) authorization: String,
    pub(crate) range: Option<&'a str>,
}

impl StreamGet<'_> {
    pub(crate) fn headers(&self) -> impl Iterator<Item = (&str, &str)> {
        [
            Some(("authorization", self.authorization.as_str())),
            self.range.map(|value| ("range", value)),
        ]
        .into_iter()
        .flatten()
    }
}

pub(crate) async fn relay<S, M, D, N, T>(
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
    let valid_range = |value: &str| {
        value.len() <= 128 && value.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
    };
    if req.method != "GET"
        || fetch_site != Some("same-origin")
        || !range.is_none_or(valid_range)
        || (committed.is_none() && ctx.cfg.private_listening.is_none())
    {
        return Err(not_found());
    }
    let (claims, _) = preference_authenticate(ctx, req, false).await?;
    let receipt;
    let (grant, version, policy_end) = match committed {
        #[cfg(test)]
        Some(grant) => (BrowserGrant { ..*grant }, "v3", None),
        _ => {
            let path = req
                .path
                .strip_prefix(MEDIA_PREFIX)
                .filter(|path| path.len() <= 512 + 128 + 30 + 2)
                .ok_or_else(not_found)?;
            let deadline = ctx
                .private_listening_deadline(&claims.user_id)
                .await?
                .ok_or_else(not_found)?;
            receipt = ctx
                .store
                .browser_playback_receipt(&claims, path, ctx.cfg)
                .await
                .map_err(|_| unavailable())?
                .ok_or_else(not_found)?;
            if !super::is_private_play_id(&receipt.lease.play_id) {
                return Err(not_found());
            }
            (receipt.as_grant(), "v4", Some(deadline))
        }
    };
    let LeaseRow {
        session_id,
        track_id,
        play_id,
        epoch,
        ..
    } = grant.lease;
    let secret = ctx.cfg.play_grant_secret.as_deref();
    if claims.user_id != grant.user_id
        || session_binding(&claims) != grant.session_binding
        || !valid_grant(secret, &grant, ctx.now, version)
        || req.path != format!("{MEDIA_PREFIX}{track_id}/{session_id}/{play_id}")
    {
        return Err(not_found());
    }

    // The browser receipt store already checks canonical publication duration.
    if !crate::private_playback::browser::is_browser_play(play_id) {
        let meta = ctx
            .store
            .track_meta(grant.user_id, track_id, true)
            .await
            .map_err(|_| unavailable())?;
        if meta.track_duration_ms != Some(grant.duration_ms.get()) {
            return Err(not_found());
        }
    }
    let lease = ctx
        .store
        .playback_lease(grant.user_id)
        .await
        .map_err(|_| unavailable())?;
    if !lease.is_some_and(|lease| {
        let cutoff = lease
            .continuation_deadline
            .min(policy_end.map_or(u64::MAX, |end| {
                lease.expires_at.min(end).min(claims.expires_at)
            }));
        lease.is_continuation(session_id, play_id, track_id, ctx.now)
            && lease.epoch == *epoch
            && grant.grant.exp_unix <= cutoff
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

fn valid_grant(secret: Option<&[u8]>, grant: &BrowserGrant<'_>, now: u64, version: &str) -> bool {
    let exp = grant.grant.exp_unix;
    let LeaseRow {
        session_id,
        track_id,
        play_id,
        epoch,
        expires_at,
        continuation_deadline,
    } = grant.lease;
    if exp <= now
        || exp - now > play_grant::MAX_GRANT_TTL
        || (version == "v4" && exp > (*expires_at).min(*continuation_deadline))
        || crate::ledger::derive_play_idempotency_key(grant.user_id, track_id, play_id).is_err()
        || !crate::ledger::is_valid_key_field(session_id, 128)
        || !nntune_core::transcode::canonical_sha256_hex(grant.artifact_sha256)
            .is_ok_and(|canonical| canonical == grant.artifact_sha256)
    {
        return false;
    }
    let Some(secret) = secret else {
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
    signed == expected && crate::private_playback::verify_grant_signature(secret, signed, signature)
}
