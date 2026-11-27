//! Additional assertions for the disposable committed catalogue fixture.
use super::*;
use crate::{
    private_playback::{
        browser::{BrowserScope, BrowserStart},
        PrivatePlayback, PrivateReceipt,
    },
    session::SessionClaims,
    worker_entry::playback_privacy_mark,
};

pub(super) async fn receipt_retries(
    reader: &mut Client,
    writer: &Client,
    service: &PrivatePlayback<'_>,
    claims: &SessionClaims,
    legacy: &PrivateReceipt,
    publication: &PublicationPins<'_>,
    root_version: [u8; 16],
) {
    let broader_service = service;
    let broader_policy = service.policy.unwrap();
    let initial_policy = crate::private_listening::PrivateListening {
        owner_user_id: broader_policy.owner_user_id.clone(),
        invite_ids: broader_policy.invite_ids.clone(),
        expires_at: broader_policy.expires_at - 30,
        library_scope: None,
    };
    let service = &PrivatePlayback {
        policy: Some(&initial_policy),
        ..*service
    };
    let incumbent = &legacy.relay().lease;
    let scope = BrowserScope {
        publication: publication.into(),
        root_version,
    };
    let mut input = BrowserStart {
        track_id: incumbent.track_id.clone(),
        session_id: incumbent.session_id.clone(),
        play_id: None,
        request_id: Some("09845e3c-8711-4d64-9c63-997b43d09a6b".into()),
    };
    let set_private = async |tab: &str| {
        writer.execute(
            "INSERT INTO public.private_sessions (user_id, session_id, expires_at) VALUES ($1, $2, $3)",
            &[&MEMBER, &tab, &(claims.expires_at as i64)],
        ).await.unwrap();
    };
    let mark_count = async || {
        writer
            .query_one(
                "SELECT count(*) FROM public.private_plays WHERE user_id = $1",
                &[&MEMBER],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    let open = async |db: &mut Client, identity: &SessionClaims, request: &BrowserStart, scope| {
        service
            .open_browser(db, identity, request, scope, SystemTime::now)
            .await
            .unwrap()
    };
    let disabled = PrivatePlayback {
        policy: None,
        ..*service
    };
    assert!(disabled
        .open_browser(reader, claims, &input, scope, SystemTime::now)
        .await
        .unwrap()
        .is_none());
    let before = mark_count().await;
    set_private(&input.session_id).await;
    for unavailable in [
        BrowserScope {
            publication: crate::private_catalogue_read::PublicationScope {
                bucket: "unpublished-fixture-bucket",
                ..scope.publication
            },
            ..scope
        },
        BrowserScope {
            root_version: [255; 16],
            ..scope
        },
    ] {
        assert!(open(reader, claims, &input, unavailable).await.is_none());
    }
    assert_eq!(mark_count().await, before);
    assert_eq!(
        query_playback_lease_row(writer, MEMBER).await.unwrap(),
        Some(incumbent.clone())
    );
    let started = SystemTime::now();
    let browser = service
        .open_browser(reader, claims, &input, scope, || started)
        .await
        .unwrap()
        .unwrap();
    let original = browser.relay();
    assert_eq!(mark_count().await, before + 1);
    let grant = original.grant.token.as_str();
    let receipt = service
        .load_for_stream(reader, grant, scope, SystemTime::now)
        .await
        .unwrap()
        .unwrap();
    let cutoff = original.lease.expires_at;
    assert_eq!(cutoff, original.lease.continuation_deadline);
    let after_native_ttl = started + Duration::from_secs(crate::playback::LEASE_TTL_SECS + 1);
    assert!(UNIX_EPOCH + Duration::from_secs(cutoff) > after_native_ttl);
    let retained = service
        .load_for_stream(reader, grant, scope, || after_native_ttl)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.relay().lease, original.lease);
    let root = crate::storage_digest_read::custody::PlaybackRoot {
        version: root_version,
        secret: &[17; 32],
    };
    let resolved = service
        .resolve(reader, &receipt, grant, &root, SystemTime::now)
        .await
        .unwrap()
        .unwrap();
    let snapshot =
        crate::worker_entry::private_browser_custody::project(&receipt, resolved, grant).unwrap();
    let pins = snapshot.pins().unwrap();
    assert_eq!(snapshot.catalogue_reference, pins.reference_id);
    assert_eq!(snapshot.media_version, publication.version);
    assert_eq!(snapshot.page_hashes.len(), snapshot.page_versions.len());
    assert!(!snapshot.page_hashes.is_empty());
    assert!(snapshot.usable_until_unix <= cutoff);
    let wrong_grant = format!("{grant}x");
    assert!(service
        .load_for_stream(reader, &wrong_grant, scope, SystemTime::now)
        .await
        .unwrap()
        .is_none());
    let stored_receipt = async || {
        writer
            .query_one(
                "SELECT browser_receipt::text FROM public.playback_leases WHERE user_id = $1",
                &[&MEMBER],
            )
            .await
            .unwrap()
            .get::<_, Option<String>>(0)
    };
    let write_receipt = async |value: Option<&str>| {
        assert_eq!(writer.execute(
            "UPDATE public.playback_leases SET browser_receipt = $2::text::jsonb WHERE user_id = $1",
            &[&MEMBER, &value],
        ).await.unwrap(), 1);
    };
    let saved = stored_receipt().await.unwrap();
    let mut mismatched: serde_json::Value = serde_json::from_str(&saved).unwrap();
    mismatched["epoch"] = serde_json::json!(original.lease.epoch + 1);
    for value in [None, Some(mismatched.to_string())] {
        write_receipt(value.as_deref()).await;
        let before = stored_receipt().await;
        assert!(service
            .open_browser(reader, claims, &input, scope, SystemTime::now)
            .await
            .is_err());
        assert!(service
            .load_for_stream(reader, grant, scope, SystemTime::now)
            .await
            .is_err());
        assert_eq!(
            query_playback_lease_row(writer, MEMBER).await.unwrap(),
            Some(original.lease.clone())
        );
        assert_eq!(stored_receipt().await, before);
    }
    write_receipt(Some(&saved)).await;
    let replay = open(reader, claims, &input, scope).await.unwrap();
    assert_eq!(replay.relay().lease, original.lease);
    assert!(replay.relay().grant.token == original.grant.token);
    let expiry = UNIX_EPOCH + Duration::from_secs(cutoff);
    assert!(service
        .open_browser(reader, claims, &input, scope, || expiry)
        .await
        .unwrap()
        .is_none());
    let key = crate::ledger::derive_play_idempotency_key(
        MEMBER,
        &input.track_id,
        &original.lease.play_id,
    )
    .unwrap();
    assert!(playback_privacy_mark(writer, MEMBER, &key).await.unwrap());
    writer
        .execute(
            "DELETE FROM public.private_sessions WHERE user_id = $1",
            &[&MEMBER],
        )
        .await
        .unwrap();
    input.request_id = None;
    input.play_id = Some(original.lease.play_id.clone());
    let retry = open(reader, claims, &input, scope).await.unwrap();
    assert_eq!(retry.relay().lease, original.lease);
    assert!(retry.relay().grant.token == original.grant.token);
    let second_cookie =
        crate::session::issue_session(&[91; 32], MEMBER, claims.created_at, 600).unwrap();
    let second =
        crate::session::verify_session(&[91; 32], &second_cookie, claims.created_at).unwrap();
    writer.execute(
        "INSERT INTO public.sessions (id, user_id, created_at, expires_at, rotation_counter, revoked) VALUES ($1, $2, $3, $4, 0, false)",
        &[&second.session_id, &MEMBER, &(second.created_at as i64), &(second.expires_at as i64)],
    ).await.unwrap();
    let path = format!(
        "{}/{}/{}",
        input.track_id, input.session_id, original.lease.play_id
    );
    let loaded = service
        .load_browser(reader, claims, &path, scope, SystemTime::now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.relay().lease, original.lease);
    assert!(service
        .load_browser(reader, &second, &path, scope, SystemTime::now)
        .await
        .unwrap()
        .is_none());
    assert!(service
        .load_browser(reader, claims, "mismatched-path", scope, SystemTime::now)
        .await
        .unwrap()
        .is_none());
    assert!(service
        .load_browser(reader, claims, &path, scope, || expiry)
        .await
        .unwrap()
        .is_none());
    assert!(service
        .open_browser(reader, &second, &input, scope, || expiry)
        .await
        .unwrap()
        .is_none());
    assert!(open(reader, &second, &input, scope).await.is_none());
    input.session_id = "different-tab".into();
    assert!(open(reader, claims, &input, scope).await.is_none());
    input.play_id = None;
    set_private(&input.session_id).await;
    assert!(open(reader, claims, &input, scope).await.is_none());
    input.session_id = original.lease.session_id.clone();
    set_private(&input.session_id).await;
    input.play_id = Some(format!(
        "{}{}",
        crate::private_playback::browser::PREFIX,
        "A".repeat(22)
    ));
    assert!(open(reader, claims, &input, scope).await.is_none());
    input.play_id = Some(original.lease.play_id.clone());

    let wrong_account = SessionClaims {
        user_id: "catalogue-owner".into(),
        ..claims.clone()
    };
    assert!(open(reader, &wrong_account, &input, scope).await.is_none());
    let set_state = async |state: &str| {
        writer
            .query_one(
                "SELECT pg_temp.private_playback_state($1, $2, $3)",
                &[&state, &input.track_id, &publication.accepted_seq],
            )
            .await
            .unwrap();
    };
    for state in ["revision", "invite", "session"] {
        set_state(state).await;
        assert!(
            service
                .open_browser(reader, claims, &input, scope, || expiry)
                .await
                .unwrap()
                .is_none(),
            "{state}"
        );
        assert!(
            service
                .load_for_stream(reader, grant, scope, || started)
                .await
                .unwrap()
                .is_none(),
            "{state}"
        );
        assert_eq!(
            query_playback_lease_row(writer, MEMBER).await.unwrap(),
            Some(original.lease.clone())
        );
    }
    set_state("valid").await;
    let renewed = service
        .open_browser(reader, claims, &input, scope, || expiry)
        .await
        .unwrap()
        .expect("the exact expired browser receipt can be reauthorized");
    let renewed = renewed.relay();
    assert_eq!(renewed.lease.play_id, original.lease.play_id);
    assert_eq!(renewed.lease.epoch, original.lease.epoch + 1);
    assert!(renewed.grant.exp_unix > cutoff);
    assert!(renewed.grant.exp_unix <= claims.expires_at);
    assert!(service
        .load_for_stream(reader, grant, scope, || started)
        .await
        .unwrap()
        .is_none());
    let replayed = service
        .open_browser(reader, claims, &input, scope, || expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replayed.relay().lease, renewed.lease);
    let loaded = service
        .load_browser(reader, claims, &path, scope, || expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.relay().lease, renewed.lease);
    let streamed = service
        .load_for_stream(reader, &renewed.grant.token, scope, || expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(streamed.relay().lease, renewed.lease);
    let expired_again = UNIX_EPOCH + Duration::from_secs(renewed.lease.expires_at);
    let resumed_again = service
        .open_browser(reader, claims, &input, scope, || expired_again)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed_again.relay().lease.play_id, original.lease.play_id);
    assert_eq!(resumed_again.relay().lease.epoch, renewed.lease.epoch + 1);
    let policy_cutoff = UNIX_EPOCH + Duration::from_secs(initial_policy.expires_at);
    let last = broader_service
        .open_browser(reader, claims, &input, scope, || {
            policy_cutoff - Duration::from_secs(1)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.relay().lease.expires_at, initial_policy.expires_at);
    assert!(broader_service
        .open_browser(reader, claims, &input, scope, || policy_cutoff)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        query_playback_lease_row(writer, MEMBER)
            .await
            .unwrap()
            .unwrap(),
        last.relay().lease
    );
    assert_eq!(mark_count().await, before + 1);
    assert!(playback_privacy_mark(writer, MEMBER, &key).await.unwrap());

    // Restore the incumbent for the separate off-route issuer below.
    writer.execute(
        "UPDATE public.playback_leases SET play_id = $2, epoch = $3, continuation_epoch = $3, expires_at = $4, continuation_deadline = $5 WHERE user_id = $1",
        &[&MEMBER, &incumbent.play_id, &(incumbent.epoch as i64),
            &(incumbent.expires_at as i64), &(incumbent.continuation_deadline as i64)],
    ).await.unwrap();
    writer
        .execute(
            "DELETE FROM public.private_sessions WHERE user_id = $1",
            &[&MEMBER],
        )
        .await
        .unwrap();
}
