// Native handler tests for the per-account preference slice:
// `/v1/me/likes*`, `/v1/me/taste`, `/v1/me/history` and
// `/v1/me/private-session`. Included verbatim inside `handlers.rs`
// `mod tests` (`include!`), so it shares that module's fixtures
// (`harness`, `play_harness`, `seed_user`, `magic_login`, `FakeStore`).
//
// Expectations pinned here deliberately follow the intended contract, not
// incidental behaviour: MBID targets are canonicalized to lowercase before
// storage and echo, `DELETE` of a non-member target is the 404, and a
// hostile import id must be rejected without a panic.

/// A recording MBID with letters so case folding is observable.
const TASTE_REC: &str = "abcdef01-2222-3333-4444-555555555555";

fn taste_user(h: &Harness, id: &str) -> String {
    let email = format!("{id}@example.com");
    seed_user(h, id, &email);
    magic_login(h, &email)
}

/// Own ledger fixture: job `job-t1` (release `CAT_RELEASE`, recording
/// `TASTE_REC`) and the unmatched upload `job-t2`.
fn taste_library(h: &Harness, user: &str) {
    let mut matched = matched_library_row("job-t1", TASTE_REC, 1);
    matched.recording_mbids = vec![TASTE_REC.to_string()];
    h.store.catalogue_library.borrow_mut().insert(
        user.to_string(),
        vec![matched, unmatched_library_row("job-t2")],
    );
}

fn taste_call(
    h: &Harness,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<serde_json::Value>,
) -> Response {
    h.call_full(
        method,
        path,
        body.map(|b| b.to_string()),
        Some(cookie.to_string()),
        Some(ORIGIN),
    )
}

fn taste_json(resp: &Response) -> serde_json::Value {
    serde_json::from_str(&resp.body).unwrap_or_else(|_| panic!("json body: {}", resp.body))
}

fn taste_code(resp: &Response) -> String {
    taste_json(resp)["code"].as_str().unwrap_or("").to_string()
}

fn taste_row(
    h: &Harness,
    user: &str,
    kind: &str,
    target: &str,
) -> Option<crate::taste::TasteDbRow> {
    h.store
        .taste_rows
        .borrow()
        .get(&(user.to_string(), kind.to_string(), target.to_string()))
        .cloned()
}

fn taste_rows_for(h: &Harness, user: &str) -> usize {
    h.store
        .taste_rows
        .borrow()
        .keys()
        .filter(|(u, _, _)| u == user)
        .count()
}

fn taste_seed_row(
    h: &Harness,
    user: &str,
    kind: &str,
    target: &str,
    signal: &str,
    snooze_until: Option<i64>,
    created_at: i64,
) {
    h.store.taste_rows.borrow_mut().insert(
        (user.to_string(), kind.to_string(), target.to_string()),
        crate::taste::TasteDbRow {
            kind: kind.to_string(),
            target_id: target.to_string(),
            signal: signal.to_string(),
            snooze_until,
            created_at,
            updated_at: created_at,
        },
    );
}

fn taste_items(resp: &Response) -> Vec<serde_json::Value> {
    assert_eq!(resp.status, 200, "{}", resp.body);
    taste_json(resp)["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn taste_put_signal(
    h: &Harness,
    cookie: &str,
    kind: &str,
    target: &str,
    signal: &str,
    snooze_seconds: Option<u64>,
) -> Response {
    let mut body = serde_json::json!({"kind": kind, "target_id": target, "signal": signal});
    if let Some(secs) = snooze_seconds {
        body["snooze_seconds"] = serde_json::json!(secs);
    }
    taste_call(h, "PUT", "/v1/me/taste", cookie, Some(body))
}

fn taste_window(
    user: &str,
    track: &str,
    play: &str,
    bytes: u64,
    seconds: u64,
    at: u64,
) -> crate::ledger::PlayWindow {
    crate::ledger::PlayWindow {
        idempotency_key: format!("play:{user}:{track}:{play}"),
        user_id: user.to_string(),
        track_id: track.to_string(),
        period_month: crate::ledger::period_month_for_timestamp(NOW),
        stream_seconds: seconds,
        bytes_served: bytes,
        file_bytes: None,
        duration_ms: None,
        rate_fallback: false,
        recorded_at: at,
    }
}

/// Issue a play grant; returns the minted `play_id`.
fn taste_issue_play(h: &Harness, cookie: &str, session: &str, track: &str) -> String {
    let resp = play_call(
        h,
        cookie,
        serde_json::json!({"track_id": track, "session_id": session, "takeover": true}),
    );
    assert_eq!(resp.status, 200, "{}", resp.body);
    taste_json(&resp)["play_id"].as_str().unwrap().to_string()
}

fn taste_private_session(h: &Harness, cookie: &str, session: &str, enabled: bool) -> Response {
    taste_call(
        h,
        "PUT",
        "/v1/me/private-session",
        cookie,
        Some(serde_json::json!({"session_id": session, "enabled": enabled})),
    )
}

fn taste_history(h: &Harness, cookie: &str) -> Vec<serde_json::Value> {
    taste_items(&taste_call(h, "GET", "/v1/me/history", cookie, None))
}

// --- Durable likes ---------------------------------------------------------------------------

#[test]
fn taste_likes_put_is_canonical_idempotent_and_first_created_at_wins() {
    let h = harness();
    let cookie = taste_user(&h, "tl-1");
    taste_library(&h, "tl-1");
    // A pre-existing like keeps its original `created_at` through a replay.
    taste_seed_row(&h, "tl-1", "track", TASTE_REC, "love", None, 1_000);
    let upper = TASTE_REC.to_uppercase();
    for _ in 0..2 {
        let resp = taste_call(
            &h,
            "PUT",
            &format!("/v1/me/likes/track/{upper}"),
            &cookie,
            None,
        );
        assert_eq!(resp.status, 200, "{}", resp.body);
        let body = taste_json(&resp);
        assert_eq!(body["target_id"], TASTE_REC, "canonical lowercase echo");
        assert_eq!(body["liked_at"], 1_000, "replay keeps the first created_at");
    }
    assert_eq!(taste_rows_for(&h, "tl-1"), 1, "one row, no uppercase twin");
    assert!(taste_row(&h, "tl-1", "track", TASTE_REC).is_some());
    assert!(taste_row(&h, "tl-1", "track", &upper).is_none());
    // An unmatched upload is likable by its job id (exact match).
    let resp = taste_call(&h, "PUT", "/v1/me/likes/track/job-t2", &cookie, None);
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(taste_json(&resp)["liked_at"], NOW);
    let listed = taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None));
    assert_eq!(listed.len(), 2);
    // Newest first: the fresh job-id like precedes the older MBID like.
    assert_eq!(listed[0]["target_id"], "job-t2");
    assert_eq!(listed[1]["target_id"], TASTE_REC);
    assert_eq!(listed[1]["kind"], "track");
    assert_eq!(listed[1]["created_at"], 1_000);
}

#[test]
fn taste_like_after_non_love_stamps_the_current_write_time_not_the_old_signal_time() {
    let h = harness();
    let cookie = taste_user(&h, "tl-2");
    taste_library(&h, "tl-2");
    let now = i64::try_from(NOW).unwrap();
    // A prior hide, an expired snooze and a live snooze all carry their own
    // old timestamps; none may leak out as `liked_at`.
    taste_seed_row(&h, "tl-2", "track", "job-t1", "hide", None, 500);
    taste_seed_row(&h, "tl-2", "track", "job-t2", "snooze", Some(now - 10), 600);
    taste_seed_row(
        &h,
        "tl-2",
        "track",
        TASTE_REC,
        "snooze",
        Some(now + 999),
        700,
    );
    for target in ["job-t1", "job-t2", TASTE_REC] {
        let resp = taste_call(
            &h,
            "PUT",
            &format!("/v1/me/likes/track/{target}"),
            &cookie,
            None,
        );
        assert_eq!(resp.status, 200, "{target}: {}", resp.body);
        assert_eq!(taste_json(&resp)["liked_at"], now, "{target}");
        let stored = taste_row(&h, "tl-2", "track", target).unwrap();
        assert_eq!((stored.signal.as_str(), stored.created_at), ("love", now));
        assert_eq!(stored.snooze_until, None);
    }
}

#[test]
fn taste_repeated_love_keeps_created_at_through_both_write_routes() {
    let h = harness();
    let cookie = taste_user(&h, "tl-3");
    taste_library(&h, "tl-3");
    taste_seed_row(&h, "tl-3", "track", "job-t1", "love", None, 1_000);
    for _ in 0..2 {
        let resp = taste_call(&h, "PUT", "/v1/me/likes/track/job-t1", &cookie, None);
        assert_eq!(taste_json(&resp)["liked_at"], 1_000);
        let resp = taste_put_signal(&h, &cookie, "track", "job-t1", "love", None);
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(
            taste_row(&h, "tl-3", "track", "job-t1").unwrap().created_at,
            1_000
        );
    }
}

#[test]
fn taste_fake_store_upsert_created_at_matches_the_production_sql_rule() {
    use crate::taste::{TargetKind::Track, TasteSignal as S};
    let h = harness();
    let cookie = taste_user(&h, "tl-4");
    taste_library(&h, "tl-4");
    let put = |signal: S, until: Option<u64>, now: u64| {
        assert!(block_on(
            h.store
                .taste_upsert("tl-4", Track, "job-t1", signal, until, now)
        )
        .unwrap());
        taste_row(&h, "tl-4", "track", "job-t1").unwrap()
    };
    // (signal, snooze_until, write time, expected created_at)
    let steps = [
        (S::Ban, None, 10, 10),
        (S::Love, None, 20, 20), // ban -> love: write time
        (S::Dislike, None, 30, 20),
        (S::Love, None, 40, 40), // dislike -> love: write time
        (S::Hide, None, 100, 40),
        (S::Love, None, 200, 200), // non-love -> love: write time
        (S::Love, None, 300, 200), // same-love replay: first wins
        (S::Hide, None, 400, 200), // love -> other keeps the stamp
        (S::Snooze, Some(450), 410, 200),
        (S::Love, None, 500, 500), // expired snooze -> love: write time
        (S::Love, None, 600, 500),
    ];
    for (signal, until, now, created) in steps {
        let row = put(signal, until, now);
        assert_eq!(
            (row.created_at, row.updated_at),
            (created, i64::try_from(now).unwrap())
        );
    }
    // job-t2 was loved at 450, between the initial love (200) and the
    // converted love (500): job-t1 sorts first only if it was restamped.
    taste_seed_row(&h, "tl-4", "track", "job-t2", "love", None, 450);
    let listed = taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None));
    assert_eq!(listed[0]["target_id"], "job-t1");
    assert_eq!(listed[0]["created_at"], 500);
    assert_eq!(listed[1]["target_id"], "job-t2");
    assert_eq!(listed[1]["created_at"], 450);
    // The fake rule above is the production SQL rule.
    assert!(crate::taste::taste_upsert_statement().contains(
        "created_at = CASE WHEN EXCLUDED.signal = 'love' AND account_taste.signal <> 'love' THEN EXCLUDED.created_at ELSE account_taste.created_at END"
    ));
}

#[test]
fn taste_unlike_is_idempotent_and_keeps_suppression_signals() {
    let h = harness();
    let cookie = taste_user(&h, "tu-1");
    taste_library(&h, "tu-1");
    let path = format!("/v1/me/likes/track/{TASTE_REC}");
    assert_eq!(taste_call(&h, "PUT", &path, &cookie, None).status, 200);
    // Unlike twice (also via the uppercase spelling): always 200, row gone.
    let upper = format!("/v1/me/likes/track/{}", TASTE_REC.to_uppercase());
    for spelling in [path.clone(), upper] {
        let resp = taste_call(&h, "DELETE", &spelling, &cookie, None);
        assert_eq!(resp.status, 200, "{}", resp.body);
        assert_eq!(taste_json(&resp)["liked"], false);
    }
    assert!(taste_row(&h, "tu-1", "track", TASTE_REC).is_none());
    assert!(taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None)).is_empty());
    // A hide on the same target survives an unlike and is not a like.
    let hide = taste_put_signal(&h, &cookie, "track", "job-t2", "hide", None);
    assert_eq!(hide.status, 200, "{}", hide.body);
    let resp = taste_call(&h, "DELETE", "/v1/me/likes/track/job-t2", &cookie, None);
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(
        taste_row(&h, "tu-1", "track", "job-t2").map(|r| r.signal),
        Some("hide".to_string())
    );
    assert!(taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None)).is_empty());
    let live = taste_items(&taste_call(&h, "GET", "/v1/me/taste", &cookie, None));
    assert_eq!(live.len(), 1);
    assert_eq!(live[0]["signal"], "hide");
    // Liking a hidden target replaces the state (upsert), it does not stack.
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/job-t2", &cookie, None).status,
        200
    );
    assert_eq!(
        taste_row(&h, "tu-1", "track", "job-t2").map(|r| r.signal),
        Some("love".to_string())
    );
}

#[test]
fn taste_like_routes_treat_unknown_and_malformed_targets_as_404() {
    let h = harness();
    let cookie = taste_user(&h, "tn-1");
    taste_library(&h, "tn-1");
    let other = taste_user(&h, "tn-2");
    h.store.catalogue_library.borrow_mut().insert(
        "tn-2".to_string(),
        vec![unmatched_library_row("job-other-private")],
    );
    let too_long = "a".repeat(crate::taste::MAX_TARGET_ID_LEN + 1);
    for target in ["no-such-track", "job-other-private", too_long.as_str()] {
        let path = format!("/v1/me/likes/track/{target}");
        let put = taste_call(&h, "PUT", &path, &cookie, None);
        assert_eq!(put.status, 404, "PUT {target}: {}", put.body);
        assert_eq!(taste_code(&put), "not_found");
        let del = taste_call(&h, "DELETE", &path, &cookie, None);
        assert_eq!(del.status, 404, "DELETE {target}: {}", del.body);
    }
    // The 404 body never distinguishes unknown from someone else's id.
    let unknown = taste_call(&h, "PUT", "/v1/me/likes/track/no-such-track", &cookie, None);
    let foreign = taste_call(
        &h,
        "PUT",
        "/v1/me/likes/track/job-other-private",
        &cookie,
        None,
    );
    assert_eq!(unknown.body, foreign.body);
    assert_eq!(taste_rows_for(&h, "tn-1"), 0);
    // The owner of the foreign job can like it, and it never leaks back.
    assert_eq!(
        taste_call(
            &h,
            "PUT",
            "/v1/me/likes/track/job-other-private",
            &other,
            None
        )
        .status,
        200
    );
    assert!(taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None)).is_empty());
    // Nested or empty tails never reach the handler.
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/", &cookie, None).status,
        404
    );
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/a/b", &cookie, None).status,
        404
    );
}

// --- Import ----------------------------------------------------------------------------------

fn taste_import(h: &Harness, cookie: &str, items: serde_json::Value) -> Response {
    taste_call(
        h,
        "POST",
        "/v1/me/likes/import",
        cookie,
        Some(serde_json::json!({"items": items})),
    )
}

#[test]
fn taste_import_classifies_items_and_retry_converges_to_already() {
    let h = harness();
    let cookie = taste_user(&h, "ti-1");
    taste_library(&h, "ti-1");
    let items = serde_json::json!([
        {"kind": "track", "target_id": TASTE_REC.to_uppercase()},
        {"kind": "track", "target_id": "job-t2"},
        {"kind": "release", "target_id": CAT_RELEASE.to_uppercase()},
        {"kind": "release", "target_id": "not-an-mbid"},
        {"kind": "playlist", "target_id": "job-t1"},
        {"kind": "track", "target_id": ""},
        {"kind": "track", "target_id": "stranger-job"},
        {"kind": "track", "target_id": TASTE_REC},
    ]);
    let resp = taste_import(&h, &cookie, items.clone());
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = taste_json(&resp);
    assert_eq!(body["added"], 3);
    assert_eq!(body["already"], 1, "in-batch duplicate is already");
    let rejected = body["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 4);
    assert_eq!(rejected[0]["target_id"], "not-an-mbid");
    assert_eq!(rejected[0]["reason"], "invalid");
    assert_eq!(rejected[1]["reason"], "invalid");
    assert_eq!(rejected[2]["reason"], "invalid");
    assert_eq!(rejected[3]["target_id"], "stranger-job");
    assert_eq!(rejected[3]["reason"], "not_member");
    // Stored canonical: lowercase MBIDs, verbatim job id, three rows.
    assert_eq!(taste_rows_for(&h, "ti-1"), 3);
    assert!(taste_row(&h, "ti-1", "track", TASTE_REC).is_some());
    assert!(taste_row(&h, "ti-1", "release", CAT_RELEASE).is_some());
    assert!(taste_row(&h, "ti-1", "track", "job-t2").is_some());
    // Retry converges: nothing new, all prior accepted items are `already`.
    let retry = taste_json(&taste_import(&h, &cookie, items));
    assert_eq!(retry["added"], 0);
    assert_eq!(retry["already"], 4);
    assert_eq!(retry["rejected"].as_array().unwrap().len(), 4);
    assert_eq!(taste_rows_for(&h, "ti-1"), 3);
    // Releases are likes too and surface in the liked page.
    let liked = taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None));
    assert_eq!(liked.len(), 3);
    assert!(liked
        .iter()
        .any(|i| i["kind"] == "release" && i["target_id"] == CAT_RELEASE));
}

#[test]
fn taste_import_is_tenant_scoped_and_a_hide_is_upgraded_not_counted_already() {
    let h = harness();
    let a = taste_user(&h, "tt-a");
    let b = taste_user(&h, "tt-b");
    taste_library(&h, "tt-a");
    h.store
        .shared_member_targets
        .borrow_mut()
        .insert(("track".to_string(), "shared-trk".to_string()));
    // B cannot import A's private ledger ids; the shared id is fine.
    let resp = taste_import(
        &h,
        &b,
        serde_json::json!([
            {"kind": "track", "target_id": "job-t1"},
            {"kind": "track", "target_id": TASTE_REC},
            {"kind": "release", "target_id": CAT_RELEASE},
            {"kind": "track", "target_id": "shared-trk"},
        ]),
    );
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = taste_json(&resp);
    assert_eq!(body["added"], 1);
    assert_eq!(body["already"], 0);
    let reasons: Vec<&str> = body["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["not_member", "not_member", "not_member"]);
    assert_eq!(taste_rows_for(&h, "tt-b"), 1);
    assert_eq!(
        taste_rows_for(&h, "tt-a"),
        0,
        "no write lands on another account"
    );
    // A prior non-love signal is converted to a like and counts as added.
    taste_seed_row(&h, "tt-a", "track", "job-t2", "hide", None, 500);
    let resp = taste_import(
        &h,
        &a,
        serde_json::json!([{"kind": "track", "target_id": "job-t2"}]),
    );
    assert_eq!(taste_json(&resp)["added"], 1);
    assert_eq!(
        taste_row(&h, "tt-a", "track", "job-t2").map(|r| r.signal),
        Some("love".to_string())
    );
}

#[test]
fn taste_import_full_length_batches_use_the_bounded_collection_allowance() {
    use crate::worker_entry::{
        collect_body_with_cap, BodyChunkStream, BodyReadError, BodySource, CollectedBody,
    };
    struct OneChunk(Option<Vec<u8>>);
    impl BodyChunkStream for OneChunk {
        fn poll_next_chunk(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<Vec<u8>, BodyReadError>>> {
            std::task::Poll::Ready(self.0.take().map(Ok))
        }
    }
    let h = harness();
    let cookie = taste_user(&h, "import-cap");
    taste_library(&h, "import-cap");
    let long_id = "t".repeat(crate::taste::MAX_TARGET_ID_LEN);
    h.store
        .catalogue_library
        .borrow_mut()
        .get_mut("import-cap")
        .unwrap()
        .push(unmatched_library_row(&long_id));
    let path = "/v1/me/likes/import";
    let cap = max_body_bytes("POST", path);
    assert_eq!(cap, crate::taste::IMPORT_MAX_BODY_BYTES);
    assert_eq!(max_body_bytes("PUT", path), MAX_BODY_BYTES);
    assert_eq!(max_body_bytes("POST", "/v1/me/taste"), MAX_BODY_BYTES);
    for target in [TASTE_REC, long_id.as_str()] {
        let body = serde_json::json!({"items": (0..crate::taste::IMPORT_MAX_ITEMS)
            .map(|_| serde_json::json!({"kind":"track", "target_id": target}))
            .collect::<Vec<_>>()})
        .to_string()
        .into_bytes();
        assert!(body.len() > MAX_BODY_BYTES);
        assert!(body.len() <= cap);
        for declared in [None, Some(body.len())] {
            let collected = block_on(collect_body_with_cap(
                declared,
                BodySource::Stream(OneChunk(Some(body.clone()))),
                cap,
            ));
            let CollectedBody::Complete(bytes) = collected else {
                panic!("legal batch refused by collection gate");
            };
            assert_eq!(bytes, body);
            let req = Request {
                method: "POST".to_string(),
                path: path.to_string(),
                cookie: Some(cookie.clone()),
                origin: Some(ORIGIN.to_string()),
                authorization: None,
                stripe_signature: None,
                body: bytes,
            };
            let resp = block_on(handle(&h.ctx(), &req));
            assert_eq!(resp.status, 200, "{}", resp.body);
            let value = taste_json(&resp);
            assert_eq!(
                value["added"].as_u64().unwrap() + value["already"].as_u64().unwrap(),
                500
            );
            assert!(value["rejected"].as_array().unwrap().is_empty());
        }
    }
    for declared in [None, Some(cap + 1)] {
        let collected = block_on(collect_body_with_cap(
            declared,
            BodySource::Stream(OneChunk(Some(vec![b' '; cap + 1]))),
            cap,
        ));
        assert!(matches!(collected, CollectedBody::Oversized));
    }
    let req = Request {
        method: "POST".to_string(),
        path: path.to_string(),
        cookie: Some(cookie),
        origin: Some(ORIGIN.to_string()),
        authorization: None,
        stripe_signature: None,
        body: vec![b' '; cap + 1],
    };
    assert_eq!(block_on(handle(&h.ctx(), &req)).status, 400);
}

#[test]
fn taste_import_rejects_bad_bodies_and_oversize_batches_without_panicking() {
    let h = harness();
    let cookie = taste_user(&h, "tb-1");
    taste_library(&h, "tb-1");
    // Over the item bound: one 422, nothing written.
    let many_valid: Vec<serde_json::Value> = (0..=crate::taste::IMPORT_MAX_ITEMS)
        .map(|_| serde_json::json!({"kind": "track", "target_id": "job-t1"}))
        .collect();
    let oversize = taste_import(&h, &cookie, serde_json::Value::Array(many_valid));
    assert_eq!(oversize.status, 422, "{}", oversize.body);
    assert_eq!(taste_code(&oversize), "validation_failed");
    assert_eq!(taste_rows_for(&h, "tb-1"), 0);
    // The bound itself is accepted (every item a non-member here).
    let at_bound: Vec<serde_json::Value> = (0..crate::taste::IMPORT_MAX_ITEMS)
        .map(|i| serde_json::json!({"kind": "track", "target_id": format!("x{i}")}))
        .collect();
    let ok = taste_import(&h, &cookie, serde_json::Value::Array(at_bound));
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(
        taste_json(&ok)["rejected"].as_array().unwrap().len(),
        crate::taste::IMPORT_MAX_ITEMS
    );
    // Structural body failures: unknown fields, a missing array, a bare
    // string item, and an owner smuggled into the body.
    for body in [
        serde_json::json!({"items": [], "user_id": "tb-2"}),
        serde_json::json!({"items": [{"kind": "track", "target_id": "job-t1", "user_id": "tb-2"}]}),
        serde_json::json!({"items": ["job-t1"]}),
        serde_json::json!({}),
        serde_json::json!({"items": null}),
    ] {
        let resp = taste_call(
            &h,
            "POST",
            "/v1/me/likes/import",
            &cookie,
            Some(body.clone()),
        );
        assert_eq!(resp.status, 422, "{body}: {}", resp.body);
    }
    // Non-UTF-8 bytes in the body are a plain validation failure.
    let raw = Request {
        method: "POST".to_string(),
        path: "/v1/me/likes/import".to_string(),
        cookie: Some(cookie.clone()),
        origin: Some(ORIGIN.to_string()),
        authorization: None,
        stripe_signature: None,
        body: b"{\"items\":[{\"kind\":\"track\",\"target_id\":\"\xff\xfe\"}]}".to_vec(),
    };
    let resp = block_on(handle(&h.ctx(), &raw));
    assert_eq!(resp.status, 422, "{}", resp.body);
    // Multi-byte ids must be rejected cleanly (200 with a rejected row),
    // never panic, and the echo is bounded to 64 characters.
    let hostile = format!("a{}", "\u{e9}".repeat(40));
    let resp = taste_import(
        &h,
        &cookie,
        serde_json::json!([
            {"kind": "track", "target_id": hostile},
            {"kind": "release", "target_id": "\u{1F3B5}".repeat(30)},
            {"kind": "track", "target_id": "a".repeat(crate::taste::MAX_TARGET_ID_LEN + 1)},
        ]),
    );
    assert_eq!(resp.status, 200, "{}", resp.body);
    let body = taste_json(&resp);
    assert_eq!(body["added"], 0);
    let rejected = body["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 3);
    for (entry, original) in rejected.iter().zip([
        hostile.clone(),
        "\u{1F3B5}".repeat(30),
        "a".repeat(crate::taste::MAX_TARGET_ID_LEN + 1),
    ]) {
        let echo = entry["target_id"].as_str().unwrap();
        assert!(echo.chars().count() <= 64, "{echo}");
        assert!(original.starts_with(echo));
        assert!(matches!(
            entry["reason"].as_str(),
            Some("invalid" | "not_member")
        ));
    }
    assert_eq!(taste_rows_for(&h, "tb-1"), 0);
}

// --- Taste signals ---------------------------------------------------------------------------

#[test]
fn taste_put_signals_canonicalize_targets_and_clear_snooze_on_replace() {
    let h = harness();
    let cookie = taste_user(&h, "ts-1");
    taste_library(&h, "ts-1");
    for signal in ["love", "dislike", "hide", "ban"] {
        let resp = taste_put_signal(&h, &cookie, "track", "job-t1", signal, None);
        assert_eq!(resp.status, 200, "{signal}: {}", resp.body);
        let body = taste_json(&resp);
        assert_eq!(body["signal"], signal);
        assert_eq!(body["kind"], "track");
        assert!(body["until"].is_null());
    }
    // Uppercase MBID in, lowercase echo and storage out.
    let resp = taste_put_signal(
        &h,
        &cookie,
        "release",
        &CAT_RELEASE.to_uppercase(),
        "ban",
        None,
    );
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(taste_json(&resp)["target_id"], CAT_RELEASE);
    assert!(taste_row(&h, "ts-1", "release", CAT_RELEASE).is_some());
    assert!(taste_row(&h, "ts-1", "release", &CAT_RELEASE.to_uppercase()).is_none());
    let resp = taste_put_signal(
        &h,
        &cookie,
        "track",
        &TASTE_REC.to_uppercase(),
        "dislike",
        None,
    );
    assert_eq!(taste_json(&resp)["target_id"], TASTE_REC);
    assert!(taste_row(&h, "ts-1", "track", TASTE_REC).is_some());
    // A snooze then a hide: the hide clears `snooze_until`.
    let snooze = taste_put_signal(&h, &cookie, "track", "job-t2", "snooze", Some(3_600));
    assert_eq!(snooze.status, 200, "{}", snooze.body);
    assert_eq!(taste_json(&snooze)["until"], NOW + 3_600);
    assert_eq!(
        taste_row(&h, "ts-1", "track", "job-t2").and_then(|r| r.snooze_until),
        Some(i64::try_from(NOW + 3_600).unwrap())
    );
    assert_eq!(
        taste_put_signal(&h, &cookie, "track", "job-t2", "hide", None).status,
        200
    );
    let stored = taste_row(&h, "ts-1", "track", "job-t2").unwrap();
    assert_eq!(stored.signal, "hide");
    assert_eq!(stored.snooze_until, None);
    // Replace, not stack: one row per target.
    assert_eq!(taste_rows_for(&h, "ts-1"), 4);
}

#[test]
fn taste_put_validates_signal_kind_and_body_shape() {
    let h = harness();
    let cookie = taste_user(&h, "tv-1");
    taste_library(&h, "tv-1");
    let bad_signal = taste_put_signal(&h, &cookie, "track", "job-t1", "adore", None);
    assert_eq!(bad_signal.status, 422, "{}", bad_signal.body);
    let wrong_case = taste_put_signal(&h, &cookie, "track", "job-t1", "LOVE", None);
    assert_eq!(wrong_case.status, 422);
    // An unknown kind is a body validation failure; a non-MBID release
    // is the member 404.
    let bad_kind = taste_put_signal(&h, &cookie, "playlist", "job-t1", "hide", None);
    assert_eq!(bad_kind.status, 422, "{}", bad_kind.body);
    let bad_release = taste_put_signal(&h, &cookie, "release", "job-t1", "hide", None);
    assert_eq!(bad_release.status, 404, "{}", bad_release.body);
    let empty = taste_put_signal(&h, &cookie, "track", "", "hide", None);
    assert_eq!(empty.status, 404, "{}", empty.body);
    for body in [
        serde_json::json!({"kind": "track", "target_id": "job-t1"}),
        serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "hide", "user_id": "tv-2"}),
        serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "snooze", "snooze_seconds": -5}),
        serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "snooze", "snooze_seconds": "60"}),
    ] {
        let resp = taste_call(&h, "PUT", "/v1/me/taste", &cookie, Some(body.clone()));
        assert_eq!(resp.status, 422, "{body}: {}", resp.body);
    }
    assert_eq!(taste_rows_for(&h, "tv-1"), 0);
}

#[test]
fn taste_snooze_bounds_are_enforced_at_the_edges() {
    let h = harness();
    let cookie = taste_user(&h, "tz-1");
    taste_library(&h, "tz-1");
    let min = crate::taste::SNOOZE_MIN_SECS;
    let max = crate::taste::SNOOZE_MAX_SECS;
    for secs in [
        Some(0),
        Some(1),
        Some(min - 1),
        Some(max + 1),
        Some(u64::MAX),
        None,
    ] {
        let resp = taste_put_signal(&h, &cookie, "track", "job-t1", "snooze", secs);
        assert_eq!(resp.status, 422, "{secs:?}: {}", resp.body);
        assert_eq!(taste_code(&resp), "validation_failed");
    }
    assert_eq!(
        taste_rows_for(&h, "tz-1"),
        0,
        "a refused snooze writes nothing"
    );
    for secs in [min, min + 1, max - 1, max] {
        let resp = taste_put_signal(&h, &cookie, "track", "job-t1", "snooze", Some(secs));
        assert_eq!(resp.status, 200, "{secs}: {}", resp.body);
        assert_eq!(taste_json(&resp)["until"], NOW + secs);
        assert_eq!(
            taste_row(&h, "tz-1", "track", "job-t1").and_then(|r| r.snooze_until),
            Some(i64::try_from(NOW + secs).unwrap())
        );
    }
    // `snooze_seconds` is a snooze-only field, for every other signal.
    for signal in ["love", "dislike", "hide", "ban"] {
        let resp = taste_put_signal(&h, &cookie, "track", "job-t1", signal, Some(min));
        assert_eq!(resp.status, 422, "{signal}: {}", resp.body);
    }
    assert_eq!(
        taste_row(&h, "tz-1", "track", "job-t1").map(|r| r.signal),
        Some("snooze".to_string()),
        "a refused request leaves the stored state unchanged"
    );
}

#[test]
fn taste_list_hides_expired_snoozes_and_shows_live_ones() {
    let h = harness();
    let cookie = taste_user(&h, "te-1");
    let now = i64::try_from(NOW).unwrap();
    taste_seed_row(&h, "te-1", "track", "expired", "snooze", Some(now - 1), 10);
    taste_seed_row(&h, "te-1", "track", "boundary", "snooze", Some(now), 11);
    taste_seed_row(&h, "te-1", "track", "live", "snooze", Some(now + 1), 12);
    taste_seed_row(&h, "te-1", "track", "banned", "ban", None, 13);
    taste_seed_row(&h, "te-1", "track", "loved", "love", None, 14);
    // Another account's rows never appear.
    taste_seed_row(&h, "te-2", "track", "foreign", "ban", None, 15);
    let items = taste_items(&taste_call(&h, "GET", "/v1/me/taste", &cookie, None));
    let ids: Vec<&str> = items
        .iter()
        .map(|i| i["target_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["loved", "banned", "live"],
        "newest first, expiry exclusive"
    );
    let live = items.iter().find(|i| i["target_id"] == "live").unwrap();
    assert_eq!(live["signal"], "snooze");
    assert_eq!(live["until"], now + 1);
    assert_eq!(live["kind"], "track");
    assert!(items[0]["until"].is_null());
    // The likes page only carries `love` rows.
    let likes = taste_items(&taste_call(&h, "GET", "/v1/me/likes", &cookie, None));
    assert_eq!(likes.len(), 1);
    assert_eq!(likes[0]["target_id"], "loved");
}

#[test]
fn taste_delete_clears_idempotently_and_404s_unknown_members() {
    let h = harness();
    let cookie = taste_user(&h, "td-1");
    taste_library(&h, "td-1");
    assert_eq!(
        taste_put_signal(&h, &cookie, "release", CAT_RELEASE, "ban", None).status,
        200
    );
    let del = |kind: &str, target: &str| {
        taste_call(
            &h,
            "DELETE",
            "/v1/me/taste",
            &cookie,
            Some(serde_json::json!({"kind": kind, "target_id": target})),
        )
    };
    let first = del("release", &CAT_RELEASE.to_uppercase());
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(taste_json(&first)["cleared"], true);
    assert_eq!(taste_rows_for(&h, "td-1"), 0);
    let again = del("release", CAT_RELEASE);
    assert_eq!(again.status, 200);
    assert_eq!(taste_json(&again)["cleared"], false);
    // Unknown / malformed / non-member targets are 404, never a silent 200.
    for (kind, target) in [
        ("track", "no-such-track"),
        ("release", "not-an-mbid"),
        ("track", ""),
    ] {
        let resp = del(kind, target);
        assert_eq!(resp.status, 404, "{kind}/{target}: {}", resp.body);
        assert_eq!(taste_code(&resp), "not_found");
    }
    assert_eq!(del("playlist", "job-t1").status, 422);
    let unknown_field = taste_call(
        &h,
        "DELETE",
        "/v1/me/taste",
        &cookie,
        Some(serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "ban"})),
    );
    assert_eq!(unknown_field.status, 422);
    let no_body = taste_call(&h, "DELETE", "/v1/me/taste", &cookie, None);
    assert_eq!(no_body.status, 422);
}

#[test]
fn taste_store_outage_is_opaque_503_and_writes_nothing() {
    let h = harness();
    let cookie = taste_user(&h, "to-1");
    taste_library(&h, "to-1");
    *h.store.fail_catalogue_reads.borrow_mut() = true;
    for path in ["/v1/me/likes", "/v1/me/taste", "/v1/me/history"] {
        let resp = taste_call(&h, "GET", path, &cookie, None);
        assert_eq!(resp.status, 503, "{path}: {}", resp.body);
    }
    // The membership probe failing is 503, not 404 (an outage is not a
    // verdict on the target).
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/job-t1", &cookie, None).status,
        503
    );
    assert_eq!(
        taste_put_signal(&h, &cookie, "track", "job-t1", "hide", None).status,
        503
    );
    assert_eq!(
        taste_import(
            &h,
            &cookie,
            serde_json::json!([{"kind": "track", "target_id": "job-t1"}])
        )
        .status,
        503
    );
    assert_eq!(taste_rows_for(&h, "to-1"), 0);
}

// --- Auth, CSRF and API-key gate -------------------------------------------------------------

fn taste_all_routes() -> Vec<(&'static str, String, Option<serde_json::Value>)> {
    vec![
        ("GET", "/v1/me/likes".to_string(), None),
        ("GET", "/v1/me/taste".to_string(), None),
        ("GET", "/v1/me/history".to_string(), None),
        ("PUT", "/v1/me/likes/track/job-t1".to_string(), None),
        ("DELETE", "/v1/me/likes/track/job-t1".to_string(), None),
        (
            "POST",
            "/v1/me/likes/import".to_string(),
            Some(serde_json::json!({"items": [{"kind": "track", "target_id": "job-t1"}]})),
        ),
        (
            "PUT",
            "/v1/me/taste".to_string(),
            Some(serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "ban"})),
        ),
        (
            "DELETE",
            "/v1/me/taste".to_string(),
            Some(serde_json::json!({"kind": "track", "target_id": "job-t1"})),
        ),
        (
            "PUT",
            "/v1/me/private-session".to_string(),
            Some(serde_json::json!({"session_id": "tab-a", "enabled": true})),
        ),
    ]
}

#[test]
fn taste_routes_require_a_session_and_pin_origin_on_mutations() {
    let h = harness();
    let cookie = taste_user(&h, "tg-1");
    taste_library(&h, "tg-1");
    taste_seed_row(&h, "tg-1", "track", "job-t1", "love", None, 7);
    for (method, path, body) in taste_all_routes() {
        let mutation = method != "GET";
        let text = body.as_ref().map(|b| b.to_string());
        // No cookie: the generic session 401, with and without an Origin.
        for origin in [Some(ORIGIN), None] {
            let resp = h.call_full(method, &path, text.clone(), None, origin);
            assert_eq!(resp.status, 401, "{method} {path} anon: {}", resp.body);
        }
        // A garbage cookie is no session either.
        let resp = h.call_full(
            method,
            &path,
            text.clone(),
            Some("__Host-nntune-session=garbage".to_string()),
            Some(ORIGIN),
        );
        assert_eq!(resp.status, 401, "{method} {path} garbage cookie");
        if mutation {
            // A valid session from the wrong or missing Origin is the CSRF 401.
            for origin in [Some("https://evil.example"), Some(""), None] {
                let resp = h.call_full(method, &path, text.clone(), Some(cookie.clone()), origin);
                assert_eq!(
                    resp.status, 401,
                    "{method} {path} origin {origin:?}: {}",
                    resp.body
                );
            }
        }
    }
    // Nothing the refused calls carried took effect.
    assert_eq!(taste_rows_for(&h, "tg-1"), 1);
    assert_eq!(
        taste_row(&h, "tg-1", "track", "job-t1").map(|r| r.signal),
        Some("love".to_string())
    );
    assert!(h.store.private_sessions.borrow().is_empty());
    // Reads do not demand an Origin (same posture as the other reads).
    let read = h.call_full("GET", "/v1/me/likes", None, Some(cookie), None);
    assert_eq!(read.status, 200, "{}", read.body);
}

#[test]
fn taste_routes_are_not_reachable_with_a_native_rest_api_key() {
    let h = peppered_harness();
    let cookie = taste_user(&h, "tk-1");
    taste_library(&h, "tk-1");
    taste_seed_row(&h, "tk-1", "track", "job-t1", "love", None, 7);
    let mut secrets = Vec::new();
    for scope in ["library:read", "stream:read", "plays:write"] {
        let minted = mint_call(
            &h,
            &cookie,
            serde_json::json!({"scopes": [scope], "ttl_secs": 3600}),
        );
        assert_eq!(minted.status, 201, "{scope}: {}", minted.body);
        secrets.push(mint_secret(&taste_json(&minted)));
    }
    for secret in &secrets {
        for (method, path, body) in taste_all_routes() {
            let req = Request {
                method: method.to_string(),
                path: path.clone(),
                cookie: None,
                origin: Some(ORIGIN.to_string()),
                authorization: Some(format!("Bearer {secret}")),
                stripe_signature: None,
                body: body.map(|b| b.to_string().into_bytes()).unwrap_or_default(),
            };
            let resp = block_on(handle(&h.ctx(), &req));
            assert_eq!(resp.status, 404, "{method} {path}: {}", resp.body);
            assert!(
                !resp.body.contains("job-t1"),
                "{method} {path} leaked a row"
            );
            // Any Authorization header is refused even beside a valid cookie.
            let with_cookie = Request {
                cookie: Some(cookie.clone()),
                ..req
            };
            let resp = block_on(handle(&h.ctx(), &with_cookie));
            assert_eq!(resp.status, 404, "{method} {path} +cookie: {}", resp.body);
        }
    }
    assert_eq!(taste_rows_for(&h, "tk-1"), 1, "no key-driven write landed");
    assert_eq!(
        taste_row(&h, "tk-1", "track", "job-t1").map(|r| r.signal),
        Some("love".to_string())
    );
    assert!(h.store.private_sessions.borrow().is_empty());
}

// --- Account switch and ownership -------------------------------------------------------------

#[test]
fn taste_state_follows_the_session_account_across_a_switch() {
    let h = harness();
    let a = taste_user(&h, "tw-a");
    let b = taste_user(&h, "tw-b");
    taste_library(&h, "tw-a");
    h.store
        .shared_member_targets
        .borrow_mut()
        .insert(("track".to_string(), "shared-trk".to_string()));
    // A likes, hides and marks private; B has no view of any of it.
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/job-t1", &a, None).status,
        200
    );
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/shared-trk", &a, None).status,
        200
    );
    assert_eq!(
        taste_put_signal(&h, &a, "release", CAT_RELEASE, "ban", None).status,
        200
    );
    assert_eq!(taste_private_session(&h, &a, "tab-a", true).status, 200);
    block_on(
        h.store
            .record_play_window(&taste_window("tw-a", "job-t1", "p1", 1_000, 5, NOW - 5)),
    )
    .unwrap();
    assert!(taste_items(&taste_call(&h, "GET", "/v1/me/likes", &b, None)).is_empty());
    assert!(taste_items(&taste_call(&h, "GET", "/v1/me/taste", &b, None)).is_empty());
    assert!(taste_history(&h, &b).is_empty());
    assert_eq!(taste_history(&h, &a).len(), 1);
    // B cannot touch A's ledger ids, and a body-borne owner is refused.
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/job-t1", &b, None).status,
        404
    );
    assert_eq!(
        taste_call(&h, "DELETE", "/v1/me/likes/track/job-t1", &b, None).status,
        404
    );
    assert_eq!(
        taste_put_signal(&h, &b, "release", CAT_RELEASE, "hide", None).status,
        404
    );
    let smuggled = taste_call(
        &h,
        "PUT",
        "/v1/me/taste",
        &b,
        Some(serde_json::json!({
            "kind": "track", "target_id": "shared-trk", "signal": "ban", "user_id": "tw-a",
        })),
    );
    assert_eq!(smuggled.status, 422);
    // The shared target is B's to like; unliking it leaves A's like alone.
    assert_eq!(
        taste_call(&h, "PUT", "/v1/me/likes/track/shared-trk", &b, None).status,
        200
    );
    assert_eq!(
        taste_call(&h, "DELETE", "/v1/me/likes/track/shared-trk", &b, None).status,
        200
    );
    assert!(taste_row(&h, "tw-b", "track", "shared-trk").is_none());
    assert!(taste_row(&h, "tw-a", "track", "shared-trk").is_some());
    // Clearing taste is per account too.
    let cleared = taste_call(
        &h,
        "DELETE",
        "/v1/me/taste",
        &b,
        Some(serde_json::json!({"kind": "release", "target_id": CAT_RELEASE})),
    );
    assert_eq!(cleared.status, 404, "B is not a member of A's release");
    assert_eq!(
        taste_row(&h, "tw-a", "release", CAT_RELEASE).map(|r| r.signal),
        Some("ban".to_string())
    );
    // Switching back: A's own state is intact and B's private sessions
    // are independent of A's identically named one.
    assert_eq!(taste_private_session(&h, &b, "tab-a", true).status, 200);
    assert_eq!(taste_private_session(&h, &b, "tab-a", false).status, 200);
    assert!(h
        .store
        .private_sessions
        .borrow()
        .contains_key(&("tw-a".to_string(), "tab-a".to_string())));
    let a_likes = taste_items(&taste_call(&h, "GET", "/v1/me/likes", &a, None));
    let ids: Vec<&str> = a_likes
        .iter()
        .map(|i| i["target_id"].as_str().unwrap())
        .collect();
    assert_eq!(a_likes.len(), 2, "{ids:?}");
    assert!(ids.contains(&"job-t1") && ids.contains(&"shared-trk"));
}

// --- Private sessions -------------------------------------------------------------------------

#[test]
fn taste_private_session_put_sets_server_ttl_and_validates() {
    let h = harness();
    let cookie = taste_user(&h, "tp-1");
    let on = taste_private_session(&h, &cookie, "tab-a", true);
    assert_eq!(on.status, 200, "{}", on.body);
    let body = taste_json(&on);
    assert_eq!(body["session_id"], "tab-a");
    assert_eq!(
        body["private_until"],
        NOW + crate::taste::PRIVATE_SESSION_TTL_SECS
    );
    assert_eq!(
        h.store.private_sessions.borrow()[&("tp-1".to_string(), "tab-a".to_string())],
        NOW + crate::taste::PRIVATE_SESSION_TTL_SECS
    );
    // The client cannot choose a TTL.
    let custom = taste_call(
        &h,
        "PUT",
        "/v1/me/private-session",
        &cookie,
        Some(
            serde_json::json!({"session_id": "tab-a", "enabled": true, "expires_at": NOW + 10_000_000}),
        ),
    );
    assert_eq!(custom.status, 422);
    // Disable twice: 200, `private_until: null`, row gone.
    for _ in 0..2 {
        let off = taste_private_session(&h, &cookie, "tab-a", false);
        assert_eq!(off.status, 200, "{}", off.body);
        assert!(taste_json(&off)["private_until"].is_null());
    }
    assert!(h.store.private_sessions.borrow().is_empty());
    // Session id bounds mirror the play route (1..=128 bytes).
    assert_eq!(taste_private_session(&h, &cookie, "", true).status, 422);
    assert_eq!(
        taste_private_session(&h, &cookie, &"s".repeat(129), true).status,
        422
    );
    assert_eq!(
        taste_private_session(&h, &cookie, &"s".repeat(128), true).status,
        200
    );
    for body in [
        serde_json::json!({"session_id": "tab-a"}),
        serde_json::json!({"enabled": true}),
        serde_json::json!({"session_id": "tab-a", "enabled": "yes"}),
        serde_json::json!({"session_id": "tab-a", "enabled": true, "user_id": "tp-2"}),
    ] {
        let resp = taste_call(
            &h,
            "PUT",
            "/v1/me/private-session",
            &cookie,
            Some(body.clone()),
        );
        assert_eq!(resp.status, 422, "{body}");
    }
}

#[test]
fn taste_private_marks_follow_session_ownership_and_expiry() {
    let h = play_harness();
    let a = taste_user(&h, "tm-a");
    let b = taste_user(&h, "tm-b");
    seed_entitled(&h, "tm-a", "plus", 100);
    seed_entitled(&h, "tm-b", "plus", 100);
    // B's play under a session id that only A marked private is public.
    assert_eq!(taste_private_session(&h, &a, "tab-x", true).status, 200);
    let b_play = taste_issue_play(&h, &b, "tab-x", "track-9");
    assert!(
        h.store.private_plays.borrow().is_empty(),
        "other user's session id"
    );
    let a_play = taste_issue_play(&h, &a, "tab-x", "track-9");
    let a_key = format!("play:tm-a:track-9:{a_play}");
    assert!(h
        .store
        .private_plays
        .borrow()
        .contains(&("tm-a".to_string(), a_key)));
    // A different session of the same user is not private.
    let other_session = taste_issue_play(&h, &a, "tab-y", "track-9");
    assert_eq!(h.store.private_plays.borrow().len(), 1);
    // An expired session marks nothing: `expires_at > now` is exclusive.
    for (expires, marked) in [(NOW, false), (NOW - 1, false), (NOW + 1, true)] {
        h.store.private_plays.borrow_mut().clear();
        h.store
            .private_sessions
            .borrow_mut()
            .insert(("tm-a".to_string(), "tab-z".to_string()), expires);
        let play = taste_issue_play(&h, &a, "tab-z", "track-9");
        let key = ("tm-a".to_string(), format!("play:tm-a:track-9:{play}"));
        assert_eq!(
            h.store.private_plays.borrow().contains(&key),
            marked,
            "expires_at {expires} vs now {NOW}"
        );
    }
    let _ = (b_play, other_session);
}

#[test]
fn taste_private_continuation_remark_is_idempotent_and_never_unmarks() {
    let h = play_harness();
    let cookie = taste_user(&h, "tc-1");
    seed_entitled(&h, "tc-1", "plus", 100);
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", true).status,
        200
    );
    let play = taste_issue_play(&h, &cookie, "tab-a", "track-9");
    let key = ("tc-1".to_string(), format!("play:tc-1:track-9:{play}"));
    assert!(h.store.private_plays.borrow().contains(&key));
    // Grant refresh for the same play converges on the same single mark.
    let refresh = play_call(
        &h,
        &cookie,
        serde_json::json!({"track_id": "track-9", "session_id": "tab-a", "play_id": play}),
    );
    assert_eq!(refresh.status, 200, "{}", refresh.body);
    assert_eq!(taste_json(&refresh)["play_id"], play.as_str());
    assert_eq!(h.store.private_plays.borrow().len(), 1);
    // Turning private off later never un-marks the earlier play.
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", false).status,
        200
    );
    assert!(h.store.private_plays.borrow().contains(&key));
}

// --- History ----------------------------------------------------------------------------------

#[test]
fn taste_history_lists_real_metered_plays_not_requested_grants() {
    let h = play_harness();
    let a = taste_user(&h, "th-a");
    let b = taste_user(&h, "th-b");
    seed_entitled(&h, "th-a", "plus", 100);
    // A grant alone (requested play) is not history.
    let requested = taste_issue_play(&h, &a, "tab-a", "track-9");
    assert!(
        taste_history(&h, &a).is_empty(),
        "grant issuance is not a play"
    );
    // Metered delivery makes it real, with server-parsed fields.
    let real = taste_window("th-a", "track-9", &requested, 2_000_000, 120, NOW - 60);
    block_on(h.store.record_play_window(&real)).unwrap();
    let older = taste_window("th-a", "track-7", "oldplay", 500_000, 30, NOW - 600);
    block_on(h.store.record_play_window(&older)).unwrap();
    // Zero-byte rows, non-`play:` keys and other accounts' plays are not history.
    let unmetered = taste_window("th-a", "track-8", "nobytes", 0, 0, NOW - 5);
    h.store
        .play_windows
        .borrow_mut()
        .insert(unmetered.idempotency_key.clone(), unmetered);
    let mut foreign_key = taste_window("th-a", "track-8", "weird", 10, 1, NOW - 4);
    foreign_key.idempotency_key = "probe:th-a:track-8:weird".to_string();
    h.store
        .play_windows
        .borrow_mut()
        .insert(foreign_key.idempotency_key.clone(), foreign_key);
    block_on(h.store.record_play_window(&taste_window(
        "th-b",
        "track-1",
        "bplay",
        900,
        9,
        NOW - 1,
    )))
    .unwrap();
    let items = taste_history(&h, &a);
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["play_id"], requested.as_str());
    assert_eq!(items[0]["track_id"], "track-9");
    assert_eq!(items[0]["played_at"], NOW - 60);
    assert_eq!(items[0]["stream_seconds"], 120);
    assert_eq!(items[1]["play_id"], "oldplay", "newest first");
    // B sees only B's own play.
    let b_items = taste_history(&h, &b);
    assert_eq!(b_items.len(), 1);
    assert_eq!(b_items[0]["play_id"], "bplay");
    // Exactly the documented fields; no key, user or byte counts leak.
    let mut keys: Vec<&str> = items[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["play_id", "played_at", "stream_seconds", "track_id"]);
    assert!(!items[0].to_string().contains("th-a"));
}

#[test]
fn taste_history_is_bounded_to_the_recent_tail() {
    let h = harness();
    let cookie = taste_user(&h, "tq-1");
    for i in 0..(crate::taste::HISTORY_MAX_LIST as u64 + 10) {
        let window = taste_window(
            "tq-1",
            "track-1",
            &format!("p{i:03}"),
            100,
            1,
            NOW - 1_000 + i,
        );
        h.store
            .play_windows
            .borrow_mut()
            .insert(window.idempotency_key.clone(), window);
    }
    let items = taste_history(&h, &cookie);
    assert_eq!(items.len(), crate::taste::HISTORY_MAX_LIST as usize);
    assert_eq!(
        items[0]["played_at"],
        NOW - 1_000 + crate::taste::HISTORY_MAX_LIST as u64 + 9
    );
}

#[test]
fn taste_private_plays_are_suppressed_from_history_but_still_billed() {
    let h = play_harness();
    let cookie = taste_user(&h, "tx-1");
    seed_entitled(&h, "tx-1", "plus", 100);
    // A public play issued before private mode starts.
    let public_first = taste_issue_play(&h, &cookie, "tab-a", "track-1");
    // Private mode on: the next play is marked at grant time.
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", true).status,
        200
    );
    let private_play = taste_issue_play(&h, &cookie, "tab-a", "track-2");
    // Both are metered (billing sink), as bytes are delivered.
    block_on(h.store.record_play_window(&taste_window(
        "tx-1",
        "track-1",
        &public_first,
        4_000,
        40,
        NOW - 20,
    )))
    .unwrap();
    block_on(h.store.record_play_window(&taste_window(
        "tx-1",
        "track-2",
        &private_play,
        9_000,
        90,
        NOW - 10,
    )))
    .unwrap();
    // Billing is untouched by privacy: the private play's seconds count
    // toward the monthly meter exactly like the public one.
    let month = crate::ledger::period_month_for_timestamp(NOW);
    assert_eq!(
        h.store.stream_seconds.borrow()[&("tx-1".to_string(), month)],
        130
    );
    assert!(h
        .store
        .play_windows
        .borrow()
        .contains_key(&format!("play:tx-1:track-2:{private_play}")));
    // History: the private play is absent from first use; public one stays.
    let items = taste_history(&h, &cookie);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["play_id"], public_first.as_str());
    assert!(!taste_call(&h, "GET", "/v1/me/history", &cookie, None)
        .body
        .contains(private_play.as_str()));
    // Marks are grant-time only: a public play started before private
    // mode is not back-filled, and a private play stays hidden after
    // the session is switched off.
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", false).status,
        200
    );
    assert_eq!(taste_history(&h, &cookie).len(), 1);
    let after = taste_issue_play(&h, &cookie, "tab-a", "track-3");
    block_on(h.store.record_play_window(&taste_window(
        "tx-1",
        "track-3",
        &after,
        1_000,
        10,
        NOW - 1,
    )))
    .unwrap();
    let ids: Vec<String> = taste_history(&h, &cookie)
        .iter()
        .map(|i| i["play_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, [after, public_first]);
    // Private suppression must not have blocked playback or the quota path.
    assert_eq!(
        taste_call(&h, "GET", "/v1/me/usage", &cookie, None).status,
        200
    );
}

// --- Deleting accounts ------------------------------------------------------------------------

#[test]
fn taste_writes_answer_account_deleting_and_land_nothing() {
    let h = play_harness();
    let cookie = taste_user(&h, "tdl-1");
    taste_library(&h, "tdl-1");
    seed_entitled(&h, "tdl-1", "plus", 100);
    h.store
        .deleting_users
        .borrow_mut()
        .insert("tdl-1".to_string());
    let writes: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        ("PUT", "/v1/me/likes/track/job-t1".to_string(), None),
        (
            "POST",
            "/v1/me/likes/import".to_string(),
            Some(serde_json::json!({"items": [{"kind": "track", "target_id": "job-t1"}]})),
        ),
        (
            "PUT",
            "/v1/me/taste".to_string(),
            Some(serde_json::json!({"kind": "track", "target_id": "job-t1", "signal": "ban"})),
        ),
        (
            "PUT",
            "/v1/me/private-session".to_string(),
            Some(serde_json::json!({"session_id": "tab-a", "enabled": true})),
        ),
    ];
    for (method, path, body) in writes {
        let resp = taste_call(&h, method, &path, &cookie, body);
        assert_eq!(resp.status, 409, "{method} {path}: {}", resp.body);
        assert_eq!(taste_code(&resp), "account_deleting");
    }
    assert_eq!(taste_rows_for(&h, "tdl-1"), 0);
    assert!(h.store.private_sessions.borrow().is_empty());
    // A grant for a deleting account is refused before any private mark.
    let play = play_call(
        &h,
        &cookie,
        serde_json::json!({"track_id": "track-9", "session_id": "tab-a"}),
    );
    assert_eq!(play.status, 409, "{}", play.body);
    assert!(h.store.private_plays.borrow().is_empty());
    // Every preference route, reads and removals included, is 409.
    for (method, path, body) in taste_all_routes() {
        let resp = taste_call(&h, method, &path, &cookie, body);
        assert_eq!(resp.status, 409, "{method} {path}: {}", resp.body);
        assert_eq!(taste_code(&resp), "account_deleting");
    }
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", false).status,
        409
    );
    // The store-level fences mirror the statements (zero rows written).
    assert!(!block_on(h.store.taste_upsert(
        "tdl-1",
        crate::taste::TargetKind::Track,
        "job-t1",
        crate::taste::TasteSignal::Love,
        None,
        NOW,
    ))
    .unwrap());
    assert!(!block_on(h.store.private_play_mark("tdl-1", "play:tdl-1:t:p", NOW)).unwrap());
}

#[test]
fn taste_private_probe_or_mark_error_is_503_with_no_grant_and_no_mark() {
    let h = play_harness();
    let cookie = taste_user(&h, "tf-1");
    seed_entitled(&h, "tf-1", "plus", 100);
    assert_eq!(
        taste_private_session(&h, &cookie, "tab-a", true).status,
        200
    );
    let body = serde_json::json!({"track_id": "track-9", "session_id": "tab-a"});
    *h.store.fail_private_probe.borrow_mut() = true;
    let resp = play_call(&h, &cookie, body.clone());
    assert_eq!(resp.status, 503, "{}", resp.body);
    assert!(!resp.body.contains("grant"), "no grant on a probe error");
    assert!(h.store.private_plays.borrow().is_empty());
    *h.store.fail_private_probe.borrow_mut() = false;
    *h.store.fail_private_mark.borrow_mut() = true;
    let resp = play_call(&h, &cookie, body);
    assert_eq!(resp.status, 503, "{}", resp.body);
    assert!(!resp.body.contains("grant"), "no grant on a mark error");
    assert!(h.store.private_plays.borrow().is_empty());
}

#[test]
fn taste_import_partial_store_failure_is_503_and_retry_converges() {
    let h = harness();
    let cookie = taste_user(&h, "tr-1");
    taste_library(&h, "tr-1");
    let items = serde_json::json!([
        {"kind": "track", "target_id": "job-t1"},
        {"kind": "track", "target_id": "job-t2"},
    ]);
    *h.store.taste_write_budget.borrow_mut() = Some(1);
    let resp = taste_import(&h, &cookie, items.clone());
    assert_eq!(resp.status, 503, "{}", resp.body);
    assert_eq!(taste_rows_for(&h, "tr-1"), 1, "first row preserved");
    assert!(taste_row(&h, "tr-1", "track", "job-t1").is_some());
    *h.store.taste_write_budget.borrow_mut() = None;
    let retry = taste_json(&taste_import(&h, &cookie, items));
    assert_eq!(retry["added"], 1);
    assert_eq!(retry["already"], 1);
    assert_eq!(taste_rows_for(&h, "tr-1"), 2);
}
