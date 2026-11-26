#[tokio::test]
#[ignore = "run by prepare_migrated_stores.sh against its disposable clone"]
async fn owned_library_playlist_reload() {
    let mut db = connect().await;
    db.batch_execute("BEGIN").await.unwrap();

    let mut track_ids = Vec::new();
    for title in [
        "Synthetic Same Title",
        "Synthetic Same Title",
        "Synthetic Anchor",
    ] {
        let track: String = db
            .query_one_scalar(
                "INSERT INTO tracks (release_id, title, medium_no, track_no)
                 VALUES (gen_random_uuid(), $1, 1, 1) RETURNING id::text",
                &[&title],
            )
            .await
            .unwrap();
        track_ids.push(track);
    }
    let [first, second, anchor] = track_ids.as_slice() else {
        unreachable!();
    };

    let h = harness();
    let mut playlists = Vec::new();
    for (playlist_id, owner, handle, member, tracks) in [
        (
            9011_i64,
            "fixture-reload-one",
            "plLibraryReload0000001",
            first,
            [first, second, anchor],
        ),
        (
            9012,
            "fixture-reload-two",
            "plLibraryReload0000002",
            second,
            [anchor, first, second],
        ),
    ] {
        let email = format!("{owner}@example.com");
        seed_user(&h, owner, &email);
        let cookie = magic_login(&h, &email);
        db.execute(
            "WITH account AS (INSERT INTO users (id, email, created_at) VALUES ($1, $2, 1) RETURNING id),
                  library AS (INSERT INTO libraries (user_id, name) SELECT id, 'Synthetic Library' FROM account RETURNING id)
             INSERT INTO library_tracks (library_id, track_id)
             SELECT id, t FROM library CROSS JOIN unnest(ARRAY[$3::text::uuid, $4::text::uuid]) AS t",
            &[&owner, &email, &member, &anchor],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO playlists (id, owner_user_id, public_id, name, created_at)
             VALUES ($1, $2, $3, 'Synthetic Order', 1)",
            &[&playlist_id, &owner, &handle],
        )
        .await
        .unwrap();

        // Seed a historical occurrence of the other Library's track at position 1.
        // Store the rows in reverse order to exercise ordering by position.
        let mut entries = Vec::new();
        for (position, track) in tracks.into_iter().enumerate().rev() {
            let position = position as i32;
            let entry: String = db
                .query_one_scalar(
                    "INSERT INTO playlist_tracks (playlist_id, track_id, position)
                     VALUES ($1, $2::text::uuid, $3) RETURNING id::text",
                    &[&playlist_id, &track, &position],
                )
                .await
                .unwrap();
            entries.push(if position == 1 {
                json!({"id": entry, "position": position, "dormant": true})
            } else {
                json!({"id": entry, "position": position, "track_id": track})
            });
        }
        entries.reverse();
        let detail = json!({"id": handle, "name": "Synthetic Order", "legacy_items": [], "entries": entries});
        playlists.push((cookie, handle, detail));
    }

    db.batch_execute("SET TRANSACTION READ ONLY").await.unwrap();
    let ctx = h.ctx();
    // Reload the first playlist with a fresh request after reading the second owner's playlist.
    for (actor, target) in [(0, 0), (1, 1), (0, 1), (1, 0), (0, 0)] {
        let (cookie, _, _) = &playlists[actor];
        let (_, handle, detail) = &playlists[target];
        let req = Request {
            method: "GET".into(),
            path: format!("/v1/playlists/{handle}"),
            cookie: Some(cookie.clone()),
            origin: None,
            authorization: None,
            stripe_signature: None,
            body: vec![],
        };
        let response = route(&ctx, &req, &mut db, Read(handle)).await;
        let expected = if actor == target {
            json_response(200, detail.clone())
        } else {
            not_found()
        };
        assert_eq!(
            (response.status, response.body),
            (expected.status, expected.body),
            "fixture session {actor} reading {handle}"
        );
    }
    db.batch_execute("ROLLBACK").await.unwrap();
}
