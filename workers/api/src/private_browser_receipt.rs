//! The current browser play's immutable pins live on the same row as its lease.
use super::*;
use crate::{
    ledger::{derive_play_idempotency_key, is_valid_key_field},
    private_catalogue_read::{lookup_scoped, PublicationScope},
    storage_digest_read::DigestReadTarget,
    worker_entry::{
        acquire_playback_lease_with_expiry, classify_play_privacy, lock_playback_account,
        playback_privacy_mark, query_playback_lease_row,
    },
};
use serde::{Deserialize, Serialize};

pub(crate) const PREFIX: &str = "private_browser_";

pub(crate) fn is_browser_play(play: &str) -> bool {
    play.starts_with(PREFIX) && play.len() == PREFIX.len() + 22 && is_valid_key_field(play, 64)
}

fn valid_track(track: &str) -> bool {
    crate::meter_sink::is_uuid_shaped(track) && !track.bytes().any(|b| b.is_ascii_uppercase())
}

fn valid_path(path: &str) -> bool {
    path.split_once('/').is_some_and(|(track, tail)| {
        valid_track(track)
            && tail
                .split_once('/')
                .is_some_and(|(tab, play)| is_valid_key_field(tab, 128) && is_browser_play(play))
    })
}

#[derive(Clone, Copy)]
pub(crate) struct BrowserScope<'a> {
    pub publication: PublicationScope<'a>,
    pub root_version: [u8; 16],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserStart {
    pub track_id: String,
    pub session_id: String,
    #[serde(default, deserialize_with = "present_id")]
    pub play_id: Option<String>,
    pub request_id: Option<String>,
}

fn present_id<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<String>, D::Error> {
    String::deserialize(decoder).map(Some)
}

#[derive(Serialize, Deserialize)]
struct ReceiptPins {
    account_session: String,
    play: String,
    tab: String,
    epoch: u64,
    expires_at: u64,
    #[serde(default)]
    renew_until: Option<u64>,
    request_id: Option<String>,
    accepted_seq: i64,
    target: DigestReadTarget,
    bucket: String,
    version: String,
    duration_ms: NonZeroU64,
    private: bool,
}

async fn current(
    db: &impl PgQuery,
    user: &str,
) -> Result<Option<(LeaseRow, ReceiptPins)>, StoreError> {
    let Some(lease) = query_playback_lease_row(db, user).await? else {
        return Ok(None);
    };
    if !is_browser_play(&lease.play_id) {
        return Ok(None);
    }
    let row = db
        .query_typed_opt(
            "SELECT browser_receipt::text FROM public.playback_leases WHERE user_id = $1 AND play_id = $2 AND epoch = $3",
            &[
                (&user, Type::TEXT),
                (&lease.play_id, Type::TEXT),
                (&i64::try_from(lease.epoch).map_err(|_| StoreError)?, Type::INT8),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let raw: Option<&str> = row.try_get(0).map_err(|_| StoreError)?;
    let pins: ReceiptPins = serde_json::from_str(raw.ok_or(StoreError)?).map_err(|_| StoreError)?;
    if pins.play != lease.play_id
        || pins.tab != lease.session_id
        || pins.epoch != lease.epoch
        || pins.expires_at != lease.expires_at
    {
        // A caller starting a play must not treat inconsistent custody as
        // the absence of a receipt and fall through to lease acquisition.
        return Err(StoreError);
    }
    Ok(Some((lease, pins)))
}

fn duration(row: &tokio_postgres::Row) -> Result<Option<NonZeroU64>, StoreError> {
    let duration: Option<i32> = row.try_get("duration_ms").map_err(|_| StoreError)?;
    Ok(duration
        .and_then(|ms| u64::try_from(ms).ok())
        .and_then(NonZeroU64::new))
}

impl PrivatePlayback<'_> {
    async fn browser_receipt_for(
        &self,
        db: &impl PgQuery,
        claims: &SessionClaims,
        lease: LeaseRow,
        pins: &ReceiptPins,
        scope: BrowserScope<'_>,
        now: u64,
    ) -> Result<Option<PrivateReceipt>, StoreError> {
        let exp = lease.expires_at;
        let Some(policy) = self.policy.filter(|policy| exp <= policy.expires_at) else {
            return Ok(None);
        };
        if claims.session_id != pins.account_session
            || exp <= now
            || exp - now > crate::play_grant::MAX_GRANT_TTL
            || pins.renew_until.is_some_and(|until| exp > until)
            || exp > lease.continuation_deadline.min(claims.expires_at)
            || self.grant_secret.len() < MIN_SECRET_BYTES
            || !authorized(db, policy, claims).await?
        {
            return Ok(None);
        }
        let key = derive_play_idempotency_key(&claims.user_id, &lease.track_id, &lease.play_id)
            .map_err(|_| StoreError)?;
        if playback_privacy_mark(db, &claims.user_id, &key).await? != pins.private {
            return Ok(None);
        }
        let Some((mut music, row)) = lookup_scoped(
            db,
            &lease.track_id,
            scope.publication,
            Some((pins.accepted_seq, &pins.version)),
            None,
        )
        .await?
        else {
            return Ok(None);
        };
        if music.target != pins.target
            || music.target.root_version != scope.root_version
            || music.locator.bucket() != pins.bucket
            || duration(&row)? != Some(pins.duration_ms)
        {
            return Ok(None);
        }
        let digest: String = row.try_get("digest").map_err(|_| StoreError)?;
        music.grant_deadline = UNIX_EPOCH
            .checked_add(Duration::from_secs(exp))
            .ok_or(StoreError)?;
        let grant = mint_grant(
            self.grant_secret,
            &claims.user_id,
            &lease.track_id,
            &lease.play_id,
            lease.epoch,
            exp,
            &digest,
        )?;
        let relay = BrowserReceipt {
            user_id: claims.user_id.clone(),
            session_binding: session_binding(claims),
            lease,
            artifact_sha256: digest,
            duration_ms: pins.duration_ms,
            grant,
        };
        Ok(Some(PrivateReceipt {
            relay,
            session: claims.clone(),
            accepted_seq: pins.accepted_seq,
            music,
        }))
    }

    pub(crate) async fn load_browser(
        &self,
        db: &impl PgQuery,
        claims: &SessionClaims,
        path: &str,
        scope: BrowserScope<'_>,
        now: impl Fn() -> SystemTime,
    ) -> Result<Option<PrivateReceipt>, StoreError> {
        if !valid_path(path) {
            return Ok(None);
        }
        let Some((lease, pins)) = current(db, &claims.user_id).await? else {
            return Ok(None);
        };
        if path != format!("{}/{}/{}", lease.track_id, lease.session_id, lease.play_id) {
            return Ok(None);
        }
        let receipt = self
            .browser_receipt_for(db, claims, lease, &pins, scope, seconds(now())?)
            .await?;
        Ok(receipt.filter(|receipt| now() < receipt.music.grant_deadline))
    }

    /// Stream authenticates independently. Recover the session only from the
    /// committed receipt and bind the caller to the exact reconstructed bearer.
    pub(crate) async fn load_for_stream(
        &self,
        db: &impl PgQuery,
        bearer: &str,
        scope: BrowserScope<'_>,
        now: impl Fn() -> SystemTime,
    ) -> Result<Option<PrivateReceipt>, StoreError> {
        let Some(user) = stream_grant_user(self.grant_secret, bearer, seconds(now())?) else {
            return Ok(None);
        };
        let Some((lease, pins)) = current(db, &user).await? else {
            return Ok(None);
        };
        let Some(row) = db.query_typed_opt(
            "SELECT created_at, expires_at, rotation_counter FROM public.sessions WHERE id = $1 AND user_id = $2 AND NOT revoked",
            &[(&pins.account_session, Type::TEXT), (&user, Type::TEXT)],
        ).await.map_err(|_| StoreError)? else {
            return Ok(None);
        };
        let time = |col| {
            let seconds: i64 = row.try_get(col).map_err(|_| StoreError)?;
            u64::try_from(seconds).map_err(|_| StoreError)
        };
        let created_at = time("created_at")?;
        let claims = SessionClaims {
            session_id: pins.account_session.clone(),
            user_id: user.clone(),
            created_at,
            issued_at: created_at,
            expires_at: time("expires_at")?,
            rotation_counter: u32::try_from(
                row.try_get::<_, i32>("rotation_counter")
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?,
        };
        let receipt = self
            .browser_receipt_for(db, &claims, lease, &pins, scope, seconds(now())?)
            .await?;
        let Some(receipt) = receipt.filter(|receipt| {
            bool::from(
                bearer
                    .as_bytes()
                    .ct_eq(receipt.relay.grant.token.as_bytes()),
            )
        }) else {
            return Ok(None);
        };
        let lease = query_playback_lease_row(db, &user).await?;
        Ok(
            (lease.as_ref() == Some(&receipt.relay.lease) && now() < receipt.music.grant_deadline)
                .then_some(receipt),
        )
    }

    pub(crate) fn catalogue_reference(receipt: &PrivateReceipt) -> [u8; 16] {
        // This is the canonical track in the pinned publication, independent
        // of the later protected-read projection.
        receipt.music.target.reference
    }

    /// Issue under the privacy/deletion fence, or retry the exact current
    /// receipt. An expired receipt can be reissued under the same play
    /// identity after reauthorizing its pinned publication, up to the
    /// original session and policy cutoff.
    pub(crate) async fn open_browser(
        &self,
        db: &mut impl PgConnect,
        claims: &SessionClaims,
        input: &BrowserStart,
        scope: BrowserScope<'_>,
        now: impl Fn() -> SystemTime,
    ) -> Result<Option<PrivateReceipt>, StoreError> {
        let operation = async {
            let Some(policy) = self
                .policy
                .filter(|_| self.grant_secret.len() >= MIN_SECRET_BYTES)
            else {
                return Ok(None);
            };
            let start = seconds(now())?;
            let user = &claims.user_id;
            let track = &input.track_id;
            let tab = &input.session_id;
            let authorized_until = policy.expires_at.min(claims.expires_at);
            if start >= authorized_until
                || !is_valid_key_field(tab, 128)
                || input
                    .play_id
                    .as_deref()
                    .is_some_and(|play| !is_browser_play(play))
                || input
                    .request_id
                    .as_deref()
                    .is_some_and(|id| !crate::meter_sink::is_uuid_shaped(id))
                || (input.play_id.is_some() && input.request_id.is_some())
            {
                return Ok(None);
            }
            let tx = db.read_committed().await.map_err(|_| StoreError)?;
            tx.set_local_statement_timeout(LOOKUP_TIMEOUT_MS)
                .await
                .map_err(|_| StoreError)?;
            if !lock_playback_account(&tx, user).await? || !authorized(&tx, policy, claims).await? {
                return Ok(None);
            }
            let mut continuation = None;
            if let Some((lease, pins)) = current(&tx, user).await? {
                if lease.is_live(start)
                    && lease.session_id == *tab
                    && pins.account_session != claims.session_id
                {
                    return Ok(None);
                }
                let retry = input.play_id.as_ref() == Some(&lease.play_id);
                let replay = input.request_id.is_some() && input.request_id == pins.request_id;
                if retry || replay {
                    if lease.session_id != *tab
                        || lease.track_id != *track
                        || pins.account_session != claims.session_id
                    {
                        return Ok(None);
                    }
                    let checked_at = seconds(now())?;
                    if !retry || lease.is_live(start) {
                        return self
                            .browser_receipt_for(&tx, claims, lease, &pins, scope, checked_at)
                            .await;
                    }
                    continuation = Some(pins);
                }
            }
            // A rejected continuation never falls through to lease acquisition.
            let mut pins = if let Some(pins) = continuation {
                pins
            } else {
                if input.play_id.is_some() {
                    return Ok(None);
                }
                let Some((music, row)) =
                    lookup_scoped(&tx, track, scope.publication, None, None).await?
                else {
                    return Ok(None);
                };
                if music.target.root_version != scope.root_version {
                    return Ok(None);
                }
                let Some(duration_ms) = duration(&row)? else {
                    return Ok(None);
                };
                let S3Binding::VersionId(version) = music.locator.binding() else {
                    return Ok(None);
                };
                let mut play = random_id::<16>().map_err(|_| StoreError)?;
                play.insert_str(0, PREFIX);
                let key =
                    derive_play_idempotency_key(user, track, &play).map_err(|_| StoreError)?;
                ReceiptPins {
                    account_session: claims.session_id.clone(),
                    play,
                    tab: tab.clone(),
                    epoch: 0,
                    expires_at: 0,
                    renew_until: Some(authorized_until),
                    request_id: input.request_id.clone(),
                    accepted_seq: row.try_get("accepted_seq").map_err(|_| StoreError)?,
                    target: music.target,
                    bucket: scope.publication.bucket.into(),
                    version: version.into(),
                    duration_ms,
                    private: classify_play_privacy(&tx, user, tab, &key, start).await?,
                }
            };
            // Older receipts remain readable until expiry, but cannot prove
            // the original renewal cutoff once the lease has expired.
            let Some(renew_until) = pins.renew_until else {
                return Ok(None);
            };
            if start >= renew_until {
                return Ok(None);
            }
            let end = playback::continuation_deadline(
                start,
                crate::meter_sink::TrackMeterMeta {
                    track_duration_ms: Some(pins.duration_ms.get()),
                    ..Default::default()
                },
            )
            .min(authorized_until)
            .min(renew_until)
            .min(start.saturating_add(crate::play_grant::MAX_GRANT_TTL));
            // Native <audio> has no heartbeat. Bound this immutable grant
            // and lease to one track's window and the existing v4 TTL cap.
            let LeaseAcquire::Acquired { epoch } = acquire_playback_lease_with_expiry(
                &tx, user, tab, track, &pins.play, start, end, end, false,
            )
            .await?
            else {
                return Ok(None);
            };
            pins.epoch = epoch;
            pins.expires_at = end;
            let lease = LeaseRow {
                session_id: tab.clone(),
                track_id: track.clone(),
                play_id: pins.play.clone(),
                epoch,
                expires_at: end,
                continuation_deadline: end,
            };
            let Some(receipt) = self
                .browser_receipt_for(&tx, claims, lease, &pins, scope, seconds(now())?)
                .await?
            else {
                return Ok(None);
            };
            let json = serde_json::to_string(&pins).map_err(|_| StoreError)?;
            if tx.execute_typed(
                "UPDATE public.playback_leases SET browser_receipt = $2::text::jsonb WHERE user_id = $1",
                &[(&user, Type::TEXT), (&json, Type::TEXT)],
            ).await.map_err(|_| StoreError)? != 1 {
                return Err(StoreError);
            }
            tx.commit().await.map_err(|_| StoreError)?;
            Ok((now() < receipt.music.grant_deadline).then_some(receipt))
        };
        with_deadline(operation, LOOKUP_TIMEOUT_MS, StoreError).await
    }
}

fn stream_grant_user(secret: &[u8], bearer: &str, now: u64) -> Option<String> {
    if bearer.len() > 1800 {
        return None;
    }
    let (signed, signature) = bearer.rsplit_once('.')?;
    let encoded = signed.strip_prefix("v4.")?;
    if !verify_grant_signature(secret, signed, signature) {
        return None;
    }
    let payload = crate::b64::decode(encoded).ok()?;
    let mut parts = std::str::from_utf8(&payload).ok()?.split('|');
    let (user, track, play, epoch, expires, digest) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    let exp = expires.parse::<u64>().ok()?;
    if parts.next().is_some()
        || !is_browser_play(play)
        || derive_play_idempotency_key(user, track, play).is_err()
        || !epoch.bytes().all(|b| b.is_ascii_digit())
        || epoch.parse::<u64>().is_err()
        || !expires.bytes().all(|b| b.is_ascii_digit())
        || exp <= now
        || exp - now > crate::play_grant::MAX_GRANT_TTL
        || !nntune_core::transcode::canonical_sha256_hex(digest).is_ok_and(|hash| hash == digest)
    {
        return None;
    }
    Some(user.into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "../tests/support/private_playback_offline_client.rs"]
mod offline;
#[cfg(all(test, not(target_arch = "wasm32")))]
use crate::postgres::PgClient;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_media_path_requires_exact_route_fields() {
        let track = "a1000000-0000-4000-8000-000000000001";
        let play = format!("{PREFIX}{}", "A".repeat(22));
        assert!(valid_path(&format!("{track}/tab_1/{play}")));
        assert!(!valid_path(&format!("{}/tab/{play}", track.to_uppercase())));
        assert!(!valid_path(&format!("{track}//{play}")));
        assert!(!valid_path(&format!("{track}/tab/{play}/extra")));
        assert!(!valid_path(&format!("{track}/tab/private_other")));
    }

    #[test]
    fn stream_grant_requires_live_signed_browser_claims() {
        let key = [42; 32];
        let play = format!("{PREFIX}{}", "A".repeat(22));
        let digest = "a".repeat(64);
        let grant = mint_grant(&key, "member", "track", &play, 1, 120, &digest).unwrap();
        assert_eq!(
            stream_grant_user(&key, &grant.token, 119).as_deref(),
            Some("member")
        );
        assert!(stream_grant_user(&key, &grant.token, 120).is_none());
        assert!(stream_grant_user(&[17; 32], &grant.token, 119).is_none());
        assert!(stream_grant_user(&key, &format!("{}.", grant.token), 119).is_none());
        let non_browser =
            mint_grant(&key, "member", "track", "private_other", 1, 120, &digest).unwrap();
        assert!(stream_grant_user(&key, &non_browser.token, 119).is_none());
        let extra_field =
            mint_grant(&key, "member|other", "track", &play, 1, 120, &digest).unwrap();
        assert!(stream_grant_user(&key, &extra_field.token, 119).is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn stream_receipt_authenticates_before_querying_storage() {
        let db = offline::offline_client().await;
        let key = [42; 32];
        let service = PrivatePlayback {
            policy: None,
            grant_secret: &key,
        };
        let scope = BrowserScope {
            publication: PublicationScope {
                environment: [1; 16],
                library: [2; 16],
                provider_configuration: [3; 16],
                bucket: "test-bucket",
            },
            root_version: [4; 16],
        };
        let load = async |signing_key: &[u8]| {
            let play = format!("{PREFIX}{}", "A".repeat(22));
            let grant = mint_grant(
                signing_key,
                "member",
                "track",
                &play,
                1,
                120,
                &"a".repeat(64),
            )
            .unwrap();
            service
                .load_for_stream(&db, &grant.token, scope, || {
                    UNIX_EPOCH + Duration::from_secs(119)
                })
                .await
        };
        assert!(
            load(&key).await.is_err(),
            "a valid grant reaches the offline store"
        );
        assert!(matches!(load(&[17; 32]).await, Ok(None)));
    }
}
