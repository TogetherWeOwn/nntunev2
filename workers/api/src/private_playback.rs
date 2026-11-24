//! Off-route private playback. The trusted caller supplies deployment scope
//! and custody, and retains the receipt for the unique lease it created.

use std::{
    num::NonZeroU64,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use hmac::{Hmac, KeyInit, Mac};
use nntune_core::s3_locator::S3Binding;
use subtle::ConstantTimeEq;
use tokio_postgres::types::Type;

use crate::{
    handlers::{
        browser_playback_relay::BrowserReceipt, random_id, session_binding, StoreError,
        PRIVATE_PLAY_PREFIX,
    },
    play_grant::{IssuedGrant, MIN_SECRET_BYTES},
    playback::{self, LeaseAcquire, LeaseRow},
    postgres::{PgConnect, PgQuery},
    private_catalogue_read::{self, lookup, PublicationPins, ResolvedMusic},
    private_listening::PrivateListening,
    session::{verify_session_dual, SessionClaims, SessionKeys},
    storage_digest_read::{
        custody::{resolve_custody, PlaybackRoot, ResolvedPlayback},
        protect_digest_read,
    },
    worker_entry::{acquire_playback_lease_on, with_deadline},
};

/// Write-once value for a server-generated play and lease epoch. Keep the
/// whole receipt in trusted custody for relay and resolution. The v4 bearer
/// authenticates that unique play, while this value retains its exact pins.
pub struct PrivateReceipt {
    relay: BrowserReceipt,
    session: SessionClaims,
    accepted_seq: i64,
    music: ResolvedMusic,
}

impl PrivateReceipt {
    pub fn relay(&self) -> &BrowserReceipt {
        &self.relay
    }
}

pub struct PrivatePlayback<'a> {
    pub policy: Option<&'a PrivateListening>,
    pub grant_secret: &'a [u8],
}

const LOOKUP_TIMEOUT_MS: u64 = 4_000;

fn seconds(now: SystemTime) -> Result<u64, StoreError> {
    let elapsed = now.duration_since(UNIX_EPOCH).map_err(|_| StoreError)?;
    Ok(elapsed.as_secs())
}

async fn authorized(
    db: &impl PgQuery,
    policy: &PrivateListening,
    claims: &SessionClaims,
) -> Result<bool, StoreError> {
    let created = i64::try_from(claims.created_at).map_err(|_| StoreError)?;
    let expires = i64::try_from(claims.expires_at).map_err(|_| StoreError)?;
    db.query_typed_one(
        concat!(
            include_str!("private_listening_access.sql"),
            include_str!("private_playback_session.sql")
        ),
        &[
            (&claims.user_id, Type::TEXT),
            (&policy.owner_user_id, Type::TEXT),
            (&policy.invite_ids, Type::INT8_ARRAY),
            (&claims.session_id, Type::TEXT),
            (&created, Type::INT8),
            (&expires, Type::INT8),
            (&i64::from(claims.rotation_counter), Type::INT8),
        ],
    )
    .await
    .map_err(|_| StoreError)?
    .try_get(0)
    .map_err(|_| StoreError)
}

impl PrivatePlayback<'_> {
    /// Start one new private play with a live signed cookie and current
    /// invitation. Only the server supplies publication pins. The caller
    /// must enforce its origin policy before mounting this as a mutation.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue(
        &self,
        db: &mut impl PgConnect,
        keys: &SessionKeys<'_>,
        cookie: Option<&str>,
        track: &str,
        tab: &str,
        pins: &PublicationPins<'_>,
        now: impl Fn() -> SystemTime,
    ) -> Result<Option<PrivateReceipt>, StoreError> {
        let operation = async {
            let Some(policy) = self
                .policy
                .filter(|_| self.grant_secret.len() >= MIN_SECRET_BYTES)
            else {
                return Ok(None);
            };
            let Some(cookie) = cookie.filter(|token| token.len() <= 2048) else {
                return Ok(None);
            };
            let start = seconds(now())?;
            let Ok(claims) = verify_session_dual(keys, cookie, start) else {
                return Ok(None);
            };
            let user = &claims.user_id;
            if start >= policy.expires_at
                || !crate::ledger::is_valid_key_field(tab, 128)
                || crate::ledger::derive_play_idempotency_key(user, track, "private").is_err()
            {
                return Ok(None);
            }

            let tx = db.read_committed().await.map_err(|_| StoreError)?;
            tx.set_local_statement_timeout(LOOKUP_TIMEOUT_MS)
                .await
                .map_err(|_| StoreError)?;
            if !authorized(&tx, policy, &claims).await? {
                return Ok(None);
            }
            let Some((music, row)) = lookup(&tx, track, pins, None).await? else {
                return Ok(None);
            };
            let duration: Option<i32> = row.try_get("duration_ms").map_err(|_| StoreError)?;
            let Some(duration_ms) = duration
                .and_then(|ms| u64::try_from(ms).ok())
                .and_then(NonZeroU64::new)
            else {
                return Ok(None);
            };
            let end = playback::continuation_deadline(
                start,
                crate::meter_sink::TrackMeterMeta {
                    track_duration_ms: Some(duration_ms.get()),
                    ..Default::default()
                },
            )
            .min(policy.expires_at)
            .min(claims.expires_at);
            let exp = playback::lease_expiry(start, end);
            let deadline = UNIX_EPOCH
                .checked_add(Duration::from_secs(exp))
                .ok_or(StoreError)?;
            let music = ResolvedMusic {
                grant_deadline: deadline,
                ..music
            };
            let mut play_id = random_id::<16>().map_err(|_| StoreError)?;
            play_id.insert_str(0, PRIVATE_PLAY_PREFIX);
            let LeaseAcquire::Acquired { epoch } =
                acquire_playback_lease_on(&tx, user, tab, track, &play_id, start, end, false)
                    .await?
            else {
                return Ok(None);
            };
            let digest: String = row.try_get("digest").map_err(|_| StoreError)?;
            let current = lookup(&tx, track, pins, Some(&digest)).await?;
            if !current.is_some_and(|(current, _)| current.target == music.target)
                || !authorized(&tx, policy, &claims).await?
                || now() >= deadline
            {
                return Ok(None);
            }
            let payload = format!("{user}|{track}|{play_id}|{epoch}|{exp}|{digest}");
            let signed = format!("v4.{}", crate::b64::encode(payload.as_bytes()));
            let mut mac =
                Hmac::<sha2::Sha256>::new_from_slice(self.grant_secret).map_err(|_| StoreError)?;
            mac.update(signed.as_bytes());
            let signature = crate::b64::encode(&mac.finalize().into_bytes());
            let token = format!("{signed}.{signature}");
            let receipt = PrivateReceipt {
                relay: BrowserReceipt {
                    user_id: user.clone(),
                    session_binding: session_binding(&claims),
                    lease: LeaseRow {
                        session_id: tab.into(),
                        track_id: track.into(),
                        play_id,
                        epoch,
                        expires_at: exp,
                        continuation_deadline: end,
                    },
                    artifact_sha256: digest,
                    duration_ms,
                    grant: IssuedGrant {
                        token,
                        exp_unix: exp,
                    },
                },
                session: claims,
                accepted_seq: pins.accepted_seq,
                music,
            };
            tx.commit().await.map_err(|_| StoreError)?;
            Ok((now() < deadline).then_some(receipt))
        };
        with_deadline(operation, LOOKUP_TIMEOUT_MS, StoreError).await
    }

    /// Resolve only the live receipt's exact publication and protected custody.
    pub async fn resolve<C: PgConnect>(
        &self,
        db: &mut C,
        receipt: &PrivateReceipt,
        bearer: &str,
        root: &PlaybackRoot<'_>,
        now: impl Fn() -> SystemTime,
    ) -> Result<Option<ResolvedPlayback>, StoreError> {
        let operation = async {
            let Some(policy) = self.policy else {
                return Ok(None);
            };
            let expected = &receipt.relay;
            let expires = expected.grant.exp_unix;
            let stored = &receipt.music;
            if root.version != stored.target.root_version
                || !bool::from(bearer.as_bytes().ct_eq(expected.grant.token.as_bytes()))
                || expires > policy.expires_at
            {
                return Ok(None);
            }
            let S3Binding::VersionId(version) = stored.locator.binding() else {
                return Ok(None);
            };
            let pins = PublicationPins {
                environment: stored.target.environment,
                library: stored.target.library,
                provider_configuration: stored.target.provider_configuration,
                accepted_seq: receipt.accepted_seq,
                bucket: stored.locator.bucket(),
                version,
            };
            let current = async |db: &mut C| {
                let Some(found) =
                    private_catalogue_read::resolve(db, expected, &pins, self.grant_secret, &now)
                        .await?
                else {
                    return Ok(None);
                };
                let active = found == *stored && authorized(db, policy, &receipt.session).await?;
                Ok::<_, StoreError>(
                    (active && now() < stored.grant_deadline).then_some(found.target),
                )
            };
            let Some(target) = current(db).await? else {
                return Ok(None);
            };
            let until = stored.grant_deadline;
            let Some(read) =
                protect_digest_read(db, target, until, async |_| Some(until), &now).await?
            else {
                return Ok(None);
            };
            let Some(resolved) = resolve_custody(db, read, &stored.locator, root, &now).await?
            else {
                return Ok(None);
            };
            Ok(current(db).await?.is_some().then_some(resolved))
        };
        with_deadline(operation, LOOKUP_TIMEOUT_MS, StoreError).await
    }
}
