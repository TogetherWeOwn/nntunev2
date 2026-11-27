#[tokio::test]
#[ignore = "run by prepare_migrated_stores.sh against its disposable clone"]
async fn owner_scope_and_fences_via_route() {
    let mut db = connect().await;
    let h = harness();
    let req_for = |cookie: Option<String>, origin: Option<&str>, auth: Option<&str>| Request {
        method: String::new(),
        path: String::new(),
        cookie,
        origin: origin.map(str::to_owned),
        authorization: auth.map(str::to_owned),
        stripe_signature: None,
        body: Vec::new(),
    };
    let check = |got: Response, expected: Response, label: &str| {
        assert_eq!(
            (got.status, got.body),
            (expected.status, expected.body),
            "{label}"
        );
    };
    let json_body = |got: Response, status: u16, label: &str| -> Value {
        assert_eq!(got.status, status, "{label}: {}", got.body);
        serde_json::from_str(&got.body).unwrap()
    };

    // Public legacy playlists stay excluded from the private store for both owner and peer.
    let public_id = "plMixedResUnres0000000";
    let seed_track: String = db
        .query_one_scalar(
            "SELECT id::text FROM public.tracks WHERE title = 'new-track'",
            &[],
        )
        .await
        .unwrap();
    for (uid, email) in [
        ("fixture-user-owner", "scope-public-owner@example.com"),
        ("fixture-user-other", "scope-public-other@example.com"),
    ] {
        seed_user(&h, uid, email);
    }
    let pub_owner_cookie = magic_login(&h, "scope-public-owner@example.com");
    let pub_other_cookie = magic_login(&h, "scope-public-other@example.com");
    let ctx = h.ctx();
    for (actor, cookie) in [
        ("public-owner", &pub_owner_cookie),
        ("public-other", &pub_other_cookie),
    ] {
        let req = req_for(Some(cookie.clone()), Some(ORIGIN), None);
        for cmd in [
            Read(public_id),
            Rename(public_id, "Denied Public".into()),
            Append(public_id, seed_track.clone()),
            Delete(public_id),
        ] {
            check(route(&ctx, &req, &mut db, cmd).await, not_found(), actor);
        }
        let listed = json_body(route(&ctx, &req, &mut db, List(None, 50)).await, 200, actor);
        assert!(listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != public_id));
    }

    // Isolated synthetic principals and tracks for create/rename/append/delete and fence checks.
    let (scope_owner, scope_other) = ("fixture-scope-owner", "fixture-scope-other");
    seed_user(&h, scope_owner, "scope-owner@example.com");
    seed_user(&h, scope_other, "scope-other@example.com");
    let owner_cookie = magic_login(&h, "scope-owner@example.com");
    let other_cookie = magic_login(&h, "scope-other@example.com");
    let ctx = h.ctx();
    db.execute(
        "INSERT INTO public.users (id, email, created_at)
         VALUES ($1, 'scope-owner@example.com', 1), ($2, 'scope-other@example.com', 1)",
        &[&scope_owner, &scope_other],
    )
    .await
    .unwrap();
    let lib_sql = "INSERT INTO public.libraries (user_id, name, deleting_at)
                   VALUES ($1, $2, CASE WHEN $3 THEN now() ELSE NULL END) RETURNING id::text";
    let owner_lib: String = db
        .query_one_scalar(lib_sql, &[&scope_owner, &"Owner Active", &false])
        .await
        .unwrap();
    let owner_fenced: String = db
        .query_one_scalar(lib_sql, &[&scope_owner, &"Owner Fenced", &true])
        .await
        .unwrap();
    let other_lib: String = db
        .query_one_scalar(lib_sql, &[&scope_other, &"Other Active", &false])
        .await
        .unwrap();

    let mut tracks = Vec::new();
    for title in [
        "scope-owned",
        "scope-shared",
        "scope-foreign",
        "scope-fenced",
        "scope-unlisted",
    ] {
        let tid: String = db
            .query_one_scalar(
                "INSERT INTO public.tracks (release_id, title, medium_no, track_no)
                 VALUES (gen_random_uuid(), $1, 1, 1) RETURNING id::text",
                &[&title],
            )
            .await
            .unwrap();
        tracks.push(tid);
    }
    let [owned_track, shared_track, foreign_track, fenced_track, unlisted_track]: [String; 5] =
        tracks.try_into().unwrap();
    for (lib, tid) in [
        (&owner_lib, &owned_track),
        (&owner_lib, &shared_track),
        (&owner_fenced, &fenced_track),
        (&other_lib, &shared_track),
        (&other_lib, &foreign_track),
    ] {
        db.execute(
            "INSERT INTO public.library_tracks (library_id, track_id) VALUES ($1::text::uuid, $2::text::uuid)",
            &[lib, tid],
        )
        .await
        .unwrap();
    }

    // Create rejects missing auth, wrong Origin, Authorization header, and invalid titles.
    let owner_req = req_for(Some(owner_cookie.clone()), Some(ORIGIN), None);
    let other_req = req_for(Some(other_cookie), Some(ORIGIN), None);
    let overlong = "a".repeat(crate::library::MAX_NAME_LEN + 1);
    for (req, name, expected, label) in [
        (
            req_for(None, Some(ORIGIN), None),
            "Draft".into(),
            generic_auth(),
            "no cookie",
        ),
        (
            req_for(
                Some(owner_cookie.clone()),
                Some("https://wrong.example"),
                None,
            ),
            "Draft".into(),
            generic_auth(),
            "bad origin",
        ),
        (
            req_for(Some(owner_cookie), Some(ORIGIN), Some("Bearer fixture")),
            "Draft".into(),
            not_found(),
            "auth header",
        ),
        (owner_req.clone(), "   ".into(), invalid_request(), "blank"),
        (
            owner_req.clone(),
            "bad\0title".into(),
            invalid_request(),
            "nul",
        ),
        (owner_req.clone(), overlong, invalid_request(), "overlong"),
    ] {
        check(
            route(&ctx, &req, &mut db, Create(name)).await,
            expected,
            label,
        );
    }

    let created = json_body(
        route(&ctx, &owner_req, &mut db, Create("  Owner Draft  ".into())).await,
        201,
        "create",
    );
    let id = created["id"].as_str().unwrap();
    assert_eq!((id.len(), &created["name"]), (22, &json!("Owner Draft")));
    assert_eq!(
        json_body(
            route(&ctx, &other_req, &mut db, List(None, 50)).await,
            200,
            "other list"
        ),
        json!({"items": [], "next_cursor": null})
    );
    let appended = json_body(
        route(&ctx, &owner_req, &mut db, Append(id, owned_track.clone())).await,
        201,
        "append",
    );
    assert_eq!(appended["position"], 0);
    let entry_id = appended["id"].as_str().unwrap();

    // Cross-account read/rename/append/delete fail closed and leave owner state intact.
    for (label, cmd) in [
        ("other read", Read(id)),
        ("other rename", Rename(id, "Hijacked".into())),
        ("other append shared", Append(id, shared_track.clone())),
        ("other append foreign", Append(id, foreign_track.clone())),
        ("other delete", Delete(id)),
    ] {
        check(
            route(&ctx, &other_req, &mut db, cmd).await,
            not_found(),
            label,
        );
    }

    // Non-Library, foreign-Library, fenced-Library, and unknown track appends fail closed.
    for (label, denied_track) in [
        ("foreign-only library track", foreign_track),
        ("owner fenced library track", fenced_track),
        ("catalogue track without library membership", unlisted_track),
        (
            "missing track uuid",
            "00000000-0000-0000-0000-000000000000".into(),
        ),
    ] {
        check(
            route(&ctx, &owner_req, &mut db, Append(id, denied_track)).await,
            not_found(),
            label,
        );
    }

    assert_eq!(
        json_body(
            route(&ctx, &owner_req, &mut db, Read(id)).await,
            200,
            "owner read"
        ),
        json!({"id": id, "name": "Owner Draft", "legacy_items": [],
               "entries": [{"id": entry_id, "position": 0, "track_id": owned_track}]})
    );
    let ops: i64 = db
        .query_one_scalar(
            "SELECT count(*) FROM public.delete_operations WHERE scope = 'playlist' AND container_id = $1",
            &[&id],
        )
        .await
        .unwrap();
    assert_eq!(ops, 0);

    // Owner rename commits trimmed title; fencing the account blocks every command.
    assert_eq!(
        json_body(
            route(
                &ctx,
                &owner_req,
                &mut db,
                Rename(id, "  Updated Draft  ".into())
            )
            .await,
            200,
            "rename"
        ),
        json!({"id": id, "name": "Updated Draft"})
    );
    assert_eq!(
        db.execute(
            "UPDATE public.users SET deleting_at = now() WHERE id = $1",
            &[&scope_owner]
        )
        .await
        .unwrap(),
        1
    );
    for (label, cmd) in [
        ("fenced create", Create("Blocked".into())),
        ("fenced read", Read(id)),
        ("fenced rename", Rename(id, "Blocked".into())),
        ("fenced append", Append(id, shared_track)),
        ("fenced delete", Delete(id)),
    ] {
        check(
            route(&ctx, &owner_req, &mut db, cmd).await,
            not_found(),
            label,
        );
    }
    assert_eq!(
        json_body(
            route(&ctx, &owner_req, &mut db, List(None, 50)).await,
            200,
            "fenced list"
        ),
        json!({"items": [], "next_cursor": null})
    );
    let state: (String, i64, i64) = db
        .query_one(
            "SELECT p.name,
                    (SELECT count(*) FROM public.playlist_tracks e WHERE e.playlist_id = p.id),
                    (SELECT count(*) FROM public.delete_operations d WHERE d.user_id = $1)
             FROM public.playlists p
             WHERE p.owner_user_id = $1 AND p.public_id = $2 AND p.deleting_at IS NULL",
            &[&scope_owner, &id],
        )
        .await
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .unwrap();
    assert_eq!(state, ("Updated Draft".into(), 1, 0));
}
