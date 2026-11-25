#[tokio::test]
#[ignore = "run by prepare_migrated_stores.sh against its disposable clone"]
async fn repeated_entries_read() {
    let mut db = connect().await;
    let owner = "fixture-user-owner";
    let handle = "plRepeatedRead00000000";
    let playlist_id = 9010_i64;
    let first = "00000000-0000-0000-0000-000000000010";
    let middle = "00000000-0000-0000-0000-000000000011";
    let last = "00000000-0000-0000-0000-000000000012";
    let legacy_items = json!(["fixture-repeat", "fixture-other", "fixture-repeat"]);
    let track_by_title = "SELECT id::text FROM tracks WHERE title = $1";
    let repeated: String = db
        .query_one_scalar(track_by_title, &[&"trkRpRepeat00000000000"])
        .await
        .unwrap();
    let different: String = db
        .query_one_scalar(track_by_title, &[&"new-track"])
        .await
        .unwrap();

    db.batch_execute("BEGIN").await.unwrap();
    db.execute(
        "INSERT INTO playlists (id, owner_user_id, public_id, name, items, is_public, created_at)
             VALUES ($1, $2, $3, 'Fixture Repeated Read', $4::text::jsonb, false, 1)",
        &[&playlist_id, &owner, &handle, &legacy_items.to_string()],
    )
    .await
    .unwrap();
    // Model backfilled occurrences. The append command rejects duplicate tracks.
    db.execute(
        "INSERT INTO playlist_tracks (playlist_id, id, track_id, position, is_legacy)
         VALUES ($1, $2::text::uuid, $3::text::uuid, 2, true),
                ($1, $4::text::uuid, $3::text::uuid, 0, true),
                ($1, $5::text::uuid, $6::text::uuid, 1, true)",
        &[&playlist_id, &last, &repeated, &first, &middle, &different],
    )
    .await
    .unwrap();

    let snapshot =
        "SELECT jsonb_build_array(to_jsonb(p), (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.position)
                         FROM playlist_tracks e WHERE e.playlist_id = p.id))::text
                    FROM playlists p WHERE p.id = $1";
    // The seed stays visible in this transaction, but the read cannot write.
    db.batch_execute("SET TRANSACTION READ ONLY").await.unwrap();
    let before: String = db
        .query_one_scalar(snapshot, &[&playlist_id])
        .await
        .unwrap();
    let detail = run(&mut db, owner, Read(handle)).await.unwrap();
    let after: String = db
        .query_one_scalar(snapshot, &[&playlist_id])
        .await
        .unwrap();
    db.batch_execute("ROLLBACK").await.unwrap();

    assert_eq!(before, after);
    assert_eq!(
        detail,
        json!({
            "id": handle,
            "name": "Fixture Repeated Read",
            "legacy_items": legacy_items,
            "entries": [
                {"id": first, "position": 0, "track_id": repeated},
                {"id": middle, "position": 1, "track_id": different},
                {"id": last, "position": 2, "track_id": repeated}
            ]
        })
    );
}
