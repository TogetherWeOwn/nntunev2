fn history_call(
    h: &Harness,
    cookie: &str,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> Response {
    h.call_full(
        method,
        path,
        Some(body.to_string()),
        Some(cookie.to_string()),
        Some(ORIGIN),
    )
}

fn history_body(event: &str) -> serde_json::Value {
    serde_json::json!({
        "playback_id": "40c325c0-2cee-489d-a8b6-fbe937155671",
        "track_id": "track-1", "event": event,
    })
}

#[test]
fn listen_history_records_lifecycle_idempotently_without_meter_or_lease() {
    let h = harness(); // no grant secret or paid entitlement: history is not playback authorization
    let cookie = play_user(&h);
    let call = |event| {
        history_call(
            &h,
            &cookie,
            "POST",
            crate::listen_history::EVENTS_PATH,
            history_body(event),
        )
    };
    assert_eq!(
        call("complete").status,
        422,
        "terminal requires accepted start"
    );
    assert_eq!(call("start").status, 200);
    assert_eq!(call("start").status, 200);
    assert_eq!(call("complete").status, 200);
    assert_eq!(call("complete").status, 200);
    assert_eq!(call("skip").status, 422, "one terminal slot per playback");
    assert_eq!(h.store.listen_events.borrow().len(), 2);
    let mut changed = history_body("start");
    changed["track_id"] = serde_json::json!("other");
    assert_eq!(
        history_call(
            &h,
            &cookie,
            "POST",
            crate::listen_history::EVENTS_PATH,
            changed
        )
        .status,
        422
    );
    assert!(h.store.playback_leases.borrow().is_empty());
    let exported = history_call(
        &h,
        &cookie,
        "POST",
        crate::listen_history::EXPORT_PATH,
        serde_json::json!({}),
    );
    assert_eq!(exported.status, 200);
    let page: serde_json::Value = serde_json::from_str(&exported.body).unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 2);
    assert_eq!(page["retention"], "account_lifetime");
    assert!(page["next_cursor"].is_null());
    assert!(!exported.body.contains("user-plays-1"));
}

#[test]
fn listen_history_rejects_anonymous_csrf_bad_input_and_storage_failure() {
    let h = harness();
    let cookie = play_user(&h);
    let path = crate::listen_history::EVENTS_PATH;
    assert_eq!(h.call("POST", path, history_body("start")).status, 401);
    for origin in [None, Some("https://wrong.example")] {
        assert_eq!(
            h.call_full(
                "POST",
                path,
                Some(history_body("start").to_string()),
                Some(cookie.clone()),
                origin
            )
            .status,
            401
        );
    }
    let mut bad = history_body("start");
    bad["user_id"] = serde_json::json!("victim");
    assert_eq!(history_call(&h, &cookie, "POST", path, bad).status, 422);
    *h.store.fail_listen.borrow_mut() = true;
    assert_eq!(
        history_call(&h, &cookie, "POST", path, history_body("start")).status,
        500
    );
    *h.store.fail_listen.borrow_mut() = false;
    h.store
        .deleting_users
        .borrow_mut()
        .insert("user-plays-1".into());
    let deleting = history_call(&h, &cookie, "POST", path, history_body("start"));
    assert_eq!(deleting.status, 409);
    assert!(deleting.body.contains("account_deleting"));
    assert!(h.store.listen_events.borrow().is_empty());
}

#[test]
fn listen_history_export_pages_and_clear_are_owner_scoped() {
    use crate::listen_history::{ListenEvent, ListenRow, EXPORT_PAGE_SIZE};
    let h = harness();
    let cookie = play_user(&h);
    seed_user(&h, "other", "other@example.com");
    let other_cookie = magic_login(&h, "other@example.com");
    for id in 1..=EXPORT_PAGE_SIZE + 2 {
        h.store.listen_events.borrow_mut().push((
            "user-plays-1".into(),
            ListenRow {
                id: id.to_string(),
                playback_id: format!("{id:08x}-2cee-489d-a8b6-fbe937155671"),
                track_id: "t".into(),
                event: ListenEvent::Start,
                recorded_at: NOW as i64,
            },
        ));
    }
    assert_eq!(
        history_call(
            &h,
            &other_cookie,
            "POST",
            crate::listen_history::EVENTS_PATH,
            history_body("start")
        )
        .status,
        200
    );
    let first = history_call(
        &h,
        &cookie,
        "POST",
        crate::listen_history::EXPORT_PATH,
        serde_json::json!({}),
    );
    let first: serde_json::Value = serde_json::from_str(&first.body).unwrap();
    assert_eq!(first["events"].as_array().unwrap().len(), EXPORT_PAGE_SIZE);
    let second = history_call(
        &h,
        &cookie,
        "POST",
        crate::listen_history::EXPORT_PATH,
        serde_json::json!({"before_id": first["next_cursor"]}),
    );
    let second: serde_json::Value = serde_json::from_str(&second.body).unwrap();
    assert_eq!(second["events"].as_array().unwrap().len(), 2);
    assert!(second["next_cursor"].is_null());
    assert_eq!(
        history_call(
            &h,
            &cookie,
            "POST",
            crate::listen_history::EXPORT_PATH,
            serde_json::json!({"before_id": "-1"})
        )
        .status,
        422
    );
    assert_eq!(
        history_call(
            &h,
            &cookie,
            "DELETE",
            crate::listen_history::EVENTS_PATH,
            serde_json::json!({})
        )
        .status,
        200
    );
    assert_eq!(h.store.listen_events.borrow().len(), 1);
    assert_eq!(h.store.listen_events.borrow()[0].0, "other");
}

#[test]
fn listen_history_bearer_write_uses_scope_and_never_cookie_rescues() {
    let h = peppered_harness();
    let cookie = play_user(&h);
    let mint = |scopes| {
        let resp = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": scopes, "ttl_secs": 3600}),
        );
        assert_eq!(resp.status, 201, "{}", resp.body);
        mint_secret(&serde_json::from_str::<serde_json::Value>(&resp.body).unwrap())
    };
    let plays = mint(serde_json::json!(["plays:write"]));
    let library = mint(serde_json::json!(["library:read"]));
    let call = |path: &str, secret: &str, cookie: Option<String>| {
        block_on(handle(
            &h.ctx(),
            &Request {
                method: "POST".into(),
                path: path.into(),
                cookie,
                origin: None,
                authorization: Some(format!("Bearer {secret}")),
                stripe_signature: None,
                body: history_body("start").to_string().into_bytes(),
            },
        ))
    };
    assert_eq!(
        call(crate::listen_history::EVENTS_PATH, &plays, None).status,
        200
    );
    assert_eq!(
        call(crate::listen_history::EVENTS_PATH, &library, None).status,
        403
    );
    assert_eq!(
        call(
            crate::listen_history::EVENTS_PATH,
            "ntk_invalid",
            Some(cookie)
        )
        .status,
        404
    );
    assert_eq!(
        call("/v1/plays", &plays, None).status,
        401,
        "history bearer cannot mint grants"
    );
    assert_eq!(
        call(crate::listen_history::EXPORT_PATH, &plays, None).status,
        401,
        "export remains session-only"
    );
    assert!(h.store.playback_leases.borrow().is_empty());
}

#[test]
fn listen_history_throttles_before_unbounded_writes() {
    let h = harness();
    let cookie = play_user(&h);
    for _ in 0..crate::listen_history::EVENTS_PER_MINUTE {
        assert_eq!(
            history_call(
                &h,
                &cookie,
                "POST",
                crate::listen_history::EVENTS_PATH,
                history_body("start")
            )
            .status,
            200
        );
    }
    let refused = history_call(
        &h,
        &cookie,
        "POST",
        crate::listen_history::EVENTS_PATH,
        history_body("start"),
    );
    assert_eq!(refused.status, 429);
    assert_eq!(refused.retry_after, Some(60));
    assert_eq!(h.store.listen_events.borrow().len(), 1);
}
