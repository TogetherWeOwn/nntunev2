fn history_start() -> serde_json::Value {
    serde_json::json!({
        "playback_id": "12345678-1234-4234-8234-123456789abc",
        "track_id": "track-9",
        "event": "start",
    })
}

#[test]
fn history_context_follows_the_verified_session_across_cookie_refresh() {
    let h = harness();
    let login = |email| magic_login(&h, email);
    seed_user(&h, "listener-a", "alice@example.com");
    seed_user(&h, "listener-b", "bob@example.com");
    let first = login("alice@example.com");
    let context_path = listen_history_handlers::CONTEXT_PATH;
    let context =
        |cookie: Option<String>, origin| h.call_full("POST", context_path, None, cookie, origin);
    assert_eq!(context(None, Some(ORIGIN)).status, 401);
    assert_eq!(context(Some(first.clone()), None).status, 401);
    let issued = context(Some(first.clone()), Some(ORIGIN));
    assert_eq!(issued.status, 200);
    assert!(issued
        .extra_headers
        .contains(&("cache-control".into(), "private, no-store".into())));
    let binding =
        serde_json::from_str::<serde_json::Value>(&issued.body).unwrap()["context"].clone();
    let events = crate::listen_history::EVENTS_PATH;
    let call = |cookie: String, body: serde_json::Value| {
        h.call_full(
            "POST",
            events,
            Some(body.to_string()),
            Some(cookie),
            Some(ORIGIN),
        )
    };
    assert_eq!(call(first.clone(), history_start()).status, 401);
    let mut bound = history_start();
    bound["context"] = binding;
    let no_origin = h.call_full(
        "POST",
        events,
        Some(bound.to_string()),
        Some(first.clone()),
        None,
    );
    assert_eq!(no_origin.status, 401);
    let refreshed = h.call_full(
        "POST",
        "/v1/session/refresh",
        None,
        Some(first.clone()),
        Some(ORIGIN),
    );
    assert_eq!(refreshed.status, 200, "{}", refreshed.body);
    assert_eq!(call(first, bound.clone()).status, 401);
    for email in ["alice@example.com", "bob@example.com"] {
        assert_eq!(call(login(email), bound.clone()).status, 401);
    }
    assert!(h.store.listen_writes.borrow().is_empty());
    assert_eq!(call(cookie_of(&refreshed), bound).status, 200);
    let writes = h.store.listen_writes.borrow();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "listener-a");
    assert!(h.store.playback_leases.borrow().is_empty());
}

#[test]
fn history_native_write_requires_plays_scope_without_browser_context() {
    for (scope, expected) in [("plays:write", 200), ("library:read", 403)] {
        let (h, cookie, secret) = play_bearer_harness(scope, false);
        let mut req = Request {
            method: "POST".into(),
            path: crate::listen_history::EVENTS_PATH.into(),
            cookie: None,
            origin: None,
            authorization: Some(format!("Bearer {secret}")),
            stripe_signature: None,
            body: history_start().to_string().into_bytes(),
        };
        assert_eq!(block_on(handle(&h.ctx(), &req)).status, expected);
        assert_eq!(
            h.store.listen_writes.borrow().len(),
            if expected == 200 { 1 } else { 0 }
        );
        req.path = "/v1/plays".into();
        assert_eq!(
            block_on(handle(&h.ctx(), &req)).status,
            if expected == 200 { 401 } else { 403 }
        );
        assert!(h.store.playback_leases.borrow().is_empty());
        req.path = listen_history_handlers::CONTEXT_PATH.into();
        req.cookie = Some(cookie);
        req.origin = Some(ORIGIN.into());
        req.body.clear();
        assert_eq!(block_on(handle(&h.ctx(), &req)).status, 404);
    }
}
