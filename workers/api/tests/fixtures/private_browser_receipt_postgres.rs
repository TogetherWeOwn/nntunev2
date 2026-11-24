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
    let wrong_scope = BrowserScope {
        root_version: [255; 16],
        ..scope
    };
    assert!(open(reader, claims, &input, wrong_scope).await.is_none());
    let before = mark_count().await;
    set_private(&input.session_id).await;
    let browser = open(reader, claims, &input, scope).await.unwrap();
    let original = browser.relay();
    assert_eq!(mark_count().await, before + 1);
    let replay = open(reader, claims, &input, scope).await.unwrap();
    assert_eq!(replay.relay().lease, original.lease);
    assert!(replay.relay().grant.token == original.grant.token);
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
    for state in ["revision", "invite", "session"] {
        writer
            .query_one(
                "SELECT pg_temp.private_playback_state($1, $2, $3)",
                &[&state, &input.track_id, &publication.accepted_seq],
            )
            .await
            .unwrap();
        assert!(
            open(reader, claims, &input, scope).await.is_none(),
            "{state}"
        );
    }
    assert_eq!(
        query_playback_lease_row(writer, MEMBER)
            .await
            .unwrap()
            .unwrap(),
        original.lease
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
