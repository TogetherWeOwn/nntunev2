use super::listen_history_receiver as receiver;

const RECORD_PATH: &str = "/v1/plays/events";
const CONTEXT_PATH: &str = "/v1/plays/events/context";

fn receive_request(cookie: Option<String>, body: serde_json::Value) -> Request {
    Request {
        method: "POST".into(),
        path: RECORD_PATH.into(),
        cookie,
        origin: Some(ORIGIN.into()),
        authorization: None,
        stripe_signature: None,
        body: body.to_string().into_bytes(),
    }
}

fn receive_start() -> serde_json::Value {
    serde_json::json!({"playback_id": "40c325c0-2cee-489d-a8b6-fbe937155601", "track_id": "catalogue-42", "event": "start"})
}

fn issued_context(response: Response) -> serde_json::Value {
    assert_eq!(response.status, 200);
    assert!(response
        .extra_headers
        .contains(&("cache-control".into(), "private, no-store".into())));
    response_json(&response)["context"].clone()
}

fn response_json(response: &Response) -> serde_json::Value {
    serde_json::from_str(&response.body).unwrap()
}

#[test]
fn unmounted_history_context_tracks_cookie_refresh_and_key_rotation() {
    let mut h = harness();
    seed_user(&h, "listener-a", "alice@example.com");
    seed_user(&h, "listener-b", "bob@example.com");
    let first = magic_login(&h, "alice@example.com");
    let same_account = magic_login(&h, "alice@example.com");
    let other_account = magic_login(&h, "bob@example.com");
    let mut context = receive_request(Some(first.clone()), serde_json::Value::Null);
    context.path = CONTEXT_PATH.into();
    context.origin = Some("https://example.com:443".into());
    assert_eq!(block_on(receiver::context(&h.ctx(), &context)).status, 401);
    context.origin = Some(ORIGIN.into());
    let previous = issued_context(block_on(receiver::context(&h.ctx(), &context)));
    h.config.session_previous_secret =
        Some(std::mem::replace(&mut h.config.session_secret, vec![7; 32]));
    let current = issued_context(block_on(receiver::context(&h.ctx(), &context)));
    assert_ne!(current, previous);
    let refreshed = h.call_full(
        "POST",
        "/v1/session/refresh",
        None,
        Some(first.clone()),
        Some(ORIGIN),
    );
    let refreshed = cookie_of(&refreshed);
    let mut body = receive_start();
    body["context"] = previous;
    for (cookie, origin) in [
        (Some(first), Some(ORIGIN)),
        (Some(same_account), Some(ORIGIN)),
        (Some(other_account), Some(ORIGIN)),
        (None, Some(ORIGIN)),
        (Some(refreshed.clone()), None),
        (Some(refreshed.clone()), Some("https://example.com:443")),
    ] {
        let mut req = receive_request(cookie, body.clone());
        req.origin = origin.map(str::to_owned);
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 401);
    }
    assert!(h.store.listen_owners.borrow().is_empty());
    let mut req = receive_request(Some(refreshed), body.clone());
    assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 200);
    h.config.session_previous_secret = None;
    assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 401);
    body["context"] = current;
    req.body = body.to_string().into_bytes();
    for _ in 1..120 {
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 200);
    }
    let limited = block_on(receiver::write(&h.ctx(), &req));
    assert_eq!((limited.status, limited.retry_after), (429, Some(60)));
    for (method, path) in [("POST", EXPORT_PATH), ("DELETE", CLEAR_PATH)] {
        let response = h.call_full(
            method,
            path,
            Some("{}".into()),
            req.cookie.clone(),
            Some(ORIGIN),
        );
        assert_eq!(response.status, 200);
    }
    assert_eq!(
        h.store.listen_owners.borrow().as_slice(),
        ["listener-a"; 120]
    );
}

#[test]
fn unmounted_history_receiver_keeps_bearer_admission_out_of_the_router() {
    use crate::native_rest::{scope_for_operation, RouteDecision};
    for (scope, status) in [("plays:write", 200), ("library:read", 403)] {
        let (h, cookie, secret) = play_bearer_harness(scope, false);
        let mut req = receive_request(Some(cookie), receive_start());
        req.authorization = Some("Bearer malformed".into());
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, 404);
        req.authorization = Some(format!("Bearer {secret}"));
        assert_eq!(block_on(receiver::context(&h.ctx(), &req)).status, 404);
        assert_eq!(block_on(receiver::write(&h.ctx(), &req)).status, status);
        let mut bound = receive_start();
        bound["context"] = "browser-context".into();
        req.body = bound.to_string().into_bytes();
        assert_eq!(
            block_on(receiver::write(&h.ctx(), &req)).status,
            if status == 200 { 422 } else { 403 }
        );
        let writes = h.store.listen_owners.borrow().len();
        assert_eq!(writes, usize::from(status == 200));
        for path in [RECORD_PATH, CONTEXT_PATH] {
            req.path = path.into();
            assert_eq!(scope_for_operation("POST", path), RouteDecision::Deny);
            assert_eq!(block_on(handle(&h.ctx(), &req)).status, 404);
        }
        assert!(!options_preflight_served(CONTEXT_PATH));
    }
}

#[cfg(all(feature = "postgres-tests", not(target_arch = "wasm32")))]
#[tokio::test]
async fn unmounted_history_receiver_writes_through_pg_store() {
    use crate::listen_history::{
        export,
        postgres_tests::{connect, seed_lease, seed_owner},
        ListenEvent,
    };
    let observer = connect().await;
    let owner = format!("listen-receiver-{:x}", rand::random::<u64>());
    seed_owner(&observer, &owner).await;
    let store =
        crate::worker_entry::playback_store_for_postgres_test(|| async { Ok(connect().await) });
    let h = harness();
    let ctx = Ctx {
        store: &store,
        now: 1000,
        ..h.ctx()
    };
    let cookie = cookie_of(&ctx.start_session(&owner).await.unwrap());
    let mut req = receive_request(Some(cookie), serde_json::Value::Null);
    req.path = CONTEXT_PATH.into();
    let mut body = receive_start();
    body["context"] = issued_context(receiver::context(&ctx, &req).await);
    req.path = RECORD_PATH.into();
    for invalid in [
        b"{}".as_slice(),
        br#"["40c325c0-2cee-489d-a8b6-fbe937155601","catalogue-42","start",null]"#,
    ] {
        req.body = invalid.to_vec();
        let response = receiver::write(&ctx, &req).await;
        assert_eq!(response.status, 422);
        assert_eq!(response_json(&response)["code"], "validation_failed");
    }
    req.body = body.to_string().into_bytes();
    let refused = receiver::write(&ctx, &req).await;
    assert_eq!((refused.status, refused.retry_after), (409, None));
    let envelope = response_json(&refused);
    assert_eq!(envelope["code"], "conflict");
    assert_eq!(envelope["retryable"], false);
    assert!(export(&observer, &owner, None).await.unwrap().is_empty());
    seed_lease(&observer, &owner, 1).await;
    assert_eq!(receiver::write(&ctx, &req).await.status, 200);
    assert_eq!(receiver::write(&ctx, &req).await.status, 200);
    let rows = export(&observer, &owner, None).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event, ListenEvent::Start);
    observer
        .execute("DELETE FROM public.users WHERE id = $1", &[&owner])
        .await
        .unwrap();
}
