use super::listen_history_controls::{CLEAR_PATH, EXPORT_PATH};

#[test]
fn history_export_pages_and_clear_affect_only_the_session_owner() {
    use crate::listen_history::{ListenEvent, ListenRow, EXPORT_PAGE_SIZE};
    let h = harness();
    seed_user(&h, "listener-a", "alice@example.com");
    seed_user(&h, "listener-b", "bob@example.com");
    let alice = magic_login(&h, "alice@example.com");
    let bob = magic_login(&h, "bob@example.com");
    for (owner, count) in [("listener-a", EXPORT_PAGE_SIZE + 1), ("listener-b", 1)] {
        h.store
            .listen_rows
            .borrow_mut()
            .extend((1..=count).map(|index| {
                (
                    owner.into(),
                    ListenRow {
                        playback_id: format!("00000000-0000-4000-8000-{index:012}"),
                        track_id: format!("{owner}-{index}"),
                        event: ListenEvent::Start,
                        recorded_at: NOW as i64,
                    },
                )
            }));
    }
    let export = |cookie: &str, body: serde_json::Value| -> serde_json::Value {
        let resp = h.call_full(
            "POST",
            EXPORT_PATH,
            Some(body.to_string()),
            Some(cookie.into()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        serde_json::from_str(&resp.body).unwrap()
    };
    let first = export(&alice, serde_json::json!({}));
    let events = first["events"].as_array().unwrap();
    assert_eq!(events.len(), EXPORT_PAGE_SIZE);
    let newest_track = format!("listener-a-{}", EXPORT_PAGE_SIZE + 1);
    assert_eq!(events[0]["track_id"], newest_track);
    assert!(events
        .iter()
        .all(|row| row.get("id").is_none() && row.get("user_id").is_none()));
    assert_eq!(first["retention"], "account_lifetime");
    let cursor = first["next_cursor"].as_str().unwrap();
    let last = export(&alice, serde_json::json!({"before_id": cursor}));
    assert_eq!(last["events"].as_array().unwrap().len(), 1);
    assert_eq!(last["events"][0]["track_id"], "listener-a-1");
    assert!(last["next_cursor"].is_null());
    assert_eq!(
        export(&bob, serde_json::json!({"before_id": cursor}))["events"],
        serde_json::json!([])
    );
    for _ in 0..2 {
        let resp = h.call_full("DELETE", CLEAR_PATH, None, Some(alice.clone()), Some(ORIGIN));
        assert_eq!(resp.status, 200, "{}", resp.body);
    }
    assert_eq!(
        export(&alice, serde_json::json!({}))["events"],
        serde_json::json!([])
    );
    assert_eq!(
        export(&bob, serde_json::json!({}))["events"][0]["track_id"],
        "listener-b-1"
    );
}

#[test]
fn history_privacy_routes_require_cookie_origin_and_fresh_clear() {
    let (h, cookie, secret) = play_bearer_harness("plays:write", false);
    for (method, path) in [("POST", EXPORT_PATH), ("DELETE", CLEAR_PATH)] {
        let mut ctx = h.ctx();
        let mut req = Request {
            method: method.into(),
            path: path.into(),
            cookie: None,
            origin: Some(ORIGIN.into()),
            authorization: None,
            stripe_signature: None,
            body: b"{}".to_vec(),
        };
        assert_eq!(block_on(handle(&ctx, &req)).status, 401);
        req.cookie = Some(cookie.clone());
        req.authorization = Some(format!("Bearer {secret}"));
        assert_eq!(block_on(handle(&ctx, &req)).status, 404);
        req.authorization = None;
        req.origin = None;
        assert_eq!(block_on(handle(&ctx, &req)).status, 401);
        req.origin = Some("https://other.example".into());
        assert_eq!(block_on(handle(&ctx, &req)).status, 401);
        req.origin = Some(ORIGIN.into());
        ctx.now = NOW + 601;
        if method == "DELETE" {
            let stale = block_on(handle(&ctx, &req));
            assert_eq!(stale.status, 401, "{}", stale.body);
            assert!(stale.body.contains("reauth_required"));
            req.method = "POST".into();
            ctx.now = NOW;
            for path in [CLEAR_PATH, "/v1/plays/events/context"] {
                req.path = path.into();
                assert_eq!(block_on(handle(&ctx, &req)).status, 404, "{path}");
            }
        } else {
            assert_eq!(block_on(handle(&ctx, &req)).status, 200);
            for body in [r#"{"before_id":"42"}"#, r#"{"user_id":"other"}"#] {
                req.body = body.as_bytes().to_vec();
                assert_eq!(block_on(handle(&ctx, &req)).status, 422);
            }
        }
    }
}

#[test]
fn history_export_and_clear_exhaust_independent_budgets() {
    let h = harness();
    seed_user(&h, "listener-a", "alice@example.com");
    let cookie = magic_login(&h, "alice@example.com");
    for (method, path, limit) in [("DELETE", CLEAR_PATH, 6), ("POST", EXPORT_PATH, 120)] {
        let request = || {
            h.call_full(
                method,
                path,
                Some("{}".into()),
                Some(cookie.clone()),
                Some(ORIGIN),
            )
        };
        for _ in 0..limit {
            assert_eq!(request().status, 200);
        }
        let limited = request();
        assert_eq!(limited.status, 429);
        assert_eq!(limited.retry_after, Some(60));
        let body: serde_json::Value = serde_json::from_str(&limited.body).unwrap();
        assert_eq!(body["code"], "rate_limited");
    }
}
