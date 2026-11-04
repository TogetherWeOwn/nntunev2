// Internal pool resolver handler tests. Included inside `mod tests` after
// `admin_pool_tests.rs`, so it reuses that file's POOL_* constants and
// `pool_create` / `pool_put` / `pool_settings` seeding helpers. Synthetic
// values only.

const PR_SECRET: &[u8] = b"synthetic-pool-resolver-key-32-bytes!!";
const PR_REGISTRY: &str = crate::pool_resolver::REGISTRY_PATH;
const PR_RESOLVE: &str = crate::pool_resolver::RESOLVE_PATH;

/// DNS double that counts lookups, so refusals can prove no resolution ran.
struct PrCountingDns {
    inner: FakeDns,
    calls: std::cell::Cell<usize>,
}

impl AsyncDns for PrCountingDns {
    fn resolve<'a>(&'a self, host: &'a str) -> Pin<Box<dyn Future<Output = Vec<IpAddr>> + 'a>> {
        self.calls.set(self.calls.get() + 1);
        self.inner.resolve(host)
    }
}

fn pr_harness() -> Harness {
    let mut h = harness();
    h.config = cfg().with_pool_resolver_secret(PR_SECRET.to_vec());
    h
}

/// Harness plus one seeded enabled read account; returns `(h, cookie, secret_ref)`.
fn pr_seeded() -> (Harness, String, String) {
    let h = pr_harness();
    let cookie = seed_admin(&h, "admin1", "admin@example.com");
    assert_eq!(pool_create(&h, &cookie, POOL_ID).status, 201);
    let secret_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();
    (h, cookie, secret_ref)
}

fn pr_resolve_body(id: &str, revision: u64, secret_ref: &str, class: &str) -> String {
    serde_json::json!({
        "provider_id": id, "revision": revision, "secret_ref": secret_ref, "class": class,
    })
    .to_string()
}

fn pr_request(path: &str, authorization: Option<String>, body: &str) -> Request {
    Request {
        method: "POST".to_string(),
        path: path.to_string(),
        cookie: None,
        origin: None,
        authorization,
        stripe_signature: None,
        body: body.as_bytes().to_vec(),
    }
}

fn pr_signed(path: &str, body: &str) -> Request {
    let header =
        crate::pool_resolver::authorization_header(Some(PR_SECRET), path, NOW, body.as_bytes());
    pr_request(path, header, body)
}

fn pr_send(h: &Harness, req: &Request) -> Response {
    block_on(handle(&h.ctx(), req))
}

fn pr_call(h: &Harness, path: &str, body: &str) -> Response {
    pr_send(h, &pr_signed(path, body))
}

fn pr_resolve(h: &Harness, id: &str, revision: u64, secret_ref: &str, class: &str) -> Response {
    pr_call(
        h,
        PR_RESOLVE,
        &pr_resolve_body(id, revision, secret_ref, class),
    )
}

fn pr_no_store(resp: &Response) -> bool {
    resp.extra_headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("cache-control") && v == "no-store")
}

fn pr_assert_generic_503(resp: &Response, generic: &str) {
    assert_eq!(resp.status, 503, "{}", resp.body);
    assert_eq!(resp.body, generic);
    assert!(pr_no_store(resp));
    for needle in [
        POOL_USER_MARKER,
        POOL_PASS_MARKER,
        POOL_PASS_ROTATED,
        "peer",
        "password",
        "username",
        "secret_ref",
    ] {
        assert!(!resp.body.contains(needle), "{needle} leaked: {}", resp.body);
    }
}

/// The wire every denial must share, taken from a missing account.
fn pr_generic(h: &Harness) -> String {
    let resp = pr_resolve(h, POOL_ID_B, 1, POOL_ID_B, "stream");
    assert_eq!(resp.status, 503, "{}", resp.body);
    resp.body
}

#[test]
fn pool_resolver_registry_projection_is_scheduling_only() {
    let (h, cookie, secret_ref) = pr_seeded();
    let mut second = pool_settings(POOL_ID_B);
    second["enabled"] = serde_json::json!(false);
    assert_eq!(
        pool_call(
            &h,
            "POST",
            POOL_PATH,
            pool_body(second, 0, Some(POOL_PASS_MARKER)),
            &cookie
        )
        .status,
        201
    );

    let resp = pr_call(&h, PR_REGISTRY, "{}");
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert!(pr_no_store(&resp));
    for leaked in [
        POOL_USER_MARKER,
        POOL_PASS_MARKER,
        "news.example.com",
        "host",
        "port",
        "username",
        "password",
        "envelope",
        "ciphertext",
        "label",
    ] {
        assert!(!resp.body.contains(leaked), "{leaked} leaked: {}", resp.body);
    }
    let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2, "disabled rows stay visible to schedulers");
    let first = accounts.iter().find(|a| a["id"] == POOL_ID).unwrap();
    assert_eq!(first["secret_ref"], secret_ref);
    assert_eq!(first["revision"], 1);
    assert_eq!(first["can_read"], true);
    assert_eq!(first["enabled"], true);
    assert_eq!(first["max_connections"], 10);
    assert_eq!(first["stream_reserve"], 2);
    assert_eq!(first["ingest_limit"], 4);
    assert_eq!(first["tier"], 1);
    let off = accounts.iter().find(|a| a["id"] == POOL_ID_B).unwrap();
    assert_eq!(off["enabled"], false);

    for malformed in [r#"{"x":1}"#, "[]", "null", "{", ""] {
        let extra = pr_call(&h, PR_REGISTRY, malformed);
        assert_eq!(extra.status, 422, "{}", extra.body);
    }
}

#[test]
fn pool_resolver_signed_resolve_returns_exact_dial_material() {
    let (h, _cookie, secret_ref) = pr_seeded();
    for class in ["stream", "ingest"] {
        let resp = pr_resolve(&h, POOL_ID, 1, &secret_ref, class);
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert!(pr_no_store(&resp));
        let body: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
        assert_eq!(body["provider_id"], POOL_ID);
        assert_eq!(body["revision"], 1);
        assert_eq!(body["secret_ref"], secret_ref);
        assert_eq!(body["host"], "news.example.com");
        assert_eq!(body["port"], 563);
        assert_eq!(body["tls"], true);
        assert_eq!(body["peer"], "93.184.216.34");
        assert_eq!(body["username"], POOL_USER_MARKER);
        assert_eq!(body["password"], POOL_PASS_MARKER);
        assert_eq!(
            body["expires_at"],
            NOW + crate::pool_resolver::DIAL_MATERIAL_TTL_SECS
        );
    }
}

#[test]
fn pool_resolver_all_ineligible_states_share_one_generic_503() {
    let (h, cookie, secret_ref) = pr_seeded();
    let generic = pr_generic(&h);
    assert!(generic.contains("service unavailable"));

    // Missing account and wrong secret_ref / revision.
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID_B, 1, &secret_ref, "stream"), &generic);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, POOL_ID_B, "stream"), &generic);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 2, &secret_ref, "stream"), &generic);

    // Class-limited: ingest refused, stream still served.
    let mut limited = pool_settings(POOL_ID);
    limited["ingest_limit"] = serde_json::json!(0);
    assert_eq!(pool_put(&h, &cookie, limited.clone(), 1, None).status, 200);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 2, &secret_ref, "ingest"), &generic);
    assert_eq!(pr_resolve(&h, POOL_ID, 2, &secret_ref, "stream").status, 200);
    // Stale revision after that update.
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);

    // Non-read account.
    let mut no_read = limited.clone();
    no_read["can_read"] = serde_json::json!(false);
    assert_eq!(pool_put(&h, &cookie, no_read, 2, None).status, 200);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 3, &secret_ref, "stream"), &generic);

    // Disabled account.
    let mut disabled = pool_settings(POOL_ID);
    disabled["enabled"] = serde_json::json!(false);
    assert_eq!(pool_put(&h, &cookie, disabled, 3, None).status, 200);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 4, &secret_ref, "stream"), &generic);

    // Re-enable with a rotated secret: the old identity is dead, the new
    // identity resolves to the rotated password only.
    assert_eq!(
        pool_put(
            &h,
            &cookie,
            pool_settings(POOL_ID),
            4,
            Some(POOL_PASS_ROTATED)
        )
        .status,
        200
    );
    let new_ref = h.store.pool_provider_rows.borrow()[0].secret_ref.clone();
    assert_ne!(new_ref, secret_ref);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 5, &secret_ref, "stream"), &generic);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 4, &new_ref, "stream"), &generic);
    let ok = pr_resolve(&h, POOL_ID, 5, &new_ref, "stream");
    assert_eq!(ok.status, 200, "{}", ok.body);
    let body: serde_json::Value = serde_json::from_str(&ok.body).unwrap();
    assert_eq!(body["password"], POOL_PASS_ROTATED);
}

#[test]
fn pool_resolver_refuses_every_other_auth_without_store_or_dns() {
    let (h, cookie, secret_ref) = pr_seeded();
    h.store.pool_reads.set(0);
    let counting = PrCountingDns {
        inner: FakeDns {
            map: h.dns.map.clone(),
        },
        calls: std::cell::Cell::new(0),
    };
    let body = pr_resolve_body(POOL_ID, 1, &secret_ref, "stream");
    let good = pr_signed(PR_RESOLVE, &body);
    let header = good.authorization.clone().unwrap();
    let meter =
        crate::meter_sink::authorization_header(Some(PR_SECRET), NOW, body.as_bytes()).unwrap();
    let registry_header = crate::pool_resolver::authorization_header(
        Some(PR_SECRET),
        PR_REGISTRY,
        NOW,
        body.as_bytes(),
    );
    // Registry path signed header replayed on resolve: wrong-path.
    assert!(registry_header.is_some());
    let forged = {
        let mut f = header.clone();
        f.pop();
        f.push(if header.ends_with('0') { '1' } else { '0' });
        f
    };
    let stale = crate::pool_resolver::authorization_header(
        Some(PR_SECRET),
        PR_RESOLVE,
        NOW - crate::pool_resolver::MAX_CLOCK_SKEW_SECS - 1,
        body.as_bytes(),
    );

    let mut cases: Vec<(&str, Request)> = vec![
        ("absent", pr_request(PR_RESOLVE, None, &body)),
        ("forged", pr_request(PR_RESOLVE, Some(forged), &body)),
        ("stale", pr_request(PR_RESOLVE, stale, &body)),
        ("wrong path", pr_request(PR_RESOLVE, registry_header, &body)),
        ("meter", pr_request(PR_RESOLVE, Some(meter.clone()), &body)),
        (
            "meter renamed",
            pr_request(
                PR_RESOLVE,
                Some(meter.replacen("MeterSink ", "PoolResolver ", 1)),
                &body,
            ),
        ),
        (
            "bearer",
            pr_request(PR_RESOLVE, Some("Bearer ntk_synthetic".into()), &body),
        ),
        (
            "tampered body",
            pr_request(PR_RESOLVE, Some(header.clone()), &body.replace("stream", "ingest")),
        ),
    ];
    let mut session = good.clone();
    session.cookie = Some(cookie.clone());
    cases.push(("session cookie", session));
    let mut origin = good.clone();
    origin.origin = Some(ORIGIN.to_string());
    cases.push(("browser origin", origin));
    let mut foreign = good.clone();
    foreign.origin = Some("https://evil.example".to_string());
    cases.push(("foreign origin", foreign));

    let state_before = pool_state(&h);
    for (name, req) in &cases {
        let ctx = Ctx {
            cfg: &h.config,
            store: &h.store,
            mailer: &h.mailer,
            dialer: &h.dialer,
            dns: &counting,
            stripe: &h.stripe,
            now: NOW,
            source_ip: "198.51.100.9".to_string(),
        };
        let resp = block_on(handle(&ctx, req));
        assert_eq!(resp.status, 404, "{name}: {}", resp.body);
        assert_eq!(resp.body, NOT_FOUND_BODY, "{name}");
        for needle in [POOL_USER_MARKER, POOL_PASS_MARKER, "peer"] {
            assert!(!resp.body.contains(needle), "{name}: {}", resp.body);
        }
    }
    assert_eq!(h.store.pool_reads.get(), 0, "refusals must not read registry or secrets");
    assert_eq!(counting.calls.get(), 0, "refusals must not resolve DNS");
    assert_eq!(pool_state(&h), state_before);

    // Sanity: the unmodified signed request still succeeds on the same ctx.
    let ctx = Ctx {
        cfg: &h.config,
        store: &h.store,
        mailer: &h.mailer,
        dialer: &h.dialer,
        dns: &counting,
        stripe: &h.stripe,
        now: NOW,
        source_ip: "198.51.100.9".to_string(),
    };
    assert_eq!(block_on(handle(&ctx, &good)).status, 200);
    assert_eq!(counting.calls.get(), 1);

    // The registry path is equally closed to session/bearer/meter auth.
    let reg_meter = crate::meter_sink::authorization_header(Some(PR_SECRET), NOW, b"{}").unwrap();
    for auth in [None, Some(reg_meter), Some("Bearer ntk_synthetic".to_string())] {
        let resp = pr_send(&h, &pr_request(PR_REGISTRY, auth, "{}"));
        assert_eq!(resp.status, 404, "{}", resp.body);
    }
}

#[test]
fn pool_resolver_has_no_get_or_browser_preflight_surface() {
    let h = pr_harness();
    for path in [PR_REGISTRY, PR_RESOLVE] {
        assert!(!is_known_path(path));
        for method in ["GET", "PUT", "OPTIONS"] {
            let mut req = pr_signed(path, "{}");
            req.method = method.into();
            let response = pr_send(&h, &req);
            assert_eq!(response.status, 404);
            assert_eq!(response.body, NOT_FOUND_BODY);
        }
    }
    assert_eq!(h.store.pool_reads.get(), 0);
}

#[test]
fn pool_resolver_unknown_fields_and_malformed_bodies_are_422() {
    let (h, _cookie, secret_ref) = pr_seeded();
    let good = serde_json::json!({
        "provider_id": POOL_ID, "revision": 1, "secret_ref": secret_ref, "class": "stream",
    });
    let mut extra = good.clone();
    extra["endpoint"] = serde_json::json!("x");
    let mut upper = good.clone();
    upper["provider_id"] = serde_json::json!("AAAAAAAA-1111-4111-8111-111111111111");
    let mut zero = good.clone();
    zero["revision"] = serde_json::json!(0);
    let mut negative = good.clone();
    negative["revision"] = serde_json::json!(-1);
    let mut huge = good.clone();
    huge["revision"] = serde_json::json!(u64::MAX);
    let mut class = good.clone();
    class["class"] = serde_json::json!("post");
    let mut bad_ref = good.clone();
    bad_ref["secret_ref"] = serde_json::json!("not-a-uuid");
    let mut missing = good.clone();
    missing.as_object_mut().unwrap().remove("class");

    for body in [
        extra.to_string(),
        upper.to_string(),
        zero.to_string(),
        negative.to_string(),
        huge.to_string(),
        class.to_string(),
        bad_ref.to_string(),
        missing.to_string(),
        "{".to_string(),
        "[]".to_string(),
        "null".to_string(),
        String::new(),
    ] {
        let resp = pr_call(&h, PR_RESOLVE, &body);
        assert_eq!(resp.status, 422, "{body}: {}", resp.body);
        assert!(pr_no_store(&resp));
        assert!(!resp.body.contains(POOL_PASS_MARKER));
    }
}

#[test]
fn pool_resolver_missing_or_short_key_fails_closed() {
    let (mut h, _cookie, secret_ref) = pr_seeded();
    let body = pr_resolve_body(POOL_ID, 1, &secret_ref, "stream");
    // A header validly signed under the real key must not work once the
    // deployment has no (or a too-short) resolver key.
    let signed_resolve = pr_signed(PR_RESOLVE, &body);
    let signed_registry = pr_signed(PR_REGISTRY, "{}");
    let unsigned = pr_request(PR_RESOLVE, None, &body);

    let mut configs = vec![cfg(), cfg().with_pool_resolver_secret(b"short".to_vec())];
    configs.push(cfg().with_pool_resolver_secret(PR_SECRET[..31].to_vec()));
    for config in configs {
        h.config = config;
        for req in [&signed_resolve, &signed_registry, &unsigned] {
            let resp = pr_send(&h, req);
            assert_eq!(resp.status, 500, "{}", resp.body);
            for needle in [POOL_USER_MARKER, POOL_PASS_MARKER, "peer", "accounts"] {
                assert!(!resp.body.contains(needle), "{}", resp.body);
            }
        }
    }
    // Short keys also cannot sign at all.
    assert!(crate::pool_resolver::authorization_header(
        Some(&PR_SECRET[..31]),
        PR_RESOLVE,
        NOW,
        body.as_bytes()
    )
    .is_none());
}

#[test]
fn pool_resolver_crypto_state_failures_are_generic_503() {
    let (h, _cookie, secret_ref) = pr_seeded();
    let generic = pr_generic(&h);

    // Unknown stored KEK version.
    let original_version = h.store.pool_provider_secrets.borrow()[&secret_ref].0;
    h.store
        .pool_provider_secrets
        .borrow_mut()
        .get_mut(&secret_ref)
        .unwrap()
        .0 = 4242;
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);
    h.store
        .pool_provider_secrets
        .borrow_mut()
        .get_mut(&secret_ref)
        .unwrap()
        .0 = original_version;
    assert_eq!(pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream").status, 200);

    // Corrupted ciphertext / wrapped DEK / nonce.
    for field in 0..3 {
        let backup = {
            let secrets = h.store.pool_provider_secrets.borrow();
            let e = &secrets[&secret_ref].1;
            (
                e.dek_wrapped_nonce,
                e.dek_wrapped.clone(),
                e.payload_nonce,
                e.ciphertext.clone(),
            )
        };
        {
            let mut secrets = h.store.pool_provider_secrets.borrow_mut();
            let e = &mut secrets.get_mut(&secret_ref).unwrap().1;
            match field {
                0 => e.ciphertext[0] ^= 0x01,
                1 => e.dek_wrapped[0] ^= 0x01,
                _ => e.payload_nonce[0] ^= 0x01,
            }
        }
        pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);
        let mut secrets = h.store.pool_provider_secrets.borrow_mut();
        let e = &mut secrets.get_mut(&secret_ref).unwrap().1;
        e.dek_wrapped_nonce = backup.0;
        e.dek_wrapped = backup.1;
        e.payload_nonce = backup.2;
        e.ciphertext = backup.3;
    }
    // Truncated envelope.
    h.store
        .pool_provider_secrets
        .borrow_mut()
        .get_mut(&secret_ref)
        .unwrap()
        .1
        .ciphertext
        .truncate(1);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);
}

#[test]
fn pool_resolver_envelope_substitution_fails() {
    let (h, _cookie, secret_ref) = pr_seeded();
    let generic = pr_generic(&h);
    let kek_version = h.config.kek_version;
    let kek = *h.config.kek_by_version(kek_version).unwrap();
    let swap = |envelope: crate::providers::ProviderCredsEnvelope| {
        h.store.pool_provider_secrets.borrow_mut().insert(
            secret_ref.clone(),
            (
                kek_version,
                StoredPoolEnvelope {
                    dek_wrapped_nonce: envelope.dek_wrapped_nonce,
                    dek_wrapped: envelope.dek_wrapped.clone(),
                    payload_nonce: envelope.payload_nonce,
                    ciphertext: envelope.ciphertext.clone(),
                },
            ),
        );
    };
    let plaintext = serde_json::json!({"password": POOL_PASS_ROTATED}).to_string();

    // Legacy-provider-table v2 envelope for a user whose id equals this
    // secret_ref, and for another user: the pool AAD table/scope differ.
    for user in [secret_ref.as_str(), "other-user"] {
        let foreign = crate::providers::encrypt_provider_creds(
            &kek,
            kek_version,
            user,
            &secret_ref,
            plaintext.as_bytes(),
        )
        .map_err(|_| ())
        .unwrap();
        swap(foreign);
        pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);
    }

    // A genuine pool envelope sealed for a different secret_ref.
    let other = crate::pool_providers::encrypt_pool_secret(
        &kek,
        kek_version,
        POOL_ID_B,
        &crate::pool_providers::PoolProviderSecret {
            password: POOL_PASS_ROTATED.to_string(),
        },
    )
    .map_err(|_| ())
    .unwrap();
    swap(other);
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);

    // The same envelope under the right identity does open (control).
    let right = crate::pool_providers::encrypt_pool_secret(
        &kek,
        kek_version,
        &secret_ref,
        &crate::pool_providers::PoolProviderSecret {
            password: POOL_PASS_ROTATED.to_string(),
        },
    )
    .map_err(|_| ())
    .unwrap();
    swap(right);
    let ok = pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream");
    assert_eq!(ok.status, 200, "{}", ok.body);
}

#[test]
fn pool_resolver_dns_must_be_entirely_public() {
    let (mut h, _cookie, secret_ref) = pr_seeded();
    let generic = pr_generic(&h);
    let public: Vec<IpAddr> = vec![
        "93.184.216.34".parse().unwrap(),
        "93.184.216.35".parse().unwrap(),
        "93.184.216.36".parse().unwrap(),
        "93.184.216.37".parse().unwrap(),
    ];
    let with = |extra: IpAddr| {
        let mut v = public.clone();
        v.push(extra);
        v
    };
    let cases: Vec<(&str, Vec<IpAddr>)> = vec![
        ("empty", Vec::new()),
        (
            "mixed public+private",
            vec![[93, 184, 216, 34].into(), [10, 0, 0, 7].into()],
        ),
        ("late private", with([192, 168, 1, 9].into())),
        ("late loopback", with([127, 0, 0, 1].into())),
        ("late v6 loopback", with(std::net::Ipv6Addr::LOCALHOST.into())),
        ("late link-local", with([169, 254, 169, 254].into())),
        (
            "late metadata-mapped v6",
            with(std::net::Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped().into()),
        ),
    ];
    for (name, addrs) in cases {
        h.dns
            .map
            .insert("news.example.com".to_string(), addrs.clone());
        let resp = pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream");
        assert_eq!(resp.status, 503, "{name}: {}", resp.body);
        pr_assert_generic_503(&resp, &generic);
    }
    // Missing host record (not just an empty answer) is the same denial.
    h.dns.map.remove("news.example.com");
    pr_assert_generic_503(&pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream"), &generic);
    // Control: an all-public answer is served, with the first address pinned.
    h.dns.map.insert("news.example.com".to_string(), public);
    let ok = pr_resolve(&h, POOL_ID, 1, &secret_ref, "stream");
    assert_eq!(ok.status, 200, "{}", ok.body);
    let body: serde_json::Value = serde_json::from_str(&ok.body).unwrap();
    assert_eq!(body["peer"], "93.184.216.34");
}
