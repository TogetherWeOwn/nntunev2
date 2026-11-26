use crate::{meter_sink::TrackMeterMeta, play_grant::IssuedGrant, playback::LeaseRow};

const MEDIA_PLAY: &str = "private_1234567890abcdefghijkl";
const MEDIA_RANGE: &str = "bytes=2-7";
const MEDIA_TOKEN: &str = include_str!("../../../../fixtures/play-grant/private-enrolled.token");
const MEDIA_EPOCH: u64 = 42;

fn media_relay_fixture() -> (Harness, Request, HashMap<String, String>) {
    let mut h = harness();
    const USER: &str = "member";
    const TRACK: &str = "track";
    h.config = cfg().with_play_grant_secret(b"test only, not a deployed secret".to_vec());
    h.config.private_listening = crate::private_listening::PrivateListening::parse(
        r#"{"owner_user_id":"member","invite_ids":[],"expires_at":220}"#,
    );
    seed_user(&h, USER, "test@example.com");
    let ctx = media_relay_context(&h);
    let req = Request {
        method: "GET".into(),
        path: format!("/v1/plays/media/{TRACK}/tab-1/{MEDIA_PLAY}"),
        cookie: block_on(ctx.start_session(USER)).unwrap().set_cookie,
        origin: None,
        authorization: None,
        stripe_signature: None,
        body: Vec::new(),
    };
    let (claims, _) = block_on(ctx.authenticate(&req, false)).unwrap();
    let lease = LeaseRow {
        session_id: "tab-1".into(),
        track_id: TRACK.into(),
        play_id: MEDIA_PLAY.into(),
        epoch: MEDIA_EPOCH,
        expires_at: 12,
        continuation_deadline: 222,
    };
    h.store
        .playback_leases
        .borrow_mut()
        .insert(USER.into(), lease.clone());
    h.store.meter_meta.borrow_mut().insert(
        (USER.into(), TRACK.into()),
        TrackMeterMeta {
            track_duration_ms: Some(60_000),
            ..Default::default()
        },
    );
    h.store
        .browser_receipt
        .replace(Some(browser_playback_relay::BrowserReceipt {
            user_id: USER.into(),
            session_binding: session_binding(&claims),
            lease,
            artifact_sha256: "e96d0ce34def34abaa429581f626f574ac45129a52459a35dc2d78acacf26bd3"
                .into(),
            duration_ms: 60_000.try_into().unwrap(),
            grant: IssuedGrant {
                token: MEDIA_TOKEN.into(),
                exp_unix: 12,
            },
        }));
    (h, req, relay_headers())
}

fn media_relay_context(h: &Harness) -> Ctx<'_, FakeStore, FakeMailer, FakeDialer, FakeDns> {
    Ctx { now: 10, ..h.ctx() }
}

fn shorten_media_relay_lease(h: &Harness) {
    h.store
        .playback_leases
        .borrow_mut()
        .get_mut("member")
        .unwrap()
        .expires_at = 11;
}

fn relay_headers() -> HashMap<String, String> {
    HashMap::from([
        ("sec-fetch-site".into(), "same-origin".into()),
        ("range".into(), MEDIA_RANGE.into()),
    ])
}

fn expected_media_relay_headers() -> [(&'static str, &'static str); 2] {
    const AUTHORIZATION: &str = concat!(
        "Bearer ",
        include_str!("../../../../fixtures/play-grant/private-enrolled.token")
    );
    [("authorization", AUTHORIZATION), ("range", MEDIA_RANGE)]
}

#[test]
fn browser_issue_selects_only_armed_verified_sessions_and_projects_five_fields() {
    use serde_json::{json, Value};
    let (h, media, _) = media_relay_fixture();
    let ctx = media_relay_context(&h);
    let signed_cookie = media.cookie.as_deref().unwrap();
    let (claims, _) = block_on(ctx.authenticate(&media, false)).unwrap();
    let bind = session_binding(&claims);
    let call = |selector: Option<&str>,
                binding: Option<&str>,
                origin: &str,
                cookie: Option<&str>,
                body: Value| {
        let req = Request {
            method: "POST".into(),
            path: "/v1/plays".into(),
            cookie: cookie.map(str::to_string),
            origin: Some(origin.into()),
            authorization: None,
            stripe_signature: None,
            body: body.to_string().into_bytes(),
        };
        let headers = [
            selector.map(|value| ("x-nntune-browser-playback".into(), value.into())),
            binding.map(|value| (SESSION_BINDING_HEADER.into(), value.into())),
        ]
        .into_iter()
        .flatten()
        .collect();
        block_on(crate::worker_entry::handle_with_transport_headers(
            &ctx, req, &headers,
        ))
    };
    let start = json!({"track_id": "track", "session_id": "tab-1"});
    let browser = |body| call(Some("1"), Some(&bind), ORIGIN, Some(signed_cookie), body);
    assert_eq!(browser(start.clone()).status, 404);
    h.store.browser_playback_armed.set(true);
    assert_eq!(
        call(
            Some("0"),
            Some(&bind),
            ORIGIN,
            Some(signed_cookie),
            start.clone()
        )
        .status,
        404
    );
    assert_eq!(
        call(Some("1"), None, ORIGIN, Some(signed_cookie), start.clone()).status,
        412
    );
    assert_eq!(
        call(Some("1"), Some(&bind), ORIGIN, None, start.clone()).status,
        401
    );
    assert_eq!(
        call(
            Some("1"),
            Some(&bind),
            "https://other.example",
            Some(signed_cookie),
            start.clone()
        )
        .status,
        401
    );
    assert_eq!(h.store.browser_playback_opens.get(), 0);

    assert_eq!(
        browser(json!({"track_id": "track", "session_id": "tab-1", "play_id": null})).status,
        400
    );
    assert_eq!(h.store.browser_playback_opens.get(), 0);
    let response = browser(start.clone());
    assert_eq!(response.status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&response.body).unwrap(),
        json!({
            "sessionBinding": bind,
            "playbackSessionId": "tab-1",
            "trackId": "track",
            "playId": MEDIA_PLAY,
            "durationMs": 60000,
        })
    );
    assert!(response.set_cookie.is_none());
    assert_eq!(
        response.extra_headers,
        [
            ("cache-control".into(), "private, no-store".into()),
            ("x-nntune-playback-ttl".into(), "2".into()),
        ]
    );
    assert_eq!(
        browser(json!({"track_id": "track", "session_id": "tab-1", "play_id": MEDIA_PLAY})).body,
        response.body
    );
    assert_eq!(
        browser(json!({"track_id": "track", "session_id": "tab-1", "play_id": "another-play"}))
            .status,
        404
    );

    // An ordinary caller keeps the legacy grant shape even while the
    // browser adapter is armed.
    h.store
        .catalogue_library
        .borrow_mut()
        .insert("member".into(), vec![unmatched_library_row("track")]);
    let native = call(None, None, ORIGIN, Some(signed_cookie), start);
    assert_eq!(native.status, 200);
    assert!(serde_json::from_str::<Value>(&native.body).unwrap()["grant"].is_string());
    assert_eq!(h.store.browser_playback_opens.get(), 3);
}

fn install_browser_play(
    h: &Harness,
    request: &mut Request,
    play_byte: u8,
    epoch: u64,
) -> (LeaseRow, String) {
    let play = format!(
        "{}{}",
        crate::private_playback::browser::PREFIX,
        crate::b64::encode(&[play_byte; 16])
    );
    let mut receipt = h.store.browser_receipt.take().unwrap();
    receipt.lease.play_id.clone_from(&play);
    receipt.lease.epoch = epoch;
    receipt.grant = crate::private_playback::mint_grant(
        h.config.play_grant_secret.as_deref().unwrap(),
        &receipt.user_id,
        &receipt.lease.track_id,
        &play,
        receipt.lease.epoch,
        receipt.grant.exp_unix,
        &receipt.artifact_sha256,
    )
    .unwrap();
    let lease = receipt.lease.clone();
    h.store
        .playback_leases
        .borrow_mut()
        .insert("member".into(), lease.clone());
    let expected = crate::play_grant::bearer(&receipt.grant.token);
    h.store.browser_receipt.replace(Some(receipt));
    request.path = format!("/v1/plays/media/track/tab-1/{play}");
    (lease, expected)
}

#[test]
fn production_media_relay_and_legacy_routes_fence_the_browser_lease() {
    use crate::worker_entry::{relay_media, CollectedBody};
    use serde_json::json;

    let (h, mut request, headers) = media_relay_fixture();
    let (lease, expected) = install_browser_play(&h, &mut request, 0, MEDIA_EPOCH);
    let play = &lease.play_id;
    h.store.meter_meta.borrow_mut().clear();

    let ctx = media_relay_context(&h);
    let serve = |req: &Request| {
        block_on(relay_media(
            &ctx,
            req,
            &CollectedBody::Complete(Vec::new()),
            &headers,
            async |get| {
                assert_eq!(get.path, "/play/track");
                assert!(get
                    .headers()
                    .eq([("authorization", expected.as_str()), ("range", MEDIA_RANGE)]));
                206
            },
        ))
        .unwrap()
        .unwrap_or_else(|response| response.status)
    };
    assert_eq!(serve(&request), 206);
    request.path = format!("/v1/plays/media/track/other-tab/{play}");
    assert_eq!(serve(&request), 404);
    request.path = format!("/v1/plays/media/track/tab-1/{play}");
    let signed_cookie = request.cookie.clone().unwrap();
    request.cookie = block_on(ctx.start_session("member")).unwrap().set_cookie;
    assert_eq!(serve(&request), 404);
    request.cookie = Some(signed_cookie.clone());

    let body = json!({"track_id": "track", "session_id": "tab-1", "play_id": play});
    h.store
        .catalogue_library
        .borrow_mut()
        .insert("member".into(), vec![unmatched_library_row("track")]);
    for (path, body) in [
        ("/v1/plays", body),
        (
            "/v1/plays/heartbeat",
            json!({"session_id": "tab-1", "play_id": play, "lease_epoch": MEDIA_EPOCH}),
        ),
    ] {
        let req = Request {
            method: "POST".into(),
            path: path.into(),
            cookie: Some(signed_cookie.clone()),
            origin: Some(ORIGIN.into()),
            authorization: None,
            stripe_signature: None,
            body: body.to_string().into_bytes(),
        };
        assert_eq!(block_on(handle(&ctx, &req)).status, 404);
        assert_eq!(h.store.playback_leases.borrow()["member"], lease);
    }
    shorten_media_relay_lease(&h);
    assert_eq!(serve(&request), 404);
}

#[test]
fn private_media_relay_refuses_a_new_range_for_a_replaced_browser_lease() {
    use crate::worker_entry::{relay_media, CollectedBody};

    let (h, mut original, headers) = media_relay_fixture();
    let (original_lease, original_bearer) = install_browser_play(&h, &mut original, 0, MEDIA_EPOCH);
    let original_receipt = h.store.browser_receipt.borrow().clone().unwrap();
    let ctx = media_relay_context(&h);
    let body = CollectedBody::Complete(Vec::new());
    let read = |request: &Request, bearer: &str| {
        block_on(relay_media(&ctx, request, &body, &headers, async |get| {
            assert_eq!(get.path, "/play/track");
            assert!(get
                .headers()
                .eq([("authorization", bearer), ("range", MEDIA_RANGE)]));
            (206, b"sample")
        }))
        .unwrap()
        .unwrap()
    };
    assert_eq!(read(&original, &original_bearer), (206, b"sample"));

    let mut replacement = original.clone();
    let (replacement_lease, replacement_bearer) =
        install_browser_play(&h, &mut replacement, 1, original_lease.epoch + 1);
    assert_eq!(read(&replacement, &replacement_bearer), (206, b"sample"));

    // Keep the signed original receipt available to the relay at the same
    // clock time, while the lease lookup returns the replacement.
    h.store.browser_receipt.replace(Some(original_receipt));
    let result: Option<Result<(), Response>> =
        block_on(relay_media(&ctx, &original, &body, &headers, async |_| {
            panic!("a replaced lease reached Stream")
        }));
    let refusal = result.unwrap().unwrap_err();
    assert_eq!(refusal.status, 404);
    assert_eq!(refusal.body, not_found().body);
    assert!(refusal.extra_headers.is_empty());
    assert_eq!(
        h.store.playback_leases.borrow()["member"],
        replacement_lease
    );
}

#[test]
fn browser_grants_and_media_refuse_a_removed_cohort_member() {
    use crate::worker_entry::{handle_with_transport_headers, relay_media, CollectedBody};
    use serde_json::json;

    let (mut h, mut media, mut media_headers) = media_relay_fixture();
    h.config.private_listening = crate::private_listening::PrivateListening::parse(
        r#"{"owner_user_id":"owner","invite_ids":[1],"expires_at":220}"#,
    );
    let mut invite = InviteRecord::new_issued(
        "synthetic-invite".into(),
        Some("owner".into()),
        crate::auth::INVITE_KIND_ADMIN,
        Some("test@example.com".into()),
        1,
        200,
    );
    invite.id = 1;
    invite.used = true;
    invite.redeemed_by = Some("member".into());
    invite.redeemed_at = Some(2);
    h.store
        .invites
        .borrow_mut()
        .insert(invite.code_hash.clone(), invite);
    h.store.browser_playback_armed.set(true);

    let (lease, bearer) = install_browser_play(&h, &mut media, 0, MEDIA_EPOCH);
    let issue = |h: &Harness, browser: &Request, input: serde_json::Value| {
        let ctx = media_relay_context(h);
        let (claims, _) = block_on(ctx.authenticate(browser, false)).unwrap();
        let headers = HashMap::from([
            ("x-nntune-browser-playback".into(), "1".into()),
            (SESSION_BINDING_HEADER.into(), session_binding(&claims)),
        ]);
        let req = Request {
            method: "POST".into(),
            path: "/v1/plays".into(),
            origin: Some(ORIGIN.into()),
            body: input.to_string().into_bytes(),
            ..browser.clone()
        };
        block_on(handle_with_transport_headers(&ctx, req, &headers))
    };
    let start = json!({"track_id": "track", "session_id": "tab-1"});
    let retry = json!({"track_id": "track", "session_id": "tab-1", "play_id": lease.play_id});
    let before = issue(&h, &media, start.clone());
    assert_eq!(before.status, 200);
    let projection: serde_json::Value = serde_json::from_str(&before.body).unwrap();
    assert_eq!(projection["playId"], lease.play_id);
    assert_eq!(issue(&h, &media, retry.clone()).body, before.body);
    let body = CollectedBody::Complete(Vec::new());
    let first = block_on(relay_media(
        &media_relay_context(&h),
        &media,
        &body,
        &media_headers,
        async |get| {
            assert_eq!(get.path, "/play/track");
            assert!(get
                .headers()
                .eq([("authorization", bearer.as_str()), ("range", MEDIA_RANGE)]));
            206
        },
    ));
    assert_eq!(first.unwrap().ok(), Some(206));

    // Remove only the invite from the configured cohort. The cookie and
    // committed receipt are still live at the same clock time.
    h.config
        .private_listening
        .as_mut()
        .unwrap()
        .invite_ids
        .clear();
    let refused = issue(&h, &media, start);
    assert_eq!(refused.status, 403);
    let refused_retry = issue(&h, &media, retry.clone());
    assert_eq!(refused_retry.status, 403);
    assert_eq!(refused_retry.body, refused.body);
    media_headers.insert("range".into(), "bytes=8-".into());
    let seek = |request: &Request| {
        let result: Option<Result<(), Response>> = block_on(relay_media(
            &media_relay_context(&h),
            request,
            &body,
            &media_headers,
            async |_| panic!("a refused seek reached Stream"),
        ));
        result.unwrap().unwrap_err()
    };
    assert_eq!(seek(&media).status, 403);

    media.cookie = block_on(media_relay_context(&h).start_session("member"))
        .unwrap()
        .set_cookie;
    assert_eq!(issue(&h, &media, retry).status, 403);
    assert_eq!(
        issue(
            &h,
            &media,
            json!({"track_id": "track", "session_id": "new-tab"})
        )
        .status,
        403
    );
    assert_eq!(seek(&media).status, 403);
    assert_eq!(h.store.browser_playback_opens.get(), 2);
    assert_eq!(h.store.playback_leases.borrow()["member"], lease);
}
