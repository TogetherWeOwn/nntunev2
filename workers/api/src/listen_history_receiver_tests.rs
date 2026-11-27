use super::listen_history_receiver as receiver;
use serde_json::{from_str, json, Value};

const CONTEXT_PATH: &str = "/v1/plays/events/context";
const START: &str = r#"{"playback_id":"40c325c0-2cee-489d-a8b6-fbe937155601","track_id":"catalogue-42","event":"start"}"#;

fn receive_request(cookie: Option<String>) -> Request {
    Request {
        method: "POST".into(),
        path: CONTEXT_PATH.into(),
        cookie,
        origin: Some(ORIGIN.into()),
        authorization: None,
        stripe_signature: None,
        body: START.as_bytes().to_vec(),
    }
}

fn bind_start(response: Response) -> Vec<u8> {
    assert_eq!(response.status, 200);
    let no_store = ("cache-control".into(), "private, no-store".into());
    assert!(response.extra_headers.contains(&no_store));
    let mut event: Value = from_str(START).unwrap();
    event["context"] = from_str::<Value>(&response.body).unwrap()["context"].clone();
    event.to_string().into_bytes()
}

#[test]
fn unmounted_history_context_tracks_cookie_refresh_and_key_rotation() {
    let mut h = harness();
    seed_user(&h, "listener-a", "alice@example.com");
    let first = magic_login(&h, "alice@example.com");
    let same = magic_login(&h, "alice@example.com");
    let mut req = receive_request(Some(first.clone()));
    for origin in [None, Some("https://example.com:443")] {
        req.origin = origin.map(str::to_owned);
        assert_eq!(block_on(receiver::context(&h.ctx(), &req)).status, 401);
    }
    req.origin = Some(ORIGIN.into());
    let previous = bind_start(block_on(receiver::context(&h.ctx(), &req)));
    h.config.session_previous_secret =
        Some(std::mem::replace(&mut h.config.session_secret, vec![7; 32]));
    let current = bind_start(block_on(receiver::context(&h.ctx(), &req)));
    req.path = "/v1/session/refresh".into();
    req.body.clear();
    let refreshed = cookie_of(&block_on(handle(&h.ctx(), &req)));
    req.path = CLEAR_PATH.into();
    req.body = previous;
    let write = |h: &Harness, req: &Request| block_on(receiver::write(&h.ctx(), req));
    for cookie in [None, Some(first), Some(same)] {
        req.cookie = cookie;
        assert_eq!(write(&h, &req).status, 401);
    }
    req.cookie = Some(refreshed);
    req.origin = Some("https://example.com:443".into());
    assert_eq!(write(&h, &req).status, 401);
    req.origin = Some(ORIGIN.into());
    assert_eq!(write(&h, &req).status, 200);
    h.config.session_previous_secret = None;
    assert_eq!(write(&h, &req).status, 401);
    req.body = current;
    for _ in 1..120 {
        assert_eq!(write(&h, &req).status, 200);
    }
    let limited = write(&h, &req);
    assert_eq!((limited.status, limited.retry_after), (429, Some(60)));
    let owners = h.store.listen_owners.borrow();
    assert_eq!(owners.as_slice(), ["listener-a"; 120]);
    req.body = b"{}".to_vec();
    req.path = EXPORT_PATH.into();
    assert_eq!(block_on(handle(&h.ctx(), &req)).status, 200);
    req.path = CLEAR_PATH.into();
    req.method = "DELETE".into();
    assert_eq!(block_on(handle(&h.ctx(), &req)).status, 200);
}

#[test]
fn unmounted_history_receiver_keeps_bearer_admission_out_of_the_router() {
    use crate::native_rest::{scope_for_operation, RouteDecision::Deny};
    let (h, _, secret) = play_bearer_harness("plays:write", false);
    seed_user(&h, "listener-b", "bob@example.com");
    let other = magic_login(&h, "bob@example.com");
    let mut req = receive_request(None);
    req.authorization = Some(format!("Bearer {secret}"));
    for cookie in [None, Some(other)] {
        req.cookie = cookie;
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 200);
    }
    assert_eq!(h.store.listen_owners.take(), ["user-plays-1"; 2]);
    assert_eq!(block_on(receiver::context(&h.ctx(), &req)).status, 404);
    for path in [CLEAR_PATH, CONTEXT_PATH] {
        assert_eq!(scope_for_operation("POST", path), Deny);
    }
    assert!(!options_preflight_served(CONTEXT_PATH));
    assert_history_rejections(&h, req);
    let (h, cookie, secret) = play_bearer_harness("library:read", false);
    let mut req = receive_request(Some(cookie));
    req.body = bind_start(block_on(receiver::context(&h.ctx(), &req)));
    let bearer = format!("Bearer {secret}");
    for (header, status) in [("Bearer malformed", 404), (&bearer, 403)] {
        req.authorization = Some(header.into());
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, status);
    }
    assert!(h.store.listen_owners.borrow().is_empty());
    req.authorization = None;
    assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 200);
}

fn assert_history_rejections(h: &Harness, mut req: Request) {
    let write = |req: &Request| block_on(receiver::write(&h.ctx(), req));
    for (key, value) in [
        ("extra", json!(1)),
        ("context", json!("browser")),
        ("playback_id", json!("bad")),
        ("track_id", json!("")),
        ("track_id", json!("x".repeat(129))),
        ("track_id", json!("x\ny")),
    ] {
        let mut event: Value = from_str(START).unwrap();
        event[key] = value;
        req.body = event.to_string().into_bytes();
        let response = write(&req);
        assert_eq!(response.status, 422, "{key}: {}", response.body);
    }
    let array = r#"["40c325c0-2cee-489d-a8b6-fbe937155601","catalogue-42","start",null]"#;
    req.body = array.as_bytes().to_vec();
    assert_eq!(write(&req).status, 422);
    req.body = START.as_bytes().to_vec();
    req.body.resize(MAX_BODY_BYTES + 1, b' ');
    assert_eq!(write(&req).status, 400);
    assert!(h.store.listen_owners.borrow().is_empty());
    req.body.truncate(START.len());
    for (failure, attempts) in [("plays.history", 0), ("write", 1)] {
        h.store.listen_failure.set(Some(failure));
        let resp = write(&req);
        assert_eq!(resp.status, 500);
        assert_eq!(resp.body, r#"{"error":"internal error"}"#);
        assert_eq!(h.store.listen_owners.borrow().len(), attempts);
    }
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
fn assert_history_conflict(response: Response) {
    assert_eq!((response.status, response.retry_after), (409, None));
    let envelope: Value = from_str(&response.body).unwrap();
    assert_eq!(envelope["code"], "conflict");
    assert_eq!(envelope["retryable"], false);
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
#[tokio::test]
async fn unmounted_history_receiver_writes_through_pg_store() {
    use crate::account_deletion::{ACCOUNT_DELETING_CODE, SCHEDULE_ACCOUNT_DELETION_SQL};
    use crate::listen_history::postgres_tests::{connect, seed_lease, seed_owner};
    use crate::listen_history::{export, ListenEvent, ListenRow};
    use crate::worker_entry::playback_store_for_postgres_test;
    let mut observer = connect().await;
    let suffix = rand::random::<u64>();
    let owner = format!("listen-receiver-{suffix:x}");
    let other = format!("listen-receiver-{suffix:x}-other");
    seed_owner(&observer, &owner).await;
    seed_owner(&observer, &other).await;
    let store = playback_store_for_postgres_test(|| async { Ok(connect().await) });
    let h = harness();
    let mut ctx = Ctx {
        cfg: &h.config,
        store: &store,
        mailer: &h.mailer,
        dialer: &h.dialer,
        dns: &h.dns,
        stripe: &h.stripe,
        now: 1000,
        source_ip: h.ctx().source_ip,
    };
    let mut start = receive_request(Some(cookie_of(&ctx.start_session(&owner).await.unwrap())));
    start.body.clear();
    assert_eq!(handle(&ctx, &start).await.status, 404);
    start.body = bind_start(receiver::context(&ctx, &start).await);
    start.path = CLEAR_PATH.into();
    assert_history_conflict(receiver::write(&ctx, &start).await);
    assert!(export(&observer, &owner, None).await.unwrap().is_empty());

    let mut other_start =
        receive_request(Some(cookie_of(&ctx.start_session(&other).await.unwrap())));
    other_start.body = bind_start(receiver::context(&ctx, &other_start).await);
    other_start.path = CLEAR_PATH.into();
    seed_lease(&observer, &other, 1).await;
    assert_eq!(receiver::write(&ctx, &other_start).await.status, 200);
    let other_rows = export(&observer, &other, None).await.unwrap();
    assert_eq!(other_rows.len(), 1);

    let start_event: Value = serde_json::from_slice(&start.body).unwrap();
    let with_field = |request: &Request, key: &str, value: &str| {
        let mut body: Value = serde_json::from_slice(&request.body).unwrap();
        body[key] = json!(value);
        Request {
            body: serde_json::to_vec(&body).unwrap(),
            ..request.clone()
        }
    };
    let complete = with_field(&start, "event", "complete");
    let skip = with_field(&start, "event", "skip");

    // Neither the other owner's grant nor their admitted start can authorize this owner.
    assert_history_conflict(receiver::write(&ctx, &start).await);
    assert_history_conflict(receiver::write(&ctx, &complete).await);
    seed_lease(&observer, &owner, 1).await;
    assert_history_conflict(receiver::write(&ctx, &complete).await);
    let wrong_track = with_field(&start, "track_id", "catalogue-43");
    assert_history_conflict(receiver::write(&ctx, &wrong_track).await);
    assert!(export(&observer, &owner, None).await.unwrap().is_empty());

    ctx.now = 1044;
    assert_eq!(handle(&ctx, &start).await.status, 404);
    assert!(export(&observer, &owner, None).await.unwrap().is_empty());
    assert_eq!(receiver::write(&ctx, &start).await.status, 200);
    ctx.now = 1045;
    let expired_start = with_field(
        &start,
        "playback_id",
        "40c325c0-2cee-489d-a8b6-fbe937155602",
    );
    seed_lease(&observer, &owner, 2).await;
    assert_history_conflict(receiver::write(&ctx, &expired_start).await);
    assert_eq!(receiver::write(&ctx, &start).await.status, 200);
    ctx.now = 1046;
    assert_eq!(receiver::write(&ctx, &complete).await.status, 200);
    ctx.now = 1047;
    assert_eq!(receiver::write(&ctx, &complete).await.status, 200);
    assert_history_conflict(receiver::write(&ctx, &skip).await);

    let expected = |event, recorded_at| ListenRow {
        playback_id: start_event["playback_id"].as_str().unwrap().into(),
        track_id: start_event["track_id"].as_str().unwrap().into(),
        event,
        recorded_at,
    };
    let owner_rows = [
        expected(ListenEvent::Complete, 1046),
        expected(ListenEvent::Start, 1044),
    ];
    assert_eq!(export(&observer, &owner, None).await.unwrap(), owner_rows);
    assert_eq!(export(&observer, &other, None).await.unwrap(), other_rows);

    let other_skip = with_field(&other_start, "event", "skip");
    assert_eq!(receiver::write(&ctx, &other_skip).await.status, 200);
    let other_rows = [expected(ListenEvent::Skip, 1047), other_rows[0].clone()];
    assert_eq!(export(&observer, &other, None).await.unwrap(), other_rows);

    observer
        .query_one(SCHEDULE_ACCOUNT_DELETION_SQL, &[&owner])
        .await
        .unwrap();
    let response = receiver::write(&ctx, &complete).await;
    assert_eq!((response.status, response.retry_after), (409, None));
    let envelope: Value = from_str(&response.body).unwrap();
    assert_eq!(envelope["code"], ACCOUNT_DELETING_CODE);
    assert_eq!(export(&observer, &owner, None).await.unwrap(), owner_rows);

    // Model the final database transaction after the grace period. The
    // operation is a restrictive FK, so remove it before deleting the user.
    let tx = observer.transaction().await.unwrap();
    tx.execute(
        "DELETE FROM public.delete_operations WHERE user_id = $1",
        &[&owner],
    )
    .await
    .unwrap();
    let delete_user = "DELETE FROM public.users WHERE id = $1";
    assert_eq!(tx.execute(delete_user, &[&owner]).await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert!(export(&observer, &owner, None).await.unwrap().is_empty());
    assert_eq!(export(&observer, &other, None).await.unwrap(), other_rows);
    observer.execute(delete_user, &[&other]).await.unwrap();
    assert_transition_write_boundaries(&ctx, &observer).await;
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
async fn assert_transition_write_boundaries<S, M, D, N>(
    ctx: &Ctx<'_, S, M, D, N>,
    observer: &tokio_postgres::Client,
) where
    S: Store + SingleUseStore,
    M: Mailer,
    D: Dialer,
    N: AsyncDns,
{
    use crate::listen_history::postgres_tests::{connect, seed_owner};
    use crate::listen_history::{export, ListenEvent, ListenRow};
    use crate::playback::LeaseAcquire;
    use crate::worker_entry::private_aware_lease_for_postgres_test;

    let owner = format!("listen-transition-{:x}", rand::random::<u64>());
    seed_owner(observer, &owner).await;
    let mut ctx = Ctx { now: 2000, ..*ctx };
    let mut base = receive_request(Some(cookie_of(&ctx.start_session(&owner).await.unwrap())));
    base.body.clear();
    let context_resp = receiver::context(&ctx, &base).await;
    assert_eq!(context_resp.status, 200);
    let context_token = from_str::<Value>(&context_resp.body).unwrap()["context"].clone();
    base.path = CLEAR_PATH.into();

    let play_ids = |n: u8| {
        let play_id = crate::b64::encode(&[
            0x40, 0xc3, 0x25, 0xc0, 0x2c, 0xee, 0x48, 0x9d, 0xa8, 0xb6, 0xfb, 0xe9, 0x37, 0x15,
            0x56, n,
        ]);
        (
            play_id,
            format!("40c325c0-2cee-489d-a8b6-fbe9371556{n:02x}"),
        )
    };
    let req = |playback_id: &str, track_id: &str, event: &str| {
        Request {
        body: serde_json::to_vec(&json!({
            "playback_id": playback_id, "track_id": track_id, "event": event, "context": context_token,
        }))
        .unwrap(),
        ..base.clone()
    }
    };
    let acquire = |track_id: &'static str, play_id: String, epoch: u64| {
        let owner = owner.clone();
        async move {
            let res = private_aware_lease_for_postgres_test(
                connect().await,
                &owner,
                "tab-transition",
                track_id,
                &play_id,
                2000,
                2045,
                false,
            )
            .await
            .unwrap();
            assert_eq!(res, LeaseAcquire::Acquired { epoch });
        }
    };
    let (play_a, uuid_a) = play_ids(0x11);
    let (play_b, uuid_b) = play_ids(0x12);
    let (play_c, uuid_c) = play_ids(0x13);

    // Track A starts, then rapid skip acquires Track B (never started) and Track C.
    acquire("track-a", play_a, 1).await;
    assert_eq!(
        receiver::write(&ctx, &req(&uuid_a, "track-a", "start"))
            .await
            .status,
        200
    );
    acquire("track-b", play_b, 2).await;
    acquire("track-c", play_c, 3).await;
    // Rapid-skipped Track B never started; Track A's delayed terminal rejects wrong track,
    // commits once idempotently, and refuses `complete` after `skip`; Track C replays start
    // and complete idempotently while refusing mismatched track or `skip` after `complete`.
    for (now, playback_id, track_id, event, ok) in [
        (2005, uuid_b.as_str(), "track-b", "start", false),
        (2005, uuid_b.as_str(), "track-b", "complete", false),
        (2005, uuid_b.as_str(), "track-b", "skip", false),
        (2005, uuid_a.as_str(), "track-c", "skip", false),
        (2005, uuid_a.as_str(), "track-a", "skip", true),
        (2006, uuid_a.as_str(), "track-a", "skip", true),
        (2006, uuid_a.as_str(), "track-a", "complete", false),
        (2010, uuid_c.as_str(), "track-c", "start", true),
        (2011, uuid_c.as_str(), "track-c", "start", true),
        (2011, uuid_c.as_str(), "track-a", "complete", false),
        (2020, uuid_c.as_str(), "track-c", "complete", true),
        (2021, uuid_c.as_str(), "track-c", "complete", true),
        (2021, uuid_c.as_str(), "track-c", "skip", false),
    ] {
        ctx.now = now;
        let resp = receiver::write(&ctx, &req(playback_id, track_id, event)).await;
        if ok {
            assert_eq!(resp.status, 200, "{track_id}:{event}");
        } else {
            assert_history_conflict(resp);
        }
    }

    let row = |playback_id: &str, track_id: &str, event, recorded_at| ListenRow {
        playback_id: playback_id.into(),
        track_id: track_id.into(),
        event,
        recorded_at,
    };
    assert_eq!(
        export(observer, &owner, None).await.unwrap(),
        [
            row(&uuid_c, "track-c", ListenEvent::Complete, 2020),
            row(&uuid_c, "track-c", ListenEvent::Start, 2010),
            row(&uuid_a, "track-a", ListenEvent::Skip, 2005),
            row(&uuid_a, "track-a", ListenEvent::Start, 2000),
        ]
    );
    observer
        .execute("DELETE FROM public.users WHERE id = $1", &[&owner])
        .await
        .unwrap();
}
