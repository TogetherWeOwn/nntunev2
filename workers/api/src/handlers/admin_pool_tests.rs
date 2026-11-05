// Admin pool registry handler tests. Included inside `mod tests` so the
// FakeStore, harness and session helpers are shared.

const POOL_ID: &str = "11111111-1111-4111-8111-111111111111";
const POOL_ID_B: &str = "22222222-2222-4222-8222-222222222222";
const POOL_USER_MARKER: &str = "synthetic-pool-user-marker";
const POOL_PASS_MARKER: &str = "synthetic-pool-pass-marker";
const POOL_PASS_ROTATED: &str = "synthetic-pool-rotated-marker";
const POOL_PATH: &str = "/v1/admin/pool-providers";

fn pool_settings(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "label": "Primary", "host": "news.example.com", "port": 563,
        "tls": true, "username": POOL_USER_MARKER, "can_read": true,
        "can_post": false, "max_connections": 10, "stream_reserve": 2,
        "ingest_limit": 4, "backbone": "alpha", "tier": 1, "enabled": true,
    })
}

fn pool_body(
    settings: serde_json::Value,
    expected_revision: u64,
    password: Option<&str>,
) -> String {
    let mut body = serde_json::json!({
        "settings": settings, "expected_revision": expected_revision,
    });
    if let Some(password) = password {
        body["password"] = serde_json::json!(password);
    }
    body.to_string()
}

fn pool_call(h: &Harness, method: &str, path: &str, body: String, cookie: &str) -> Response {
    h.call_full(
        method,
        path,
        Some(body),
        Some(cookie.to_string()),
        Some(ORIGIN),
    )
}

fn pool_create(h: &Harness, cookie: &str, id: &str) -> Response {
    pool_call(
        h,
        "POST",
        POOL_PATH,
        pool_body(pool_settings(id), 0, Some(POOL_PASS_MARKER)),
        cookie,
    )
}

fn pool_put(
    h: &Harness,
    cookie: &str,
    settings: serde_json::Value,
    revision: u64,
    password: Option<&str>,
) -> Response {
    let id = settings["id"].as_str().unwrap().to_string();
    pool_call(
        h,
        "PUT",
        &format!("{POOL_PATH}/{id}"),
        pool_body(settings, revision, password),
        cookie,
    )
}

fn pool_audit_actions(h: &Harness) -> Vec<String> {
    h.store
        .audits
        .borrow()
        .iter()
        .filter(|a| a.action.starts_with("pool_provider."))
        .map(|a| a.action.clone())
        .collect()
}

fn pool_state(h: &Harness) -> (usize, usize, usize) {
    (
        h.store.pool_provider_rows.borrow().len(),
        h.store.pool_provider_secrets.borrow().len(),
        pool_audit_actions(h).len(),
    )
}

fn assert_no_pool_credentials(body: &str, secret_ref: Option<&str>) {
    for marker in [POOL_USER_MARKER, POOL_PASS_MARKER, POOL_PASS_ROTATED] {
        assert!(!body.contains(marker), "credential leaked: {body}");
    }
    for key in [
        "username",
        "password",
        "secret_ref",
        "envelope",
        "ciphertext",
    ] {
        assert!(!body.contains(key), "{key} leaked: {body}");
    }
    if let Some(secret_ref) = secret_ref {
        assert!(!body.contains(secret_ref), "secret identity leaked: {body}");
    }
}

#[test]
fn admin_pool_authorization_collapses_to_404_without_mutation() {
    let h = harness();
    seed_user(&h, "member1", "member@example.com");
    let member_cookie = magic_login(&h, "member@example.com");
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    let body = pool_body(pool_settings(POOL_ID), 0, Some(POOL_PASS_MARKER));

    // Anonymous, member, missing origin and foreign origin all hear the
    // plain 404 for both reads and writes.
    let anon = h.call_full("GET", POOL_PATH, None, None, None);
    assert_eq!(anon.status, 404, "{}", anon.body);
    assert_eq!(anon.body, NOT_FOUND_BODY);
    let member_read = h.call_full("GET", POOL_PATH, None, Some(member_cookie.clone()), None);
    assert_eq!(member_read.status, 404);
    let member_write = pool_call(&h, "POST", POOL_PATH, body.clone(), &member_cookie);
    assert_eq!(member_write.status, 404, "{}", member_write.body);
    assert_eq!(member_write.body, NOT_FOUND_BODY);
    let no_origin = h.call_full(
        "POST",
        POOL_PATH,
        Some(body.clone()),
        Some(admin_cookie.clone()),
        None,
    );
    assert_eq!(no_origin.status, 404, "{}", no_origin.body);
    let bad_origin = h.call_full(
        "POST",
        POOL_PATH,
        Some(body.clone()),
        Some(admin_cookie.clone()),
        Some("https://evil.example"),
    );
    assert_eq!(bad_origin.status, 404, "{}", bad_origin.body);

    // A bearer header is refused even with a valid admin cookie.
    for (method, path) in [
        ("GET", POOL_PATH.to_string()),
        ("POST", POOL_PATH.to_string()),
    ] {
        let req = Request {
            method: method.to_string(),
            path,
            cookie: Some(admin_cookie.clone()),
            origin: Some(ORIGIN.to_string()),
            authorization: Some("Bearer ntk_synthetic".to_string()),
            stripe_signature: None,
            body: body.clone().into_bytes(),
        };
        let resp = block_on(handle(&h.ctx(), &req));
        assert_eq!(resp.status, 404, "{method}: {}", resp.body);
        assert_eq!(resp.body, NOT_FOUND_BODY);
    }

    // A role-read outage is the same 404 for reads.
    *h.store.fail_user_role_lookup.borrow_mut() = true;
    let outage = h.call_full("GET", POOL_PATH, None, Some(admin_cookie.clone()), None);
    assert_eq!(outage.status, 404, "{}", outage.body);
    *h.store.fail_user_role_lookup.borrow_mut() = false;

    assert_eq!(pool_state(&h), (0, 0, 0));
}

#[test]
fn admin_pool_stale_session_steps_up_admins_only() {
    let h = harness();
    seed_user(&h, "member1", "member@example.com");
    let member_cookie = magic_login(&h, "member@example.com");
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    let stale_ctx = Ctx {
        cfg: &h.config,
        store: &h.store,
        mailer: &h.mailer,
        dialer: &h.dialer,
        dns: &h.dns,
        stripe: &h.stripe,
        now: NOW + session::PRIVILEGE_REAUTH_WINDOW_SECS + 1,
        source_ip: "198.51.100.9".to_string(),
    };
    let stale = |method: &str, path: &str, cookie: &str, body: Vec<u8>| {
        let req = Request {
            method: method.to_string(),
            path: path.to_string(),
            cookie: Some(cookie.to_string()),
            origin: Some(ORIGIN.to_string()),
            authorization: None,
            stripe_signature: None,
            body,
        };
        block_on(handle(&stale_ctx, &req))
    };
    let body = pool_body(pool_settings(POOL_ID), 0, Some(POOL_PASS_MARKER)).into_bytes();
    let member = stale("POST", POOL_PATH, &member_cookie, body.clone());
    assert_eq!(member.status, 404, "{}", member.body);
    let admin = stale("POST", POOL_PATH, &admin_cookie, body);
    assert_eq!(admin.status, 401, "{}", admin.body);
    assert_eq!(
        admin.body,
        r#"{"code":"reauth_required","message":"reauth required","retryable":false}"#
    );
    assert_eq!(pool_state(&h), (0, 0, 0));
    // Reads never step up.
    let list = stale("GET", POOL_PATH, &admin_cookie, Vec::new());
    assert_eq!(list.status, 200, "{}", list.body);
}

#[test]
fn admin_pool_demoted_or_deleting_actor_writes_nothing() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");

    // Deleting actor: the transaction's `deleting_at IS NULL` lookup finds
    // no admin, so the write is the plain 404.
    h.store
        .deleting_users
        .borrow_mut()
        .insert("admin1".to_string());
    let deleting = pool_create(&h, &admin_cookie, POOL_ID);
    assert_eq!(deleting.status, 404, "{}", deleting.body);
    assert_eq!(deleting.body, NOT_FOUND_BODY);
    assert_eq!(pool_state(&h), (0, 0, 0));
    h.store.deleting_users.borrow_mut().clear();

    // Demoted after the guard: the store re-check refuses, with the secret
    // not stored either.
    h.store
        .user_roles
        .borrow_mut()
        .insert("admin1".to_string(), crate::admin::ROLE_MEMBER.to_string());
    let demoted = pool_create(&h, &admin_cookie, POOL_ID);
    assert_eq!(demoted.status, 404, "{}", demoted.body);
    assert_eq!(pool_state(&h), (0, 0, 0));

    // The store primitive reports the refusal directly for both.
    let write = crate::pool_providers::PoolProviderWrite {
        settings: serde_json::from_value(pool_settings(POOL_ID)).unwrap(),
        expected_revision: 0,
        secret: Some(crate::pool_providers::PoolSecretWrite {
            secret_ref: POOL_ID_B.to_string(),
            kek_version: 1,
            envelope: crate::pool_providers::PoolSecretEnvelope {
                dek_wrapped_nonce: [1; 12],
                dek_wrapped: vec![2; 48],
                payload_nonce: [3; 12],
                ciphertext: vec![4; 32],
            },
        }),
    };
    assert!(matches!(
        block_on(
            h.store
                .admin_write_pool_provider("admin1", &write, "198.51.100.9", NOW)
        ),
        Ok(crate::pool_providers::PoolProviderWriteOutcome::ActorNotAdmin)
    ));
    h.store
        .user_roles
        .borrow_mut()
        .insert("admin1".to_string(), crate::admin::ROLE_ADMIN.to_string());
    h.store
        .deleting_users
        .borrow_mut()
        .insert("admin1".to_string());
    assert!(matches!(
        block_on(
            h.store
                .admin_write_pool_provider("admin1", &write, "198.51.100.9", NOW)
        ),
        Ok(crate::pool_providers::PoolProviderWriteOutcome::ActorNotAdmin)
    ));
    assert_eq!(pool_state(&h), (0, 0, 0));
}

#[test]
fn admin_pool_create_stores_envelope_audits_and_redacts() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    let audits_before = h.store.audits.borrow().len();

    let created = pool_create(&h, &admin_cookie, POOL_ID);
    assert_eq!(created.status, 201, "{}", created.body);
    let secret_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();
    assert_no_pool_credentials(&created.body, Some(&secret_ref));
    let body: serde_json::Value = serde_json::from_str(&created.body).unwrap();
    assert_eq!(body["provider"]["id"], POOL_ID);
    assert_eq!(body["provider"]["revision"], 1);
    assert_eq!(body["provider"]["enabled"], true);

    // Envelope: bound to the stored ref and the configured KEK, opens to
    // the submitted password, and fails under a different identity.
    let secrets = h.store.pool_provider_secrets.borrow();
    assert_eq!(secrets.len(), 1);
    let (kek_version, stored) = secrets.get(&secret_ref).expect("envelope by secret_ref");
    assert_eq!(*kek_version, h.config.kek_version);
    let envelope = stored.to_envelope();
    let kek = h.config.kek_by_version(*kek_version).unwrap();
    let opened =
        crate::pool_providers::decrypt_pool_secret(kek, *kek_version, &secret_ref, &envelope)
            .map_err(|_| ())
            .unwrap();
    assert_eq!(opened.password, POOL_PASS_MARKER);
    assert!(
        crate::pool_providers::decrypt_pool_secret(kek, *kek_version, POOL_ID_B, &envelope)
            .is_err()
    );
    drop(secrets);

    // One redacted audit row with the creation action only.
    assert_eq!(
        pool_audit_actions(&h),
        vec!["pool_provider.created".to_string()]
    );
    assert_eq!(h.store.audits.borrow().len(), audits_before + 1);
    let audits = h.store.audits.borrow();
    let row = audits.last().unwrap();
    assert_eq!(row.user_id, "admin1");
    assert_eq!(row.subject_user_id, None);
    let details = row.details.as_deref().unwrap();
    assert_no_pool_credentials(details, Some(&secret_ref));
    let details: serde_json::Value = serde_json::from_str(details).unwrap();
    assert_eq!(details["provider_id"], POOL_ID);
    assert!(details["old"].is_null());
    assert_eq!(details["new"]["revision"], 1);
    drop(audits);

    // List serves the same redacted view.
    let list = h.call_full("GET", POOL_PATH, None, Some(admin_cookie), None);
    assert_eq!(list.status, 200, "{}", list.body);
    assert_no_pool_credentials(&list.body, Some(&secret_ref));
    let list: serde_json::Value = serde_json::from_str(&list.body).unwrap();
    assert_eq!(list["providers"].as_array().unwrap().len(), 1);
    assert_eq!(list["providers"][0]["id"], POOL_ID);
}

#[test]
fn admin_pool_settings_update_keeps_secret_identity_and_audits_changes() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    assert_eq!(pool_create(&h, &admin_cookie, POOL_ID).status, 201);
    let secret_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();

    // Disable + cap change, no password: identity and envelope untouched.
    let mut next = pool_settings(POOL_ID);
    next["enabled"] = serde_json::json!(false);
    next["max_connections"] = serde_json::json!(6);
    next["ingest_limit"] = serde_json::json!(1);
    let updated = pool_put(&h, &admin_cookie, next, 1, None);
    assert_eq!(updated.status, 200, "{}", updated.body);
    assert_no_pool_credentials(&updated.body, Some(&secret_ref));
    let body: serde_json::Value = serde_json::from_str(&updated.body).unwrap();
    assert_eq!(body["provider"]["revision"], 2);
    assert_eq!(body["provider"]["enabled"], false);
    assert_eq!(body["provider"]["max_connections"], 6);
    {
        let rows = h.store.pool_provider_rows.borrow();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].secret_ref, secret_ref);
        assert_eq!(rows[0].revision, 2);
        assert_eq!(h.store.pool_provider_secrets.borrow().len(), 1);
        assert!(h
            .store
            .pool_provider_secrets
            .borrow()
            .contains_key(&secret_ref));
    }
    assert_eq!(
        pool_audit_actions(&h),
        vec![
            "pool_provider.created",
            "pool_provider.updated",
            "pool_provider.disabled",
            "pool_provider.caps_changed",
        ]
    );
    for audit in h.store.audits.borrow().iter() {
        if let Some(details) = audit.details.as_deref() {
            assert_no_pool_credentials(details, Some(&secret_ref));
        }
    }

    // A label-only update records neither disable nor caps.
    let mut relabel = pool_settings(POOL_ID);
    relabel["enabled"] = serde_json::json!(false);
    relabel["max_connections"] = serde_json::json!(6);
    relabel["ingest_limit"] = serde_json::json!(1);
    relabel["label"] = serde_json::json!("Renamed");
    assert_eq!(pool_put(&h, &admin_cookie, relabel, 2, None).status, 200);
    assert_eq!(pool_audit_actions(&h).len(), 5);
    assert_eq!(pool_audit_actions(&h)[4], "pool_provider.updated");
}

#[test]
fn admin_pool_rotation_mints_new_identity_and_retires_old_envelope() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    assert_eq!(pool_create(&h, &admin_cookie, POOL_ID).status, 201);
    let old_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();

    let rotated = pool_put(
        &h,
        &admin_cookie,
        pool_settings(POOL_ID),
        1,
        Some(POOL_PASS_ROTATED),
    );
    assert_eq!(rotated.status, 200, "{}", rotated.body);
    let new_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();
    assert_ne!(new_ref, old_ref);
    assert_no_pool_credentials(&rotated.body, Some(&new_ref));
    assert!(!rotated.body.contains(&old_ref));

    let secrets = h.store.pool_provider_secrets.borrow();
    assert_eq!(secrets.len(), 1, "retired envelope is deleted");
    assert!(!secrets.contains_key(&old_ref));
    let (kek_version, stored) = secrets.get(&new_ref).unwrap();
    let kek = h.config.kek_by_version(*kek_version).unwrap();
    let opened = crate::pool_providers::decrypt_pool_secret(
        kek,
        *kek_version,
        &new_ref,
        &stored.to_envelope(),
    )
    .map_err(|_| ())
    .unwrap();
    assert_eq!(opened.password, POOL_PASS_ROTATED);
    drop(secrets);

    assert_eq!(
        pool_audit_actions(&h),
        vec![
            "pool_provider.created",
            "pool_provider.updated",
            "pool_provider.credentials_rotated",
        ]
    );
    for audit in h.store.audits.borrow().iter() {
        if let Some(details) = audit.details.as_deref() {
            assert_no_pool_credentials(details, Some(&new_ref));
            assert!(!details.contains(&old_ref));
        }
    }
}

#[test]
fn admin_pool_revision_conflicts_leave_no_partial_state() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    assert_eq!(pool_create(&h, &admin_cookie, POOL_ID).status, 201);
    let before = pool_state(&h);
    let old_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();

    // Duplicate create (revision 0 against an existing id), including one
    // that carries a fresh password envelope.
    let dup = pool_create(&h, &admin_cookie, POOL_ID);
    assert_eq!(dup.status, 409, "{}", dup.body);
    assert_eq!(
        dup.body,
        r#"{"code":"conflict","message":"pool provider revision conflict","retryable":false}"#
    );
    // Stale revision with and without a rotating password.
    let mut changed = pool_settings(POOL_ID);
    changed["label"] = serde_json::json!("Changed");
    for password in [None, Some(POOL_PASS_ROTATED)] {
        let stale = pool_put(&h, &admin_cookie, changed.clone(), 7, password);
        assert_eq!(stale.status, 409, "{}", stale.body);
    }
    // PUT to an id that does not exist is a conflict, not an upsert.
    let missing = pool_put(&h, &admin_cookie, pool_settings(POOL_ID_B), 1, None);
    assert_eq!(missing.status, 409, "{}", missing.body);

    assert_eq!(pool_state(&h), before);
    let rows = h.store.pool_provider_rows.borrow();
    assert_eq!(rows[0].secret_ref, old_ref);
    assert_eq!(rows[0].settings.label, "Primary");
    assert_eq!(rows[0].revision, 1);
}

#[test]
fn admin_pool_invalid_requests_are_rejected_without_mutation() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    assert_eq!(pool_create(&h, &admin_cookie, POOL_ID).status, 201);
    let before = pool_state(&h);

    let with = |key: &str, value: serde_json::Value| {
        let mut s = pool_settings(POOL_ID_B);
        s[key] = value;
        s
    };
    let mut unknown_field = pool_settings(POOL_ID_B);
    unknown_field["secret_ref"] = serde_json::json!(POOL_ID);
    let mut missing_field = pool_settings(POOL_ID_B);
    missing_field.as_object_mut().unwrap().remove("username");

    let long_password = "p".repeat(1025);
    let cases: Vec<(&str, serde_json::Value, u64, Option<&str>)> = vec![
        (
            "uppercase id",
            with(
                "id",
                serde_json::json!("ABCDEF01-2222-4222-8222-222222222222"),
            ),
            0,
            Some("pw"),
        ),
        (
            "malformed id",
            with("id", serde_json::json!("not-a-uuid")),
            0,
            Some("pw"),
        ),
        ("unknown settings field", unknown_field, 0, Some("pw")),
        ("missing settings field", missing_field, 0, Some("pw")),
        (
            "tls off",
            with("tls", serde_json::json!(false)),
            0,
            Some("pw"),
        ),
        (
            "bad port",
            with("port", serde_json::json!(80)),
            0,
            Some("pw"),
        ),
        (
            "bad host",
            with("host", serde_json::json!("http://x/")),
            0,
            Some("pw"),
        ),
        (
            "zero connections",
            with("max_connections", serde_json::json!(0)),
            0,
            Some("pw"),
        ),
        (
            "reserve over max",
            with("stream_reserve", serde_json::json!(11)),
            0,
            Some("pw"),
        ),
        (
            "ingest over cap",
            with("ingest_limit", serde_json::json!(9)),
            0,
            Some("pw"),
        ),
        (
            "bad backbone",
            with("backbone", serde_json::json!("a b")),
            0,
            Some("pw"),
        ),
        (
            "tier over max",
            with("tier", serde_json::json!(101)),
            0,
            Some("pw"),
        ),
        (
            "empty username",
            with("username", serde_json::json!("")),
            0,
            Some("pw"),
        ),
        ("create without password", pool_settings(POOL_ID_B), 0, None),
        (
            "create with revision",
            pool_settings(POOL_ID_B),
            1,
            Some("pw"),
        ),
        ("empty password", pool_settings(POOL_ID_B), 0, Some("")),
        (
            "newline password",
            pool_settings(POOL_ID_B),
            0,
            Some("a\nb"),
        ),
        ("nul password", pool_settings(POOL_ID_B), 0, Some("a\0b")),
        (
            "long password",
            pool_settings(POOL_ID_B),
            0,
            Some(&long_password),
        ),
        (
            "huge revision",
            pool_settings(POOL_ID_B),
            i64::MAX as u64,
            Some("pw"),
        ),
    ];
    for (name, settings, revision, password) in cases {
        let resp = pool_call(
            &h,
            "POST",
            POOL_PATH,
            pool_body(settings, revision, password),
            &admin_cookie,
        );
        assert_eq!(resp.status, 422, "{name}: {}", resp.body);
        assert_no_pool_credentials(&resp.body, None);
        assert_eq!(pool_state(&h), before, "{name}");
    }

    // Body-level shape: unknown top-level field, non-JSON, wrong password
    // type, update without a revision.
    for raw in [
        serde_json::json!({"settings": pool_settings(POOL_ID_B), "expected_revision": 0,
            "password": "pw", "enabled": true})
        .to_string(),
        "not json".to_string(),
        serde_json::json!({"settings": pool_settings(POOL_ID_B), "expected_revision": 0,
            "password": 12})
        .to_string(),
        serde_json::json!({"settings": pool_settings(POOL_ID_B)}).to_string(),
    ] {
        let resp = pool_call(&h, "POST", POOL_PATH, raw, &admin_cookie);
        assert_eq!(resp.status, 422, "{}", resp.body);
        assert_eq!(pool_state(&h), before);
    }

    // PUT: update at revision zero, mismatched path/body ids, malformed
    // and uppercase path ids (404, before any body parsing).
    let zero = pool_put(&h, &admin_cookie, pool_settings(POOL_ID), 0, None);
    assert_eq!(zero.status, 422, "{}", zero.body);
    let mismatch = pool_call(
        &h,
        "PUT",
        &format!("{POOL_PATH}/{POOL_ID_B}"),
        pool_body(pool_settings(POOL_ID), 1, None),
        &admin_cookie,
    );
    assert_eq!(mismatch.status, 422, "{}", mismatch.body);
    for bad in [
        "not-a-uuid",
        "ABCDEF01-2222-4222-8222-222222222222",
        "",
        "a/b",
    ] {
        let resp = pool_call(
            &h,
            "PUT",
            &format!("{POOL_PATH}/{bad}"),
            pool_body(pool_settings(POOL_ID), 1, None),
            &admin_cookie,
        );
        assert_eq!(resp.status, 404, "{bad}: {}", resp.body);
    }
    assert_eq!(pool_state(&h), before);
}

#[test]
fn admin_pool_store_refuses_identity_reuse_without_partial_write() {
    let h = harness();
    let admin_cookie = seed_admin(&h, "admin1", "admin@example.com");
    // A write whose store-side validation fails (rotation reusing the
    // current identity) is a store error, not a partial write.
    assert_eq!(pool_create(&h, &admin_cookie, POOL_ID).status, 201);
    let before = pool_state(&h);
    let row = h.store.pool_provider_rows.borrow()[0].clone();
    let write = crate::pool_providers::PoolProviderWrite {
        settings: row.settings.clone(),
        expected_revision: 1,
        secret: {
            let secrets = h.store.pool_provider_secrets.borrow();
            let (kek_version, stored) = secrets.get(&row.secret_ref).unwrap();
            Some(crate::pool_providers::PoolSecretWrite {
                secret_ref: row.secret_ref.clone(),
                kek_version: *kek_version,
                envelope: stored.to_envelope(),
            })
        },
    };
    assert!(block_on(
        h.store
            .admin_write_pool_provider("admin1", &write, "198.51.100.9", NOW)
    )
    .is_err());
    assert_eq!(pool_state(&h), before);
}

#[test]
fn admin_pool_routes_are_session_only_and_not_enumerable_on_preflight() {
    use crate::native_rest::{scope_for_operation, RouteDecision};
    let item = format!("{POOL_PATH}/{POOL_ID}");
    for (method, path) in [
        ("GET", POOL_PATH),
        ("POST", POOL_PATH),
        ("PUT", item.as_str()),
    ] {
        assert_eq!(
            scope_for_operation(method, path),
            RouteDecision::SessionOnly,
            "{method} {path}"
        );
    }
    // Nested or unlisted shapes never become bearer-reachable.
    for (method, path) in [
        ("DELETE", item.as_str()),
        ("PUT", POOL_PATH),
        ("GET", &format!("{item}/x")),
    ] {
        assert!(
            !matches!(scope_for_operation(method, path), RouteDecision::Bearer(_)),
            "{method} {path}"
        );
    }

    let h = harness();
    for path in [POOL_PATH, item.as_str(), "/v1/admin/pool-providers/"] {
        let resp = h.call_full("OPTIONS", path, None, None, Some(ORIGIN));
        assert_eq!(resp.status, 404, "{path}: {}", resp.body);
        assert_eq!(resp.body, NOT_FOUND_BODY, "{path}");
    }
}
